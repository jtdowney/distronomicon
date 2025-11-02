use std::{
    fs::{self, File},
    io, thread,
    time::{Duration, Instant},
};

use camino::{Utf8Path, Utf8PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum LockError {
    #[error("Lock is held by another process (timed out after {timeout_secs}s)")]
    Busy { timeout_secs: u64 },
    #[error("IO error: {0}")]
    Io(#[from] io::Error),
}

pub type Result<T> = std::result::Result<T, LockError>;

/// RAII guard for an exclusive file lock.
///
/// The lock is automatically released and the lock file is removed when the guard is dropped.
pub struct LockGuard {
    file: File,
    path: Utf8PathBuf,
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        let _ = self.file.unlock();
        let _ = fs::remove_file(&self.path);
    }
}

fn lock_path(app: &str, lock_root: Option<&Utf8Path>) -> Utf8PathBuf {
    match lock_root {
        Some(root) => root.join(app).join("lock"),
        None => Utf8PathBuf::from(format!("/var/lock/distronomicon-{app}.lock")),
    }
}

/// Acquires an exclusive lock for the given application with retry logic.
///
/// Creates or opens a lock file at `<lock_root>/distronomicon-<app>.lock` (or
/// `/var/lock/distronomicon-<app>.lock` if `lock_root` is `None`) and attempts
/// to acquire an exclusive lock. Uses non-blocking lock attempts with exponential
/// backoff retry logic.
///
/// If the lock is already held, this function will retry with exponential backoff
/// (100ms → 200ms → 400ms → 800ms → 1s) until the timeout is reached.
///
/// It's recommended to pass the state directory as `lock_root` to avoid permission
/// issues with system directories like `/var/lock`.
///
/// The lock is automatically released when the returned `LockGuard` is dropped.
///
/// # Arguments
///
/// * `app` - The application name
/// * `lock_root` - Optional directory for the lock file
/// * `timeout` - Maximum time to wait for the lock (default: 30 seconds)
///
/// # Errors
///
/// Returns an error if:
/// - `LockError::Busy` - The lock is held and timeout was reached
/// - `LockError::Io` - The parent directory cannot be created, the lock file
///   cannot be created or opened, or other I/O errors occur
pub fn acquire(
    app: &str,
    lock_root: Option<&Utf8Path>,
    timeout: Option<Duration>,
) -> Result<LockGuard> {
    let timeout = timeout.unwrap_or(Duration::from_secs(30));
    let lock_path = lock_path(app, lock_root);

    if let Some(parent) = lock_path.parent() {
        fs::create_dir_all(parent)?;
    }

    let file = File::create(&lock_path)?;
    let start = Instant::now();
    let mut delay = Duration::from_millis(100);
    let max_delay = Duration::from_secs(1);

    loop {
        if let Ok(()) = file.try_lock() {
            return Ok(LockGuard {
                file,
                path: lock_path.clone(),
            });
        }

        if start.elapsed() >= timeout {
            return Err(LockError::Busy {
                timeout_secs: timeout.as_secs(),
            });
        }

        thread::sleep(delay);
        delay = (delay * 2).min(max_delay);
    }
}

/// Forcibly removes the lock file for the given application.
///
/// This function removes the lock file without checking if a process is holding
/// the lock. Use with caution as it may disrupt a running update process.
///
/// Returns `Ok(())` if the lock file is removed or doesn't exist.
///
/// # Errors
///
/// Returns `LockError::Io` if the file exists but cannot be removed.
pub fn unlock(app: &str, lock_root: Option<&Utf8Path>) -> Result<()> {
    let lock_path = lock_path(app, lock_root);

    match fs::remove_file(&lock_path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(LockError::Io(e)),
    }
}

#[cfg(test)]
mod tests {
    use std::{sync::mpsc, thread, time::Duration};

    use camino_tempfile::tempdir;

    use super::*;

    #[test]
    fn test_acquire_lock_once() {
        let temp_dir = tempdir().unwrap();
        let lock_root = temp_dir.path();

        let guard = acquire("testapp", Some(lock_root), None).unwrap();
        drop(guard);
    }

    #[test]
    fn test_acquire_with_retry() {
        let temp_dir = tempdir().unwrap();
        let lock_root = temp_dir.path().to_path_buf();

        let guard = acquire("testapp", Some(&lock_root), None).unwrap();

        let (tx, rx) = mpsc::channel();

        let handle = thread::spawn(move || {
            tx.send("attempting").unwrap();
            let _guard2 =
                acquire("testapp", Some(&lock_root), Some(Duration::from_secs(5))).unwrap();
            tx.send("acquired").unwrap();
        });

        assert_eq!(rx.recv().unwrap(), "attempting");

        thread::sleep(Duration::from_millis(200));
        assert!(rx.try_recv().is_err());

        drop(guard);

        let result = rx.recv_timeout(Duration::from_secs(2));
        assert_eq!(result.unwrap(), "acquired");

        handle.join().unwrap();
    }

    #[test]
    fn test_acquire_timeout() {
        let temp_dir = tempdir().unwrap();
        let lock_root = temp_dir.path();

        let _guard = acquire("testapp", Some(lock_root), None).unwrap();

        let result = acquire("testapp", Some(lock_root), Some(Duration::from_millis(500)));

        assert!(result.is_err());
        if let Err(LockError::Busy { timeout_secs }) = result {
            assert_eq!(timeout_secs, 0);
        } else {
            panic!("Expected LockError::Busy");
        }
    }

    #[test]
    fn test_acquire_release_acquire() {
        let temp_dir = tempdir().unwrap();
        let lock_root = temp_dir.path();

        let guard1 = acquire("testapp", Some(lock_root), None).unwrap();
        drop(guard1);

        let guard2 = acquire("testapp", Some(lock_root), None).unwrap();
        drop(guard2);
    }

    #[test]
    fn test_unlock_removes_lock_file() {
        let temp_dir = tempdir().unwrap();
        let lock_root = temp_dir.path();

        let guard = acquire("testapp", Some(lock_root), None).unwrap();

        let lock_file = lock_root.join("testapp").join("lock");
        assert!(lock_file.exists());

        drop(guard);

        unlock("testapp", Some(lock_root)).unwrap();
        assert!(!lock_file.exists());
    }

    #[test]
    fn test_unlock_nonexistent_succeeds() {
        let temp_dir = tempdir().unwrap();
        let lock_root = temp_dir.path();

        let result = unlock("testapp", Some(lock_root));
        assert!(result.is_ok());
    }

    #[test]
    fn test_acquire_after_forced_unlock() {
        let temp_dir = tempdir().unwrap();
        let lock_root = temp_dir.path();

        let guard1 = acquire("testapp", Some(lock_root), None).unwrap();

        unlock("testapp", Some(lock_root)).unwrap();

        let guard2 = acquire("testapp", Some(lock_root), None).unwrap();

        drop(guard1);
        drop(guard2);
    }

    #[test]
    fn test_lock_file_cleaned_up_on_drop() {
        let temp_dir = tempdir().unwrap();
        let lock_root = temp_dir.path();

        let guard = acquire("testapp", Some(lock_root), None).unwrap();

        let lock_file = lock_root.join("testapp").join("lock");
        assert!(lock_file.exists());

        drop(guard);

        assert!(!lock_file.exists());
    }
}

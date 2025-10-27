use std::{
    fs::{self, File},
    io,
};

use camino::{Utf8Path, Utf8PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum LockError {
    #[error("IO error: {0}")]
    Io(#[from] io::Error),
}

pub type Result<T> = std::result::Result<T, LockError>;

/// RAII guard for an exclusive file lock.
///
/// The lock is automatically released when the guard is dropped.
pub struct LockGuard {
    file: File,
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

/// Acquires an exclusive lock for the given application.
///
/// Creates or opens a lock file at `<lock_root>/distronomicon-<app>.lock` (or
/// `/var/lock/distronomicon-<app>.lock` if `lock_root` is `None`) and acquires
/// an exclusive lock on it. This function blocks until the lock becomes available.
///
/// The lock is automatically released when the returned `LockGuard` is dropped.
///
/// # Errors
///
/// Returns an error if:
/// - The parent directory cannot be created
/// - The lock file cannot be created or opened
/// - The lock operation fails
///
/// # Panics
///
/// This function panics if all lock paths fail to acquire and no error was recorded,
/// which should not occur in practice given the current implementation.
pub fn acquire(app: &str, lock_root: Option<&Utf8Path>) -> Result<LockGuard> {
    let lock_paths: Vec<Utf8PathBuf> = match lock_root {
        Some(root) => vec![root.join(format!("distronomicon-{app}.lock"))],
        None => {
            vec![
                Utf8PathBuf::from(format!("/var/lock/distronomicon-{app}.lock")),
                Utf8PathBuf::from(format!("/tmp/distronomicon-{app}.lock")),
            ]
        }
    };

    let mut last_error = None;
    for lock_path in lock_paths {
        if let Some(parent) = lock_path.parent()
            && let Err(e) = fs::create_dir_all(parent)
        {
            last_error = Some(e);
            continue;
        }

        match File::create(&lock_path) {
            Ok(file) => match file.lock() {
                Ok(()) => return Ok(LockGuard { file }),
                Err(e) => {
                    last_error = Some(e);
                }
            },
            Err(e) => {
                last_error = Some(e);
            }
        }
    }

    Err(last_error.unwrap().into())
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

        let guard = acquire("testapp", Some(lock_root)).unwrap();
        drop(guard);
    }

    #[test]
    fn test_concurrent_lock_blocks() {
        let temp_dir = tempdir().unwrap();
        let lock_root = temp_dir.path().to_path_buf();

        let guard = acquire("testapp", Some(&lock_root)).unwrap();

        let (tx, rx) = mpsc::channel();
        let lock_root_clone = lock_root.clone();

        let handle = thread::spawn(move || {
            tx.send("attempting").unwrap();
            let _guard2 = acquire("testapp", Some(&lock_root_clone)).unwrap();
            tx.send("acquired").unwrap();
        });

        assert_eq!(rx.recv().unwrap(), "attempting");

        thread::sleep(Duration::from_millis(100));
        assert!(rx.try_recv().is_err(), "second thread should be blocked");

        drop(guard);

        let result = rx.recv_timeout(Duration::from_secs(1));
        assert_eq!(
            result.unwrap(),
            "acquired",
            "second thread should acquire after first drops"
        );

        handle.join().unwrap();
    }

    #[test]
    fn test_acquire_release_acquire() {
        let temp_dir = tempdir().unwrap();
        let lock_root = temp_dir.path();

        let guard1 = acquire("testapp", Some(lock_root)).unwrap();
        drop(guard1);

        let guard2 = acquire("testapp", Some(lock_root)).unwrap();
        drop(guard2);
    }
}

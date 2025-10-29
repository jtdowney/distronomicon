use std::fs::{self, File};

use camino::{Utf8Path, Utf8PathBuf};
use camino_tempfile::Builder;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum FsOpsError {
    #[error("release already exists: {0}")]
    AlreadyExists(String),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, FsOpsError>;

/// Creates a unique staging directory under `<root>/<app>/staging/<tag>.<random>`.
///
/// The staging parent directory is created if it doesn't exist. The returned path
/// includes a random suffix to allow multiple concurrent staging operations for the
/// same tag.
///
/// # Errors
///
/// Returns `FsOpsError::Io` if:
/// - The staging parent directory cannot be created
/// - The temporary directory cannot be created
pub fn make_staging(root: impl AsRef<Utf8Path>, app: &str, tag: &str) -> Result<Utf8PathBuf> {
    let staging_parent = root.as_ref().join(app).join("staging");
    fs::create_dir_all(&staging_parent)?;

    let temp_dir = Builder::new()
        .prefix(&format!("{tag}."))
        .tempdir_in(&staging_parent)?;

    Ok(temp_dir.keep())
}

/// Atomically moves a directory from staging to releases, fsyncing the parent.
///
/// Moves `src_dir` to `<releases_dir>/<tag>` using an atomic rename operation.
/// After the move, the releases parent directory is fsynced to ensure durability.
///
/// # Errors
///
/// Returns `FsOpsError::AlreadyExists` if the target path already exists.
///
/// Returns `FsOpsError::Io` if:
/// - The rename operation fails
/// - The parent directory cannot be opened or synced
pub fn atomic_move(
    src_dir: impl AsRef<Utf8Path>,
    releases_dir: impl AsRef<Utf8Path>,
    tag: &str,
) -> Result<Utf8PathBuf> {
    let target = releases_dir.as_ref().join(tag);

    if target.exists() {
        return Err(FsOpsError::AlreadyExists(target.to_string()));
    }

    fs::rename(src_dir.as_ref(), &target)?;

    let parent = File::open(releases_dir.as_ref())?;
    parent.sync_all()?;

    Ok(target)
}

#[cfg(test)]
mod tests {
    use assert_matches::assert_matches;
    use camino_tempfile::tempdir;
    use camino_tempfile_ext::prelude::*;

    use super::*;

    #[test]
    fn make_staging_creates_correct_path_format() {
        let root = tempdir().unwrap();
        let app = "myapp";
        let tag = "v1.2.3";

        let staging_path = make_staging(root.path(), app, tag).unwrap();

        let expected_prefix = root
            .path()
            .join(app)
            .join("staging")
            .join(format!("{tag}."));
        assert!(
            staging_path.as_str().starts_with(expected_prefix.as_str()),
            "staging path {staging_path} should start with {expected_prefix}"
        );
    }

    #[test]
    fn make_staging_creates_parent_directory() {
        let root = tempdir().unwrap();
        let app = "myapp";
        let tag = "v1.2.3";

        let _staging_path = make_staging(root.path(), app, tag).unwrap();

        let staging_parent = root.path().join(app).join("staging");
        assert!(
            staging_parent.exists(),
            "staging parent directory should exist"
        );
        assert!(
            staging_parent.is_dir(),
            "staging parent should be a directory"
        );
    }

    #[test]
    fn make_staging_returns_existing_writable_path() {
        let root = tempdir().unwrap();
        let app = "myapp";
        let tag = "v1.2.3";

        let staging_path = make_staging(root.path(), app, tag).unwrap();

        assert!(staging_path.exists(), "staging path should exist");
        assert!(staging_path.is_dir(), "staging path should be a directory");

        let test_file = staging_path.join("test.txt");
        fs::write(&test_file, "test").unwrap();
        assert!(
            test_file.exists(),
            "should be able to write to staging directory"
        );
    }

    #[test]
    fn make_staging_creates_unique_paths() {
        let root = tempdir().unwrap();
        let app = "myapp";
        let tag = "v1.2.3";

        let path1 = make_staging(root.path(), app, tag).unwrap();
        let path2 = make_staging(root.path(), app, tag).unwrap();

        assert_ne!(path1, path2, "multiple calls should create unique paths");
    }

    #[test]
    fn atomic_move_succeeds() {
        let root = tempdir().unwrap();
        let tag = "v1.2.3";

        let src_dir = root.child("staging").child(tag);
        src_dir.create_dir_all().unwrap();
        src_dir.child("file.txt").write_str("content").unwrap();

        let releases_dir = root.child("releases");
        releases_dir.create_dir_all().unwrap();

        let result = atomic_move(&src_dir, &releases_dir, tag).unwrap();

        assert_eq!(result, releases_dir.join(tag));
        assert!(result.exists(), "destination should exist");
        assert!(!src_dir.exists(), "source should be moved");
        assert_eq!(
            fs::read_to_string(result.join("file.txt")).unwrap(),
            "content"
        );
    }

    #[test]
    fn atomic_move_returns_correct_path() {
        let root = tempdir().unwrap();
        let tag = "v1.2.3";

        let src_dir = root.child("staging").child(tag);
        src_dir.create_dir_all().unwrap();

        let releases_dir = root.child("releases");
        releases_dir.create_dir_all().unwrap();

        let result = atomic_move(&src_dir, &releases_dir, tag).unwrap();

        assert_eq!(result, releases_dir.join(tag));
    }

    #[test]
    fn atomic_move_fails_when_target_exists() {
        let root = tempdir().unwrap();
        let tag = "v1.2.3";

        let src_dir = root.child("staging").child(tag);
        src_dir.create_dir_all().unwrap();

        let releases_dir = root.child("releases");
        let target_dir = releases_dir.child(tag);
        target_dir.create_dir_all().unwrap();

        let result = atomic_move(&src_dir, &releases_dir, tag);

        assert_matches!(result, Err(FsOpsError::AlreadyExists(_)));
    }

    #[test]
    fn atomic_move_fsyncs_parent() {
        let root = tempdir().unwrap();
        let tag = "v1.2.3";

        let src_dir = root.child("staging").child(tag);
        src_dir.create_dir_all().unwrap();

        let releases_dir = root.child("releases");
        releases_dir.create_dir_all().unwrap();

        let result = atomic_move(&src_dir, &releases_dir, tag);

        assert!(
            result.is_ok(),
            "fsync should not cause errors in normal operation"
        );
    }
}

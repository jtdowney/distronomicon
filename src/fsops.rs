use std::{
    collections::HashMap,
    fs::{self, File},
    io::{self, ErrorKind},
    os::unix::fs::PermissionsExt,
};

use camino::{Utf8Path, Utf8PathBuf};
use camino_tempfile::Builder;
use rustix::fs::{CWD, RenameFlags, renameat_with};
use thiserror::Error;
use tracing::warn;

#[derive(Debug, Error)]
pub enum FsOpsError {
    #[error("release already exists: {0}")]
    AlreadyExists(String),
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
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
/// Moves `src_dir` to `<releases_dir>/<tag>` using `renameat_with` with `RENAME_NOREPLACE`
/// to guarantee race-free atomicity. If the target already exists, the operation fails
/// immediately without overwriting. After the move, the releases parent directory is
/// fsynced to ensure durability.
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

    renameat_with(
        CWD,
        src_dir.as_ref().as_std_path(),
        CWD,
        target.as_std_path(),
        RenameFlags::NOREPLACE,
    )
    .map_err(|e| {
        let io_err: io::Error = e.into();
        if io_err.kind() == ErrorKind::AlreadyExists {
            FsOpsError::AlreadyExists(target.to_string())
        } else {
            FsOpsError::Io(io_err)
        }
    })?;

    let parent = File::open(releases_dir.as_ref())?;
    parent.sync_all()?;

    Ok(target)
}

/// Discovers all executable files within a directory tree.
///
/// Recursively walks the directory and returns paths (relative to `dir`) of all files
/// with the executable permission bit set on Unix systems. Non-executable files and
/// permission errors are silently skipped.
///
/// # Errors
///
/// Returns `FsOpsError::Io` if the root directory cannot be read or accessed.
pub fn discover_executables(dir: impl AsRef<Utf8Path>) -> Result<Vec<Utf8PathBuf>> {
    fn walk(base: &Utf8Path, current: &Utf8Path) -> io::Result<Vec<Utf8PathBuf>> {
        let entries = fs::read_dir(current)?
            .filter_map(std::result::Result::ok)
            .filter_map(|entry| {
                let path = Utf8PathBuf::try_from(entry.path()).ok()?;
                let metadata = entry.metadata().ok()?;
                Some((path, metadata))
            });

        let mut executables = Vec::new();

        for (path, metadata) in entries {
            if metadata.is_dir() {
                if let Ok(nested) = walk(base, &path) {
                    executables.extend(nested);
                }
            } else if metadata.is_file() {
                let mode = metadata.permissions().mode();
                if mode & 0o111 != 0
                    && let Ok(rel_path) = path.strip_prefix(base)
                {
                    executables.push(rel_path.to_path_buf());
                }
            }
        }

        Ok(executables)
    }

    let base = dir.as_ref();
    walk(base, base).map_err(Into::into)
}

/// Creates symlinks in `bin_dir` for all executables found in `release_dir`.
///
/// Discovers all executables in `release_dir` recursively and creates flattened symlinks
/// in `bin_dir` that point to `../releases/<tag>/<relative_path>`. The tag is extracted
/// from the last component of `release_dir`. Nested executables are flattened to the bin
/// root using only their filename. Uses atomic temp+rename pattern for each symlink to
/// ensure no partial state is visible.
///
/// If multiple executables share the same filename (e.g., `tools/cli` and `bin/cli`),
/// a warning is logged and the last executable processed will win. The warning includes
/// all conflicting paths for debugging.
///
/// # Errors
///
/// Returns `FsOpsError::Io` if:
/// - Executables cannot be discovered
/// - The tag cannot be extracted from `release_dir`
/// - Symlinks cannot be created or renamed
/// - The bin directory cannot be synced
pub fn link_binaries(
    release_dir: impl AsRef<Utf8Path>,
    bin_dir: impl AsRef<Utf8Path>,
) -> Result<()> {
    let release_dir = release_dir.as_ref();
    let bin_dir = bin_dir.as_ref();

    let tag = release_dir
        .file_name()
        .ok_or_else(|| io::Error::new(ErrorKind::InvalidInput, "release_dir has no filename"))?;

    let executables = discover_executables(release_dir)?;

    let collision_map = executables
        .iter()
        .filter_map(|path| path.file_name().map(|name| (name, path)))
        .fold(HashMap::new(), |mut map, (name, path)| {
            map.entry(name).or_insert_with(Vec::new).push(path);
            map
        });

    collision_map
        .iter()
        .filter(|(_, paths)| paths.len() > 1)
        .for_each(|(filename, paths)| {
            warn!(
                "duplicate filename \"{}\": {:?}, last will win",
                filename, paths
            );
        });

    for rel_path in executables {
        let filename = rel_path
            .file_name()
            .ok_or_else(|| io::Error::new(ErrorKind::InvalidInput, "executable has no filename"))?;

        let target = Utf8PathBuf::from("../releases").join(tag).join(&rel_path);
        let temp_link = bin_dir.join(format!("{filename}.tmp"));
        let final_link = bin_dir.join(filename);

        std::os::unix::fs::symlink(&target, &temp_link)?;
        fs::rename(&temp_link, &final_link)?;
    }

    let bin_file = File::open(bin_dir)?;
    bin_file.sync_all()?;

    Ok(())
}

/// Prunes old releases from the releases directory, keeping only the most recent ones.
///
/// Sorts release directories by modification time (newest first) and deletes releases
/// beyond the `retain` count. Always preserves `current_tag` regardless of its age.
///
/// # Arguments
///
/// * `releases_dir` - Path to the releases directory containing versioned subdirectories
/// * `current_tag` - The currently active release tag (will never be deleted)
/// * `retain` - Number of recent releases to keep (excluding current if it's old)
///
/// # Returns
///
/// A vector of deleted release tag names, or an error if deletion fails.
///
/// # Errors
///
/// Returns `FsOpsError::Io` if:
/// - The releases directory cannot be read
/// - Directory metadata cannot be accessed
/// - A release directory cannot be deleted
pub fn prune_old_releases(
    releases_dir: impl AsRef<Utf8Path>,
    current_tag: &str,
    retain: usize,
) -> Result<Vec<String>> {
    let releases_dir = releases_dir.as_ref();

    if !releases_dir.exists() {
        return Ok(Vec::new());
    }

    let entries = fs::read_dir(releases_dir)?
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let path = Utf8PathBuf::try_from(entry.path()).ok()?;

            if !path.is_dir() {
                return None;
            }

            let tag = path.file_name()?.to_string();
            let metadata = entry.metadata().ok()?;
            let modified = metadata.modified().ok()?;

            Some((tag, modified))
        })
        .collect::<Vec<_>>();

    let mut sorted_entries = entries;
    sorted_entries.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| b.0.cmp(&a.0)));

    let to_delete = sorted_entries
        .iter()
        .skip(retain)
        .map(|(tag, _)| tag.clone())
        .filter(|tag| tag != current_tag)
        .collect::<Vec<_>>();

    let mut deleted = Vec::new();

    for tag in to_delete {
        let release_path = releases_dir.join(&tag);
        match fs::remove_dir_all(&release_path) {
            Ok(()) => {
                tracing::info!("pruned old release: {}", tag);
                deleted.push(tag);
            }
            Err(e) => {
                tracing::warn!("failed to prune release {}: {}", tag, e);
            }
        }
    }

    Ok(deleted)
}

#[cfg(test)]
mod tests {
    use std::{thread, time::Duration};

    use assert_matches::assert_matches;
    use camino_tempfile::tempdir;
    use camino_tempfile_ext::prelude::*;

    use super::*;

    fn create_executable(path: impl AsRef<Utf8Path>, content: &str) {
        let path = path.as_ref();
        fs::write(path, content).unwrap();
        let mut perms = fs::metadata(path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(path, perms).unwrap();
    }

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
    fn atomic_move_succeeds_with_fsync() {
        let root = tempdir().unwrap();
        let tag = "v1.2.3";

        let src_dir = root.child("staging").child(tag);
        src_dir.create_dir_all().unwrap();

        let releases_dir = root.child("releases");
        releases_dir.create_dir_all().unwrap();

        let result = atomic_move(&src_dir, &releases_dir, tag);

        assert!(
            result.is_ok(),
            "atomic_move should complete without errors (including fsync step)"
        );
    }

    #[test]
    fn discover_executables_empty_directory() {
        let root = tempdir().unwrap();
        let result = discover_executables(root.path()).unwrap();
        assert!(
            result.is_empty(),
            "empty directory should return no executables"
        );
    }

    #[test]
    fn discover_executables_flat_directory() {
        let root = tempdir().unwrap();

        create_executable(root.child("exe1"), "#!/bin/sh");
        create_executable(root.child("exe2"), "#!/bin/sh");
        fs::write(root.child("regular.txt"), "not executable").unwrap();

        let result = discover_executables(root.path()).unwrap();

        assert_eq!(result.len(), 2, "should find 2 executables");
        assert!(result.contains(&Utf8PathBuf::from("exe1")));
        assert!(result.contains(&Utf8PathBuf::from("exe2")));
    }

    #[test]
    fn discover_executables_nested_structure() {
        let root = tempdir().unwrap();

        root.child("bin").create_dir_all().unwrap();
        root.child("tools/admin").create_dir_all().unwrap();

        create_executable(root.child("main"), "#!/bin/sh");
        create_executable(root.child("bin/helper"), "#!/bin/sh");
        create_executable(root.child("tools/admin/cli"), "#!/bin/sh");

        let result = discover_executables(root.path()).unwrap();

        assert_eq!(result.len(), 3, "should find 3 executables recursively");
        assert!(result.contains(&Utf8PathBuf::from("main")));
        assert!(result.contains(&Utf8PathBuf::from("bin/helper")));
        assert!(result.contains(&Utf8PathBuf::from("tools/admin/cli")));
    }

    #[test]
    fn discover_executables_skips_non_executables() {
        let root = tempdir().unwrap();

        create_executable(root.child("exe"), "#!/bin/sh");
        fs::write(root.child("readme.txt"), "documentation").unwrap();
        fs::write(root.child("data.json"), "{}").unwrap();

        let result = discover_executables(root.path()).unwrap();

        assert_eq!(result.len(), 1, "should only find executable files");
        assert!(result.contains(&Utf8PathBuf::from("exe")));
    }

    #[test]
    fn discover_executables_returns_relative_paths() {
        let root = tempdir().unwrap();

        root.child("subdir").create_dir_all().unwrap();
        create_executable(root.child("subdir/exe"), "#!/bin/sh");

        let result = discover_executables(root.path()).unwrap();

        assert_eq!(result.len(), 1);
        let path = &result[0];
        assert!(!path.is_absolute(), "path should be relative");
        assert_eq!(path.as_str(), "subdir/exe");
    }

    #[test]
    fn link_binaries_creates_symlinks_to_correct_targets() {
        let root = tempdir().unwrap();

        let releases = root.child("releases");
        let tag_dir = releases.child("v1.0.0");
        tag_dir.create_dir_all().unwrap();

        create_executable(tag_dir.child("exe1"), "#!/bin/sh");

        let bin_dir = root.child("bin");
        bin_dir.create_dir_all().unwrap();

        link_binaries(&tag_dir, &bin_dir).unwrap();

        let symlink = bin_dir.child("exe1");
        assert!(symlink.exists(), "symlink should exist");
        assert!(symlink.is_symlink(), "should be a symlink");

        let target = fs::read_link(&symlink).unwrap();
        assert_eq!(target.to_str().unwrap(), "../releases/v1.0.0/exe1");
    }

    #[test]
    fn link_binaries_flattens_nested_executables() {
        let root = tempdir().unwrap();

        let releases = root.child("releases");
        let tag_dir = releases.child("v1.0.0");
        tag_dir.child("tools/admin").create_dir_all().unwrap();

        create_executable(tag_dir.child("tools/admin/cli"), "#!/bin/sh");

        let bin_dir = root.child("bin");
        bin_dir.create_dir_all().unwrap();

        link_binaries(&tag_dir, &bin_dir).unwrap();

        let symlink = bin_dir.child("cli");
        assert!(symlink.exists(), "flattened symlink should exist");

        let target = fs::read_link(&symlink).unwrap();
        assert_eq!(
            target.to_str().unwrap(),
            "../releases/v1.0.0/tools/admin/cli"
        );
    }

    #[test]
    fn link_binaries_atomically_replaces_existing() {
        let root = tempdir().unwrap();

        let releases = root.child("releases");
        releases.create_dir_all().unwrap();

        let old_tag = releases.child("v1.0.0");
        old_tag.create_dir_all().unwrap();
        create_executable(old_tag.child("exe"), "#!/bin/sh\nold");

        let new_tag = releases.child("v2.0.0");
        new_tag.create_dir_all().unwrap();
        create_executable(new_tag.child("exe"), "#!/bin/sh\nnew");

        let bin_dir = root.child("bin");
        bin_dir.create_dir_all().unwrap();

        link_binaries(&old_tag, &bin_dir).unwrap();

        let symlink = bin_dir.child("exe");
        let old_target = fs::read_link(&symlink).unwrap();
        assert_eq!(old_target.to_str().unwrap(), "../releases/v1.0.0/exe");

        link_binaries(&new_tag, &bin_dir).unwrap();

        let new_target = fs::read_link(&symlink).unwrap();
        assert_eq!(new_target.to_str().unwrap(), "../releases/v2.0.0/exe");
    }

    #[test]
    fn link_binaries_handles_multiple_executables() {
        let root = tempdir().unwrap();

        let releases = root.child("releases");
        let tag_dir = releases.child("v1.0.0");
        tag_dir.child("bin").create_dir_all().unwrap();

        create_executable(tag_dir.child("exe1"), "#!/bin/sh");
        create_executable(tag_dir.child("exe2"), "#!/bin/sh");
        create_executable(tag_dir.child("bin/helper"), "#!/bin/sh");

        let bin_dir = root.child("bin");
        bin_dir.create_dir_all().unwrap();

        link_binaries(&tag_dir, &bin_dir).unwrap();

        assert!(bin_dir.child("exe1").is_symlink());
        assert!(bin_dir.child("exe2").is_symlink());
        assert!(bin_dir.child("helper").is_symlink());

        let target1 = fs::read_link(bin_dir.child("exe1")).unwrap();
        let target2 = fs::read_link(bin_dir.child("exe2")).unwrap();
        let target3 = fs::read_link(bin_dir.child("helper")).unwrap();

        assert_eq!(target1.to_str().unwrap(), "../releases/v1.0.0/exe1");
        assert_eq!(target2.to_str().unwrap(), "../releases/v1.0.0/exe2");
        assert_eq!(target3.to_str().unwrap(), "../releases/v1.0.0/bin/helper");
    }

    #[test]
    fn link_binaries_last_wins_on_filename_collision() {
        let root = tempdir().unwrap();

        let releases = root.child("releases");
        let tag_dir = releases.child("v1.0.0");
        tag_dir.child("tools").create_dir_all().unwrap();
        tag_dir.child("bin").create_dir_all().unwrap();

        create_executable(tag_dir.child("tools/cli"), "#!/bin/sh\ntools version");
        create_executable(tag_dir.child("bin/cli"), "#!/bin/sh\nbin version");

        let bin_dir = root.child("bin");
        bin_dir.create_dir_all().unwrap();

        link_binaries(&tag_dir, &bin_dir).unwrap();

        let symlink = bin_dir.child("cli");
        assert!(symlink.exists(), "cli symlink should exist");

        let target = fs::read_link(&symlink).unwrap();
        assert!(
            target.to_str().unwrap().contains("bin/cli"),
            "last executable (bin/cli) should win, got: {target:?}"
        );
    }

    #[test]
    fn prune_old_releases_keeps_most_recent() {
        let root = tempdir().unwrap();
        let releases_dir = root.child("releases");
        releases_dir.create_dir_all().unwrap();

        for i in 1..=5 {
            let tag = format!("v1.0.{i}");
            let release = releases_dir.child(&tag);
            release.create_dir_all().unwrap();
            release.child("binary").write_str("data").unwrap();

            thread::sleep(Duration::from_millis(10));
        }

        let deleted = prune_old_releases(&releases_dir, "v1.0.5", 3).unwrap();

        assert_eq!(deleted.len(), 2, "should delete 2 old releases");

        assert!(releases_dir.child("v1.0.5").exists());
        assert!(releases_dir.child("v1.0.4").exists());
        assert!(releases_dir.child("v1.0.3").exists());
        assert!(!releases_dir.child("v1.0.1").exists());
        assert!(!releases_dir.child("v1.0.2").exists());
    }

    #[test]
    fn prune_old_releases_with_retain_zero() {
        let root = tempdir().unwrap();
        let releases_dir = root.child("releases");
        releases_dir.create_dir_all().unwrap();

        releases_dir.child("v1.0.0").create_dir_all().unwrap();
        releases_dir.child("v1.0.1").create_dir_all().unwrap();
        releases_dir.child("v1.0.2").create_dir_all().unwrap();

        let deleted = prune_old_releases(&releases_dir, "v1.0.2", 0).unwrap();

        assert_eq!(deleted.len(), 2);
        assert!(
            releases_dir.child("v1.0.2").exists(),
            "current should remain"
        );
        assert!(!releases_dir.child("v1.0.0").exists());
        assert!(!releases_dir.child("v1.0.1").exists());
    }

    #[test]
    fn prune_old_releases_no_deletions_when_under_limit() {
        let root = tempdir().unwrap();
        let releases_dir = root.child("releases");
        releases_dir.create_dir_all().unwrap();

        releases_dir.child("v1.0.0").create_dir_all().unwrap();
        releases_dir.child("v1.0.1").create_dir_all().unwrap();

        let deleted = prune_old_releases(&releases_dir, "v1.0.1", 5).unwrap();

        assert_eq!(deleted.len(), 0, "should not delete when count <= retain");
        assert!(releases_dir.child("v1.0.0").exists());
        assert!(releases_dir.child("v1.0.1").exists());
    }

    #[test]
    fn prune_old_releases_empty_directory() {
        let root = tempdir().unwrap();
        let releases_dir = root.child("releases");
        releases_dir.create_dir_all().unwrap();

        let deleted = prune_old_releases(&releases_dir, "v1.0.0", 3).unwrap();

        assert_eq!(deleted.len(), 0, "empty directory should be no-op");
    }

    #[test]
    fn prune_old_releases_never_deletes_current() {
        let root = tempdir().unwrap();
        let releases_dir = root.child("releases");
        releases_dir.create_dir_all().unwrap();

        releases_dir.child("v1.0.0").create_dir_all().unwrap();
        thread::sleep(Duration::from_millis(10));
        releases_dir.child("v1.0.1").create_dir_all().unwrap();
        thread::sleep(Duration::from_millis(10));

        releases_dir.child("v1.0.2").create_dir_all().unwrap();

        let deleted = prune_old_releases(&releases_dir, "v1.0.0", 1).unwrap();

        assert!(releases_dir.child("v1.0.0").exists());
        assert!(!deleted.is_empty());
    }

    #[test]
    fn prune_old_releases_ignores_non_directories() {
        let root = tempdir().unwrap();
        let releases_dir = root.child("releases");
        releases_dir.create_dir_all().unwrap();

        releases_dir.child("v1.0.0").create_dir_all().unwrap();
        releases_dir.child("v1.0.1").create_dir_all().unwrap();
        releases_dir.child("notes.txt").write_str("readme").unwrap();

        let deleted = prune_old_releases(&releases_dir, "v1.0.1", 1).unwrap();

        assert_eq!(deleted.len(), 1);
        assert!(releases_dir.child("notes.txt").exists());
    }
}

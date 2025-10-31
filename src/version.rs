use std::{fs, io};

use camino::{Utf8Path, Utf8PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum VersionError {
    #[error("IO error: {0}")]
    Io(#[from] io::Error),
}

pub type Result<T> = std::result::Result<T, VersionError>;

/// Discovers the currently installed version tag by examining symlinks in the bin directory.
///
/// Looks under `<prefix>/<app>/bin/` for symlinks that point into `../releases/<tag>/...`
/// and extracts the `<tag>` component. When multiple symlinks exist, returns the tag from
/// the lexicographically last symlink name.
///
/// Returns `Ok(None)` if:
/// - The bin directory does not exist
/// - The bin directory is empty
/// - No symlinks point into the releases directory
///
/// # Errors
///
/// Returns an error if:
/// - Reading the bin directory fails due to I/O errors
/// - Reading directory entries fails
/// - Reading symlink metadata fails
/// - Reading symlink targets fails
pub fn current_tag<P: AsRef<Utf8Path>>(prefix: P, app: &str) -> Result<Option<String>> {
    let prefix = prefix.as_ref();
    let bin_dir = prefix.join(app).join("bin");

    if !bin_dir.is_dir() {
        return Ok(None);
    }

    let mut symlinks = fs::read_dir(&bin_dir)?
        .map(|entry| {
            let entry = entry?;
            let path = entry.path();

            let metadata = fs::symlink_metadata(&path)?;
            if !metadata.is_symlink() {
                return Ok(None);
            }

            let target = fs::read_link(&path)?;
            let target_utf8 = Utf8PathBuf::from_path_buf(target.clone())
                .unwrap_or_else(|p| Utf8PathBuf::from(p.to_string_lossy().as_ref()));

            let target_path = if target_utf8.is_relative() {
                bin_dir.join(target_utf8)
            } else {
                target_utf8
            };

            let Some(tag) = extract_tag_from_path(&target_path) else {
                return Ok(None);
            };

            let file_name = entry.file_name();
            Ok(Some((file_name, tag)))
        })
        .collect::<io::Result<Vec<Option<_>>>>()?
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();

    if symlinks.is_empty() {
        return Ok(None);
    }

    symlinks.sort_by(|(a, _), (b, _)| a.cmp(b));
    #[allow(clippy::missing_panics_doc)]
    let (_file_name, tag) = symlinks.last().unwrap();

    Ok(Some(tag.clone()))
}

/// Extracts the tag from a path containing "releases/<tag>/..."
fn extract_tag_from_path(path: &Utf8Path) -> Option<String> {
    let components: Vec<_> = path.components().collect();
    components
        .iter()
        .enumerate()
        .find(|(_, component)| component.as_str() == "releases")
        .and_then(|(i, _)| components.get(i + 1))
        .map(|component| component.as_str().to_string())
}

/// Prints diagnostic information about the version discovery process.
///
/// Shows:
/// - The bin directory path being checked
/// - Any symlinks found and their targets
/// - The releases directory path
/// - The current version tag if discovered
///
/// # Errors
///
/// Returns an error if:
/// - Reading the bin directory fails due to I/O errors
/// - Reading directory entries fails
/// - Reading symlink metadata fails
/// - Reading symlink targets fails
pub fn print_diagnostics<P: AsRef<Utf8Path>>(
    prefix: P,
    app: &str,
    current_tag: Option<&str>,
) -> Result<()> {
    let prefix = prefix.as_ref();
    let bin_dir = prefix.join(app).join("bin");
    let releases_dir = prefix.join(app).join("releases");

    println!("Diagnostic information:");
    println!("  Bin directory: {bin_dir}");
    println!("  Releases directory: {releases_dir}");
    println!();

    if !bin_dir.is_dir() {
        println!("  No bin directory found");
        println!();
        println!("Current version: (none)");
        return Ok(());
    }

    println!("  Symlinks in bin directory:");
    let entries = fs::read_dir(&bin_dir)?;
    let mut symlink_count = 0;

    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)?;

        if metadata.is_symlink() {
            let target = fs::read_link(&path)?;
            let file_name = entry.file_name();
            println!(
                "    {} -> {}",
                file_name.to_string_lossy(),
                target.display()
            );
            symlink_count += 1;
        }
    }

    if symlink_count == 0 {
        println!("    (no symlinks found)");
    }

    println!();

    if let Some(tag) = current_tag {
        println!("Current version: {tag}");
    } else {
        println!("Current version: (none)");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use camino_tempfile::tempdir;
    use camino_tempfile_ext::prelude::*;

    use super::*;

    #[test]
    fn test_current_tag_from_symlink() {
        let temp_dir = tempdir().unwrap();
        let opt_root = temp_dir.child("opt");
        let app = "myapp";

        let releases_dir = opt_root.child(app).child("releases").child("v1.2.3");
        releases_dir.create_dir_all().unwrap();
        let binary = releases_dir.child("foo");
        binary.write_str("fake binary").unwrap();

        let bin_dir = opt_root.child(app).child("bin");
        bin_dir.create_dir_all().unwrap();
        let symlink_path = bin_dir.child("foo");
        symlink("../releases/v1.2.3/foo", symlink_path.as_std_path()).unwrap();

        let result = current_tag(&opt_root, app).unwrap();
        assert_eq!(result, Some("v1.2.3".to_string()));
    }

    #[test]
    fn test_current_tag_no_bin_directory() {
        let temp_dir = tempdir().unwrap();
        let opt_root = temp_dir.child("opt");

        let result = current_tag(&opt_root, "myapp").unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn test_current_tag_bin_exists_no_symlinks() {
        let temp_dir = tempdir().unwrap();
        let opt_root = temp_dir.child("opt");
        let app = "myapp";

        let bin_dir = opt_root.child(app).child("bin");
        bin_dir.create_dir_all().unwrap();

        let result = current_tag(&opt_root, app).unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn test_current_tag_symlink_not_to_releases() {
        let temp_dir = tempdir().unwrap();
        let opt_root = temp_dir.child("opt");
        let app = "myapp";

        let bin_dir = opt_root.child(app).child("bin");
        bin_dir.create_dir_all().unwrap();
        let symlink_path = bin_dir.child("foo");
        symlink("/usr/bin/foo", symlink_path.as_std_path()).unwrap();

        let result = current_tag(&opt_root, app).unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn test_current_tag_multiple_symlinks() {
        let temp_dir = tempdir().unwrap();
        let opt_root = temp_dir.child("opt");
        let app = "myapp";

        for version in ["v1.2.3", "v1.2.4"] {
            let releases_dir = opt_root.child(app).child("releases").child(version);
            releases_dir.create_dir_all().unwrap();
            releases_dir.child("binary").write_str("fake").unwrap();
        }

        let bin_dir = opt_root.child(app).child("bin");
        bin_dir.create_dir_all().unwrap();

        symlink(
            "../releases/v1.2.3/binary",
            bin_dir.child("binary").as_std_path(),
        )
        .unwrap();
        symlink(
            "../releases/v1.2.4/other",
            bin_dir.child("other").as_std_path(),
        )
        .unwrap();

        let result = current_tag(&opt_root, app).unwrap();
        assert_eq!(result, Some("v1.2.4".to_string()));
    }

    #[test]
    fn test_current_tag_absolute_path_symlink() {
        let temp_dir = tempdir().unwrap();
        let opt_root = temp_dir.child("opt");
        let app = "myapp";

        let releases_dir = opt_root.child(app).child("releases").child("v2.0.0");
        releases_dir.create_dir_all().unwrap();
        let binary = releases_dir.child("foo");
        binary.write_str("fake binary").unwrap();

        let bin_dir = opt_root.child(app).child("bin");
        bin_dir.create_dir_all().unwrap();
        let symlink_path = bin_dir.child("foo");
        symlink(binary.as_std_path(), symlink_path.as_std_path()).unwrap();

        let result = current_tag(&opt_root, app).unwrap();
        assert_eq!(result, Some("v2.0.0".to_string()));
    }
}

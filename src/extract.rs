use std::{
    fs,
    io::{self, Read},
    path::{Component, Path},
};

use camino::Utf8Path;
use thiserror::Error;

/// Limits for archive extraction to prevent zip bombs and resource exhaustion.
#[derive(Debug, Clone, Copy)]
pub struct ExtractionLimits {
    /// Maximum total bytes that can be extracted across all files (default: 10 GiB)
    pub max_total_extracted_bytes: u64,
    /// Maximum number of files that can be extracted (default: 10,000)
    pub max_file_count: usize,
    /// Maximum size of any individual extracted file (default: 1 GiB)
    pub max_individual_file_bytes: u64,
    /// Maximum decompression ratio (uncompressed/compressed) (default: 100)
    pub max_decompression_ratio: u64,
}

impl Default for ExtractionLimits {
    fn default() -> Self {
        Self {
            max_total_extracted_bytes: 10 * 1024 * 1024 * 1024, // 10 GiB
            max_file_count: 10_000,
            max_individual_file_bytes: 1024 * 1024 * 1024, // 1 GiB
            max_decompression_ratio: 100,
        }
    }
}

#[derive(Debug, Error)]
pub enum ExtractError {
    #[error("unsupported archive format")]
    UnsupportedFormat,
    #[error("path validation failed: {0}")]
    PathValidation(String),
    #[error("extraction limit exceeded: {0}")]
    LimitExceeded(String),
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("zip error: {0}")]
    Zip(#[from] zip::result::ZipError),
}

pub type Result<T> = std::result::Result<T, ExtractError>;

/// A reader wrapper that tracks bytes read and enforces a limit.
struct LimitedReader<R> {
    inner: R,
    limit: u64,
    read: u64,
}

impl<R: Read> LimitedReader<R> {
    fn new(inner: R, limit: u64) -> Self {
        Self {
            inner,
            limit,
            read: 0,
        }
    }

    fn bytes_read(&self) -> u64 {
        self.read
    }
}

impl<R: Read> Read for LimitedReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let remaining = self.limit.saturating_sub(self.read);
        if remaining == 0 {
            return Ok(0);
        }

        #[allow(clippy::cast_possible_truncation)]
        let max_read = remaining.min(buf.len() as u64) as usize;
        let n = self.inner.read(&mut buf[..max_read])?;
        self.read += n as u64;

        if self.read > self.limit {
            return Err(io::Error::other("file size limit exceeded"));
        }

        Ok(n)
    }
}

#[cfg(unix)]
fn set_unix_permissions(path: impl AsRef<Utf8Path>, mode: u32) -> Result<()> {
    use std::{fs, os::unix::fs::PermissionsExt};
    let permissions = fs::Permissions::from_mode(mode);
    fs::set_permissions(path.as_ref(), permissions)?;
    Ok(())
}

fn validate_path(path: &Path) -> Result<()> {
    if path.is_absolute() {
        return Err(ExtractError::PathValidation(
            "absolute paths are not allowed".to_string(),
        ));
    }

    for component in path.components() {
        if component == Component::ParentDir {
            return Err(ExtractError::PathValidation(
                "paths containing '..' are not allowed".to_string(),
            ));
        }
    }

    Ok(())
}

fn unpack_zip(
    src: impl AsRef<Utf8Path>,
    dest_dir: impl AsRef<Utf8Path>,
    limits: &ExtractionLimits,
) -> Result<()> {
    let src = src.as_ref();
    let dest_dir = dest_dir.as_ref();

    let file = fs::File::open(src)?;
    let mut archive = zip::ZipArchive::new(file)?;

    let common_root = detect_single_root_zip(&mut archive)?;

    let mut total_bytes = 0u64;
    let mut file_count = 0usize;

    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        let entry_path = entry.enclosed_name().ok_or_else(|| {
            ExtractError::PathValidation(format!("invalid entry path: {}", entry.name()))
        })?;

        validate_path(&entry_path)?;

        let stripped_path = if let Some(ref root) = common_root {
            entry_path.strip_prefix(root).unwrap_or(&entry_path)
        } else {
            &entry_path
        };

        if stripped_path.as_os_str().is_empty() {
            continue;
        }

        let dest_path = dest_dir.join(stripped_path.to_string_lossy().as_ref());

        if entry.is_dir() {
            fs::create_dir_all(&dest_path)?;
        } else {
            if file_count >= limits.max_file_count {
                return Err(ExtractError::LimitExceeded(format!(
                    "file count limit exceeded: {} files",
                    limits.max_file_count
                )));
            }

            let uncompressed_size = entry.size();
            let compressed_size = entry.compressed_size();

            if uncompressed_size > limits.max_individual_file_bytes {
                return Err(ExtractError::LimitExceeded(format!(
                    "individual file size limit exceeded: {} bytes (limit: {})",
                    uncompressed_size, limits.max_individual_file_bytes
                )));
            }

            if compressed_size > 0 {
                let ratio = uncompressed_size / compressed_size;
                if ratio > limits.max_decompression_ratio {
                    return Err(ExtractError::LimitExceeded(format!(
                        "decompression ratio exceeded: {} (limit: {})",
                        ratio, limits.max_decompression_ratio
                    )));
                }
            }

            if total_bytes + uncompressed_size > limits.max_total_extracted_bytes {
                return Err(ExtractError::LimitExceeded(format!(
                    "total extracted bytes limit exceeded: {} bytes",
                    limits.max_total_extracted_bytes
                )));
            }

            if let Some(parent) = dest_path.parent() {
                fs::create_dir_all(parent)?;
            }

            let mut outfile = fs::File::create(&dest_path)?;
            let mut limited_reader = LimitedReader::new(&mut entry, uncompressed_size);
            io::copy(&mut limited_reader, &mut outfile)?;

            total_bytes += limited_reader.bytes_read();
            file_count += 1;

            #[cfg(unix)]
            if let Some(mode) = entry.unix_mode()
                && mode & 0o111 != 0
            {
                set_unix_permissions(&dest_path, mode)?;
            }
        }
    }

    Ok(())
}

fn detect_single_root_zip(archive: &mut zip::ZipArchive<fs::File>) -> Result<Option<String>> {
    let mut root_dirs = std::collections::HashSet::new();
    let mut has_directory_root = false;

    for i in 0..archive.len() {
        let entry = archive.by_index(i)?;
        if let Some(enclosed) = entry.enclosed_name()
            && let Some(first_component) = enclosed.components().next()
        {
            let component_str = first_component.as_os_str().to_string_lossy().to_string();
            root_dirs.insert(component_str.clone());

            if entry.is_dir() && enclosed.components().count() == 1 {
                has_directory_root = true;
            }
        }
    }

    if root_dirs.len() == 1 && has_directory_root {
        Ok(Some(root_dirs.into_iter().next().unwrap()))
    } else {
        Ok(None)
    }
}

fn unpack_tar(
    src: impl AsRef<Utf8Path>,
    dest_dir: impl AsRef<Utf8Path>,
    limits: &ExtractionLimits,
) -> Result<()> {
    let src = src.as_ref();
    let dest_dir = dest_dir.as_ref();

    let reader = autocompress::autodetect_open(src.as_std_path())?;
    let mut archive = tar::Archive::new(reader);

    let entries: Vec<_> = archive.entries()?.collect::<io::Result<_>>()?;

    let common_root = detect_single_root_tar(&entries);

    let reader = autocompress::autodetect_open(src.as_std_path())?;
    let mut archive = tar::Archive::new(reader);

    let mut total_bytes = 0u64;
    let mut file_count = 0usize;

    for entry in archive.entries()? {
        let mut entry = entry?;
        let entry_path = entry.path()?;

        validate_path(&entry_path)?;

        let stripped_path = if let Some(ref root) = common_root {
            entry_path.strip_prefix(root).unwrap_or(&entry_path)
        } else {
            entry_path.as_ref()
        };

        if stripped_path.as_os_str().is_empty() {
            continue;
        }

        let dest_path = dest_dir.join(stripped_path.to_string_lossy().as_ref());

        if entry.header().entry_type().is_dir() {
            fs::create_dir_all(&dest_path)?;
        } else if entry.header().entry_type().is_file() {
            if file_count >= limits.max_file_count {
                return Err(ExtractError::LimitExceeded(format!(
                    "file count limit exceeded: {} files",
                    limits.max_file_count
                )));
            }

            let uncompressed_size = entry.header().size()?;

            if uncompressed_size > limits.max_individual_file_bytes {
                return Err(ExtractError::LimitExceeded(format!(
                    "individual file size limit exceeded: {} bytes (limit: {})",
                    uncompressed_size, limits.max_individual_file_bytes
                )));
            }

            if total_bytes + uncompressed_size > limits.max_total_extracted_bytes {
                return Err(ExtractError::LimitExceeded(format!(
                    "total extracted bytes limit exceeded: {} bytes",
                    limits.max_total_extracted_bytes
                )));
            }

            if let Some(parent) = dest_path.parent() {
                fs::create_dir_all(parent)?;
            }

            let mut outfile = fs::File::create(&dest_path)?;
            let mut limited_reader = LimitedReader::new(&mut entry, uncompressed_size);
            io::copy(&mut limited_reader, &mut outfile)?;

            total_bytes += limited_reader.bytes_read();
            file_count += 1;

            #[cfg(unix)]
            if let Ok(mode) = entry.header().mode() {
                set_unix_permissions(&dest_path, mode)?;
            }
        }
    }

    Ok(())
}

fn detect_single_root_tar(entries: &[tar::Entry<'_, impl io::Read>]) -> Option<String> {
    let mut root_dirs = std::collections::HashSet::new();
    let mut has_directory_root = false;

    for entry in entries {
        if let Ok(path) = entry.path()
            && let Some(first_component) = path.components().next()
        {
            let component_str = first_component.as_os_str().to_string_lossy().to_string();
            root_dirs.insert(component_str.clone());

            if entry.header().entry_type().is_dir() && path.components().count() == 1 {
                has_directory_root = true;
            }
        }
    }

    if root_dirs.len() == 1 && has_directory_root {
        Some(root_dirs.into_iter().next().unwrap())
    } else {
        None
    }
}

fn ends_with_ignore_case(s: &str, suffix: &str) -> bool {
    s.len() >= suffix.len() && s[s.len() - suffix.len()..].eq_ignore_ascii_case(suffix)
}

/// Extracts an archive to the specified directory with default limits.
///
/// This is a convenience wrapper around `unpack_with_limits` that uses
/// `ExtractionLimits::default()`.
///
/// Supported formats:
/// - Zip archives (`.zip`)
/// - Tar with gzip (`.tar.gz`, `.tgz`)
/// - Tar with bzip2 (`.tar.bz2`, `.tbz2`)
/// - Tar with xz (`.tar.xz`, `.txz`)
/// - Tar with zstd (`.tar.zst`)
///
/// # Errors
///
/// Returns an error if:
/// - The archive format is unsupported
/// - An entry path contains `..` or is absolute
/// - Extraction limits are exceeded (file count, size, decompression ratio)
/// - I/O operations fail during extraction
/// - The archive is corrupted or cannot be read
pub fn unpack(src: impl AsRef<Utf8Path>, dest_dir: impl AsRef<Utf8Path>) -> Result<()> {
    unpack_with_limits(src, dest_dir, &ExtractionLimits::default())
}

/// Extracts an archive to the specified directory with custom limits.
///
/// Supported formats:
/// - Zip archives (`.zip`)
/// - Tar with gzip (`.tar.gz`, `.tgz`)
/// - Tar with bzip2 (`.tar.bz2`, `.tbz2`)
/// - Tar with xz (`.tar.xz`, `.txz`)
/// - Tar with zstd (`.tar.zst`)
///
/// # Errors
///
/// Returns an error if:
/// - The archive format is unsupported
/// - An entry path contains `..` or is absolute
/// - Extraction limits are exceeded (file count, size, decompression ratio)
/// - I/O operations fail during extraction
/// - The archive is corrupted or cannot be read
pub fn unpack_with_limits(
    src: impl AsRef<Utf8Path>,
    dest_dir: impl AsRef<Utf8Path>,
    limits: &ExtractionLimits,
) -> Result<()> {
    let src = src.as_ref();
    let path_str = src.as_str();

    if ends_with_ignore_case(path_str, ".zip") {
        unpack_zip(src, dest_dir, limits)
    } else if ends_with_ignore_case(path_str, ".tar.gz")
        || ends_with_ignore_case(path_str, ".tgz")
        || ends_with_ignore_case(path_str, ".tar.bz2")
        || ends_with_ignore_case(path_str, ".tbz2")
        || ends_with_ignore_case(path_str, ".tar.xz")
        || ends_with_ignore_case(path_str, ".txz")
        || ends_with_ignore_case(path_str, ".tar.zst")
    {
        unpack_tar(src, dest_dir, limits)
    } else {
        Err(ExtractError::UnsupportedFormat)
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs::{self, File},
        io::Write,
    };

    use assert_matches::assert_matches;
    use camino_tempfile::tempdir;
    use camino_tempfile_ext::prelude::*;

    use super::*;

    #[test]
    fn test_reject_absolute_path_zip() {
        let temp_dir = tempdir().unwrap();
        let zip_path = temp_dir.child("evil.zip");

        let file = File::create(&zip_path).unwrap();
        let mut zip = zip::ZipWriter::new(file);

        let options = zip::write::FileOptions::<()>::default()
            .compression_method(zip::CompressionMethod::Stored);
        zip.start_file("/etc/passwd", options).unwrap();
        zip.write_all(b"evil content").unwrap();
        zip.finish().unwrap();

        let extract_dir = temp_dir.child("extract");
        extract_dir.create_dir_all().unwrap();

        let result = unpack(&zip_path, &extract_dir);
        assert!(result.is_err());
        if let Err(ExtractError::PathValidation(msg)) = result {
            assert!(msg.contains("invalid entry path"));
        } else {
            panic!("Expected PathValidation error, got: {result:?}");
        }
    }

    #[test]
    fn test_reject_parent_traversal_zip() {
        let temp_dir = tempdir().unwrap();
        let zip_path = temp_dir.child("evil.zip");

        let file = File::create(&zip_path).unwrap();
        let mut zip = zip::ZipWriter::new(file);

        let options = zip::write::FileOptions::<()>::default()
            .compression_method(zip::CompressionMethod::Stored);
        zip.start_file("../evil", options).unwrap();
        zip.write_all(b"evil content").unwrap();
        zip.finish().unwrap();

        let extract_dir = temp_dir.child("extract");
        extract_dir.create_dir_all().unwrap();

        let result = unpack(&zip_path, &extract_dir);
        assert!(result.is_err());
        if let Err(ExtractError::PathValidation(msg)) = result {
            assert!(msg.contains("invalid entry path"));
        } else {
            panic!("Expected PathValidation error, got: {result:?}");
        }
    }

    #[test]
    fn test_reject_absolute_path_tar() {
        let temp_dir = tempdir().unwrap();
        let tar_gz_path = temp_dir.child("evil.tar.gz");

        let file = File::create(&tar_gz_path).unwrap();
        let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
        let mut tar = tar::Builder::new(encoder);

        let data = b"evil content";
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        let path_bytes = b"/etc/passwd\0";
        let mut name = [0u8; 100];
        name[..path_bytes.len()].copy_from_slice(path_bytes);
        header.as_gnu_mut().unwrap().name = name;
        header.set_cksum();
        tar.append(&header, &data[..]).unwrap();
        tar.into_inner().unwrap().finish().unwrap();

        let extract_dir = temp_dir.child("extract");
        extract_dir.create_dir_all().unwrap();

        let result = unpack(&tar_gz_path, &extract_dir);
        assert_matches!(
            result,
            Err(ExtractError::PathValidation(msg)) if msg.contains("absolute")
        );
    }

    #[test]
    fn test_reject_parent_traversal_tar() {
        let temp_dir = tempdir().unwrap();
        let tar_gz_path = temp_dir.child("evil.tar.gz");

        let file = File::create(&tar_gz_path).unwrap();
        let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
        let mut tar = tar::Builder::new(encoder);

        let data = b"evil content";
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        let path_bytes = b"../evil\0";
        let mut name = [0u8; 100];
        name[..path_bytes.len()].copy_from_slice(path_bytes);
        header.as_gnu_mut().unwrap().name = name;
        header.set_cksum();
        tar.append(&header, &data[..]).unwrap();
        tar.into_inner().unwrap().finish().unwrap();

        let extract_dir = temp_dir.child("extract");
        extract_dir.create_dir_all().unwrap();

        let result = unpack(&tar_gz_path, &extract_dir);
        assert_matches!(
            result,
            Err(ExtractError::PathValidation(msg)) if msg.contains("..")
        );
    }

    #[test]
    fn test_zip_single_root_stripped() {
        let temp_dir = tempdir().unwrap();
        let zip_path = temp_dir.child("archive.zip");

        let file = File::create(&zip_path).unwrap();
        let mut zip = zip::ZipWriter::new(file);

        let options = zip::write::FileOptions::<()>::default()
            .compression_method(zip::CompressionMethod::Stored);

        zip.add_directory("myapp-v1.0/", options).unwrap();
        zip.start_file("myapp-v1.0/file.txt", options).unwrap();
        zip.write_all(b"content").unwrap();
        zip.start_file("myapp-v1.0/subdir/nested.txt", options)
            .unwrap();
        zip.write_all(b"nested").unwrap();
        zip.finish().unwrap();

        let extract_dir = temp_dir.child("extract");
        extract_dir.create_dir_all().unwrap();

        unpack(&zip_path, &extract_dir).unwrap();

        assert!(extract_dir.join("file.txt").exists());
        assert!(extract_dir.join("subdir/nested.txt").exists());
        assert!(!extract_dir.join("myapp-v1.0").exists());
    }

    #[test]
    fn test_zip_basic_extraction() {
        let temp_dir = tempdir().unwrap();
        let zip_path = temp_dir.child("archive.zip");

        let file = File::create(&zip_path).unwrap();
        let mut zip = zip::ZipWriter::new(file);

        let options = zip::write::FileOptions::<()>::default()
            .compression_method(zip::CompressionMethod::Stored);
        zip.start_file("hello.txt", options).unwrap();
        zip.write_all(b"Hello, World!").unwrap();
        zip.finish().unwrap();

        let extract_dir = temp_dir.child("extract");
        extract_dir.create_dir_all().unwrap();

        unpack(&zip_path, &extract_dir).unwrap();

        let content = fs::read_to_string(extract_dir.join("hello.txt")).unwrap();
        assert_eq!(content, "Hello, World!");
    }

    #[test]
    fn test_tar_gz_extraction() {
        let temp_dir = tempdir().unwrap();
        let tar_gz_path = temp_dir.child("archive.tar.gz");

        let file = File::create(&tar_gz_path).unwrap();
        let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
        let mut tar = tar::Builder::new(encoder);

        let mut header = tar::Header::new_gnu();
        let data = b"Hello from tar.gz!";
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        tar.append_data(&mut header, "file.txt", &data[..]).unwrap();
        tar.into_inner().unwrap().finish().unwrap();

        let extract_dir = temp_dir.child("extract");
        extract_dir.create_dir_all().unwrap();

        unpack(&tar_gz_path, &extract_dir).unwrap();

        let content = fs::read_to_string(extract_dir.join("file.txt")).unwrap();
        assert_eq!(content, "Hello from tar.gz!");
    }

    #[test]
    fn test_tar_bz2_extraction() {
        let temp_dir = tempdir().unwrap();
        let tar_bz2_path = temp_dir.child("archive.tar.bz2");

        let file = File::create(&tar_bz2_path).unwrap();
        let encoder = bzip2::write::BzEncoder::new(file, bzip2::Compression::default());
        let mut tar = tar::Builder::new(encoder);

        let mut header = tar::Header::new_gnu();
        let data = b"Hello from tar.bz2!";
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        tar.append_data(&mut header, "file.txt", &data[..]).unwrap();
        tar.into_inner().unwrap().finish().unwrap();

        let extract_dir = temp_dir.child("extract");
        extract_dir.create_dir_all().unwrap();

        unpack(&tar_bz2_path, &extract_dir).unwrap();

        let content = fs::read_to_string(extract_dir.join("file.txt")).unwrap();
        assert_eq!(content, "Hello from tar.bz2!");
    }

    #[test]
    fn test_tar_xz_extraction() {
        let temp_dir = tempdir().unwrap();
        let tar_xz_path = temp_dir.child("archive.tar.xz");

        let file = File::create(&tar_xz_path).unwrap();
        let encoder = xz2::write::XzEncoder::new(file, 6);
        let mut tar = tar::Builder::new(encoder);

        let mut header = tar::Header::new_gnu();
        let data = b"Hello from tar.xz!";
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        tar.append_data(&mut header, "file.txt", &data[..]).unwrap();
        tar.into_inner().unwrap().finish().unwrap();

        let extract_dir = temp_dir.child("extract");
        extract_dir.create_dir_all().unwrap();

        unpack(&tar_xz_path, &extract_dir).unwrap();

        let content = fs::read_to_string(extract_dir.join("file.txt")).unwrap();
        assert_eq!(content, "Hello from tar.xz!");
    }

    #[test]
    fn test_tar_zst_extraction() {
        let temp_dir = tempdir().unwrap();
        let tar_zst_path = temp_dir.child("archive.tar.zst");

        let file = File::create(&tar_zst_path).unwrap();
        let encoder = zstd::Encoder::new(file, 3).unwrap();
        let mut tar = tar::Builder::new(encoder);

        let mut header = tar::Header::new_gnu();
        let data = b"Hello from tar.zst!";
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        tar.append_data(&mut header, "file.txt", &data[..]).unwrap();
        tar.into_inner().unwrap().finish().unwrap();

        let extract_dir = temp_dir.child("extract");
        extract_dir.create_dir_all().unwrap();

        unpack(&tar_zst_path, &extract_dir).unwrap();

        let content = fs::read_to_string(extract_dir.join("file.txt")).unwrap();
        assert_eq!(content, "Hello from tar.zst!");
    }

    #[test]
    fn test_zip_file_count_limit_exceeded() {
        let temp_dir = tempdir().unwrap();
        let zip_path = temp_dir.child("many-files.zip");

        let file = File::create(&zip_path).unwrap();
        let mut zip = zip::ZipWriter::new(file);

        let options = zip::write::FileOptions::<()>::default()
            .compression_method(zip::CompressionMethod::Stored);

        for i in 0..5 {
            zip.start_file(format!("file{i}.txt"), options).unwrap();
            zip.write_all(b"content").unwrap();
        }
        zip.finish().unwrap();

        let extract_dir = temp_dir.child("extract");
        extract_dir.create_dir_all().unwrap();

        let limits = ExtractionLimits {
            max_file_count: 3,
            ..Default::default()
        };

        let result = unpack_with_limits(&zip_path, &extract_dir, &limits);
        assert_matches!(
            result,
            Err(ExtractError::LimitExceeded(msg)) if msg.contains("file count")
        );
    }

    #[test]
    fn test_zip_individual_file_size_limit_exceeded() {
        let temp_dir = tempdir().unwrap();
        let zip_path = temp_dir.child("large-file.zip");

        let file = File::create(&zip_path).unwrap();
        let mut zip = zip::ZipWriter::new(file);

        let options = zip::write::FileOptions::<()>::default()
            .compression_method(zip::CompressionMethod::Stored);

        let large_data = vec![b'x'; 2000];
        zip.start_file("large.txt", options).unwrap();
        zip.write_all(&large_data).unwrap();
        zip.finish().unwrap();

        let extract_dir = temp_dir.child("extract");
        extract_dir.create_dir_all().unwrap();

        let limits = ExtractionLimits {
            max_individual_file_bytes: 1000,
            ..Default::default()
        };

        let result = unpack_with_limits(&zip_path, &extract_dir, &limits);
        assert_matches!(
            result,
            Err(ExtractError::LimitExceeded(msg)) if msg.contains("individual file size")
        );
    }

    #[test]
    fn test_zip_total_bytes_limit_exceeded() {
        let temp_dir = tempdir().unwrap();
        let zip_path = temp_dir.child("many-files.zip");

        let file = File::create(&zip_path).unwrap();
        let mut zip = zip::ZipWriter::new(file);

        let options = zip::write::FileOptions::<()>::default()
            .compression_method(zip::CompressionMethod::Stored);

        let data = vec![b'x'; 500];
        for i in 0..5 {
            zip.start_file(format!("file{i}.txt"), options).unwrap();
            zip.write_all(&data).unwrap();
        }
        zip.finish().unwrap();

        let extract_dir = temp_dir.child("extract");
        extract_dir.create_dir_all().unwrap();

        let limits = ExtractionLimits {
            max_total_extracted_bytes: 2000,
            ..Default::default()
        };

        let result = unpack_with_limits(&zip_path, &extract_dir, &limits);
        assert_matches!(
            result,
            Err(ExtractError::LimitExceeded(msg)) if msg.contains("total extracted bytes")
        );
    }

    #[test]
    fn test_zip_decompression_ratio_limit_exceeded() {
        let temp_dir = tempdir().unwrap();
        let zip_path = temp_dir.child("high-ratio.zip");

        let file = File::create(&zip_path).unwrap();
        let mut zip = zip::ZipWriter::new(file);

        let options = zip::write::FileOptions::<()>::default()
            .compression_method(zip::CompressionMethod::Deflated);

        let data = vec![0u8; 10_000];
        zip.start_file("compressible.txt", options).unwrap();
        zip.write_all(&data).unwrap();
        zip.finish().unwrap();

        let extract_dir = temp_dir.child("extract");
        extract_dir.create_dir_all().unwrap();

        let limits = ExtractionLimits {
            max_decompression_ratio: 10,
            ..Default::default()
        };

        let result = unpack_with_limits(&zip_path, &extract_dir, &limits);
        assert_matches!(
            result,
            Err(ExtractError::LimitExceeded(msg)) if msg.contains("decompression ratio")
        );
    }

    #[test]
    fn test_tar_file_count_limit_exceeded() {
        let temp_dir = tempdir().unwrap();
        let tar_gz_path = temp_dir.child("many-files.tar.gz");

        let file = File::create(&tar_gz_path).unwrap();
        let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
        let mut tar = tar::Builder::new(encoder);

        for i in 0..5 {
            let mut header = tar::Header::new_gnu();
            let data = b"content";
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            tar.append_data(&mut header, format!("file{i}.txt"), &data[..])
                .unwrap();
        }
        tar.into_inner().unwrap().finish().unwrap();

        let extract_dir = temp_dir.child("extract");
        extract_dir.create_dir_all().unwrap();

        let limits = ExtractionLimits {
            max_file_count: 3,
            ..Default::default()
        };

        let result = unpack_with_limits(&tar_gz_path, &extract_dir, &limits);
        assert_matches!(
            result,
            Err(ExtractError::LimitExceeded(msg)) if msg.contains("file count")
        );
    }

    #[test]
    fn test_tar_individual_file_size_limit_exceeded() {
        let temp_dir = tempdir().unwrap();
        let tar_gz_path = temp_dir.child("large-file.tar.gz");

        let file = File::create(&tar_gz_path).unwrap();
        let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
        let mut tar = tar::Builder::new(encoder);

        let mut header = tar::Header::new_gnu();
        let large_data = vec![b'x'; 2000];
        header.set_size(large_data.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        tar.append_data(&mut header, "large.txt", &large_data[..])
            .unwrap();
        tar.into_inner().unwrap().finish().unwrap();

        let extract_dir = temp_dir.child("extract");
        extract_dir.create_dir_all().unwrap();

        let limits = ExtractionLimits {
            max_individual_file_bytes: 1000,
            ..Default::default()
        };

        let result = unpack_with_limits(&tar_gz_path, &extract_dir, &limits);
        assert_matches!(
            result,
            Err(ExtractError::LimitExceeded(msg)) if msg.contains("individual file size")
        );
    }

    #[test]
    fn test_tar_total_bytes_limit_exceeded() {
        let temp_dir = tempdir().unwrap();
        let tar_gz_path = temp_dir.child("many-files.tar.gz");

        let file = File::create(&tar_gz_path).unwrap();
        let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
        let mut tar = tar::Builder::new(encoder);

        let data = vec![b'x'; 500];
        for i in 0..5 {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            tar.append_data(&mut header, format!("file{i}.txt"), &data[..])
                .unwrap();
        }
        tar.into_inner().unwrap().finish().unwrap();

        let extract_dir = temp_dir.child("extract");
        extract_dir.create_dir_all().unwrap();

        let limits = ExtractionLimits {
            max_total_extracted_bytes: 2000,
            ..Default::default()
        };

        let result = unpack_with_limits(&tar_gz_path, &extract_dir, &limits);
        assert_matches!(
            result,
            Err(ExtractError::LimitExceeded(msg)) if msg.contains("total extracted bytes")
        );
    }
}

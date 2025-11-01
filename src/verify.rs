use std::{collections::HashMap, fs::File, io};

use camino::Utf8Path;
use sha2::{Digest, Sha256};
use thiserror::Error;

const SHA256_HEX_LENGTH: usize = 64;
const MIN_LINE_LENGTH: usize = SHA256_HEX_LENGTH + 2;

#[derive(Debug, Error)]
pub enum VerifyError {
    #[error("failed to parse checksum text: {0}")]
    ParseError(String),

    #[error("asset '{0}' not found in checksum file")]
    NotFound(String),

    #[error("checksum mismatch for '{filename}': expected {expected}, got {actual}")]
    Mismatch {
        filename: String,
        expected: String,
        actual: String,
    },

    #[error("I/O error: {0}")]
    Io(#[from] io::Error),

    #[error("HTTP request failed: {0}")]
    Request(#[from] reqwest::Error),
}

pub type Result<T> = std::result::Result<T, VerifyError>;

/// Parses SHA256SUMS format text into a list of (hex, filename) pairs.
///
/// Supports both `<hex>  <filename>` and `<hex> *<filename>` formats.
///
/// # Errors
///
/// Returns `VerifyError::ParseError` if:
/// - A line is too short to contain a 64-char hex string and filename
/// - The hex string contains non-hexadecimal characters
/// - The separator after the hex is not `  ` (two spaces) or ` *` (space-asterisk)
/// - A filename is empty
pub fn parse_checksum_text(s: &str) -> Result<Vec<(String, String)>> {
    let mut result = Vec::new();

    for raw_line in s.lines() {
        let line = raw_line.trim_end_matches('\r');
        let line = line.trim_start();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        if line.len() < MIN_LINE_LENGTH {
            return Err(VerifyError::ParseError(format!(
                "line too short to contain checksum and filename: {line}"
            )));
        }

        let (hex, rest) = line.split_at(SHA256_HEX_LENGTH);

        if !hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(VerifyError::ParseError(format!(
                "invalid hex characters in checksum: {hex}"
            )));
        }

        let filename = if let Some(filename) = rest.strip_prefix("  ") {
            filename
        } else if let Some(filename) = rest.strip_prefix(" *") {
            filename
        } else {
            return Err(VerifyError::ParseError(format!(
                "invalid separator after hex: expected '  ' or ' *', got: {rest}"
            )));
        };

        if filename.is_empty() {
            return Err(VerifyError::ParseError(format!(
                "missing filename in line: {line}"
            )));
        }

        result.push((hex.to_string(), filename.to_string()));
    }

    Ok(result)
}

/// Fetches a checksum file from a URL and verifies a local file against it.
///
/// Downloads the checksum file (e.g., SHA256SUMS), finds the entry matching
/// `asset_filename`, computes the SHA256 hash of the file at `downloaded_path`,
/// and compares them.
///
/// # Errors
///
/// Returns an error if:
/// - `VerifyError::Request` - HTTP request fails, times out, or returns non-2xx status
/// - `VerifyError::ParseError` - Checksum file format is invalid
/// - `VerifyError::NotFound` - `asset_filename` is not found in the checksum file
/// - `VerifyError::Mismatch` - Computed hash does not match expected hash
/// - `VerifyError::Io` - File reading fails
pub async fn fetch_and_verify_checksum(
    asset_filename: &str,
    checksum_url: &str,
    token: Option<&str>,
    client: reqwest::Client,
    downloaded_path: &Utf8Path,
) -> Result<()> {
    let mut request = client.get(checksum_url);

    if let Some(token) = token {
        request = request.bearer_auth(token);
    }

    let response = request.send().await?.error_for_status()?;
    let checksum_text = response.text().await?;

    let checksums: HashMap<_, _> = parse_checksum_text(&checksum_text)?
        .into_iter()
        .map(|(hex, filename)| (filename, hex))
        .collect();

    let expected_hex = checksums
        .get(asset_filename)
        .ok_or_else(|| VerifyError::NotFound(asset_filename.to_string()))?;

    let path = downloaded_path.to_owned();
    let actual_hex = tokio::task::spawn_blocking(move || {
        let mut file = File::open(&path)?;
        let mut hasher = Sha256::new();
        io::copy(&mut file, &mut hasher)?;
        let actual_hash = hasher.finalize();
        Ok::<String, io::Error>(format!("{actual_hash:x}"))
    })
    .await
    .map_err(io::Error::other)??;

    if !actual_hex.eq_ignore_ascii_case(expected_hex) {
        return Err(VerifyError::Mismatch {
            filename: asset_filename.to_string(),
            expected: expected_hex.clone(),
            actual: actual_hex,
        });
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use camino_tempfile::tempdir;
    use camino_tempfile_ext::prelude::*;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{header, method, path},
    };

    use super::*;

    #[test]
    fn test_parse_valid_two_space_format() {
        let input = "a".repeat(64) + "  file.tar.gz";
        let result = parse_checksum_text(&input).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].0, "a".repeat(64));
        assert_eq!(result[0].1, "file.tar.gz");
    }

    #[test]
    fn test_parse_valid_asterisk_format() {
        let input = "b".repeat(64) + " *binary.zip";
        let result = parse_checksum_text(&input).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].0, "b".repeat(64));
        assert_eq!(result[0].1, "binary.zip");
    }

    #[test]
    fn test_parse_mixed_formats() {
        let input = format!(
            "{}\n{}",
            "a".repeat(64) + "  file1.tar.gz",
            "b".repeat(64) + " *file2.zip"
        );
        let result = parse_checksum_text(&input).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].0, "a".repeat(64));
        assert_eq!(result[0].1, "file1.tar.gz");
        assert_eq!(result[1].0, "b".repeat(64));
        assert_eq!(result[1].1, "file2.zip");
    }

    #[test]
    fn test_parse_with_empty_lines() {
        let input = format!(
            "{}\n\n{}",
            "a".repeat(64) + "  file.tar.gz",
            "b".repeat(64) + " *binary.zip"
        );
        let result = parse_checksum_text(&input).unwrap();
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn test_parse_malformed_short_hex() {
        let input = "abc123  file.tar.gz";
        let result = parse_checksum_text(input);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), VerifyError::ParseError(_)));
    }

    #[test]
    fn test_parse_malformed_invalid_separator() {
        let input = "a".repeat(64) + " file.tar.gz";
        let result = parse_checksum_text(&input);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_malformed_no_filename() {
        let input = "a".repeat(64);
        let result = parse_checksum_text(&input);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_empty_string() {
        let result = parse_checksum_text("").unwrap();
        assert_eq!(result.len(), 0);
    }

    #[test]
    fn test_parse_ignores_comment_lines_and_leading_ws() {
        let input = format!("   # comment line\r\n{}  file.tar.gz\r\n", "a".repeat(64));
        let result = parse_checksum_text(&input).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].1, "file.tar.gz");
    }

    #[test]
    fn test_parse_crlf_endings() {
        let input = format!("{}  win.bin\r\n", "b".repeat(64));
        let result = parse_checksum_text(&input).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].0, "b".repeat(64));
        assert_eq!(result[0].1, "win.bin");
    }

    #[test]
    fn test_parse_with_comments() {
        let input = format!(
            "# This is a comment\n{}\n # Another comment\n{}",
            "a".repeat(64) + "  file1.tar.gz",
            "b".repeat(64) + "  file2.tar.gz"
        );
        let result = parse_checksum_text(&input).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].0, "a".repeat(64));
        assert_eq!(result[0].1, "file1.tar.gz");
        assert_eq!(result[1].0, "b".repeat(64));
        assert_eq!(result[1].1, "file2.tar.gz");
    }

    #[test]
    fn test_parse_with_crlf_line_endings() {
        let input = format!(
            "{}\r\n{}",
            "a".repeat(64) + "  file1.tar.gz",
            "b".repeat(64) + " *file2.zip"
        );
        let result = parse_checksum_text(&input).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].0, "a".repeat(64));
        assert_eq!(result[0].1, "file1.tar.gz");
        assert_eq!(result[1].0, "b".repeat(64));
        assert_eq!(result[1].1, "file2.zip");
    }

    #[test]
    fn test_parse_mixed_comments_and_crlf() {
        let input = format!(
            "# Header comment\r\n{}\r\n # Middle comment\r\n{}",
            "a".repeat(64) + "  file.tar.gz",
            "b".repeat(64) + " *binary.zip"
        );
        let result = parse_checksum_text(&input).unwrap();
        assert_eq!(result.len(), 2);
    }

    #[tokio::test]
    async fn test_fetch_and_verify_happy_path() {
        let temp_dir = tempdir().unwrap();
        let file_path = temp_dir.child("test-asset.tar.gz");
        file_path.write_binary(b"test content").unwrap();

        let expected_hash = "6ae8a75555209fd6c44157c0aed8016e763ff435a19cf186f76863140143ff72";
        let checksum_content = format!("{expected_hash}  test-asset.tar.gz");

        let mock_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/checksums.txt"))
            .respond_with(ResponseTemplate::new(200).set_body_string(checksum_content))
            .expect(1)
            .mount(&mock_server)
            .await;

        let client = reqwest::Client::new();
        let checksum_url = format!("{}/checksums.txt", mock_server.uri());
        let result =
            fetch_and_verify_checksum("test-asset.tar.gz", &checksum_url, None, client, &file_path)
                .await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_fetch_and_verify_with_token() {
        let temp_dir = tempdir().unwrap();
        let file_path = temp_dir.child("asset.zip");
        file_path.write_binary(b"test content").unwrap();

        let expected_hash = "6ae8a75555209fd6c44157c0aed8016e763ff435a19cf186f76863140143ff72";
        let checksum_content = format!("{expected_hash} *asset.zip");

        let mock_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/checksums.txt"))
            .and(header("authorization", "Bearer test-token"))
            .respond_with(ResponseTemplate::new(200).set_body_string(checksum_content))
            .expect(1)
            .mount(&mock_server)
            .await;

        let client = reqwest::Client::new();
        let checksum_url = format!("{}/checksums.txt", mock_server.uri());
        let result = fetch_and_verify_checksum(
            "asset.zip",
            &checksum_url,
            Some("test-token"),
            client,
            &file_path,
        )
        .await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_fetch_and_verify_filename_not_found() {
        let temp_dir = tempdir().unwrap();
        let file_path = temp_dir.child("missing.tar.gz");
        file_path.write_binary(b"test content").unwrap();

        let checksum_content = format!("{}  other-file.tar.gz", "a".repeat(64));

        let mock_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/checksums.txt"))
            .respond_with(ResponseTemplate::new(200).set_body_string(checksum_content))
            .mount(&mock_server)
            .await;

        let client = reqwest::Client::new();
        let checksum_url = format!("{}/checksums.txt", mock_server.uri());
        let result =
            fetch_and_verify_checksum("missing.tar.gz", &checksum_url, None, client, &file_path)
                .await;

        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), VerifyError::NotFound(_)));
    }

    #[tokio::test]
    async fn test_fetch_and_verify_hash_mismatch() {
        let temp_dir = tempdir().unwrap();
        let file_path = temp_dir.child("bad-hash.tar.gz");
        file_path.write_binary(b"test content").unwrap();

        let wrong_hash = "f".repeat(64);
        let checksum_content = format!("{wrong_hash}  bad-hash.tar.gz");

        let mock_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/checksums.txt"))
            .respond_with(ResponseTemplate::new(200).set_body_string(checksum_content))
            .mount(&mock_server)
            .await;

        let client = reqwest::Client::new();
        let checksum_url = format!("{}/checksums.txt", mock_server.uri());
        let result =
            fetch_and_verify_checksum("bad-hash.tar.gz", &checksum_url, None, client, &file_path)
                .await;

        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), VerifyError::Mismatch { .. }));
    }

    #[tokio::test]
    async fn test_fetch_and_verify_http_error() {
        let temp_dir = tempdir().unwrap();
        let file_path = temp_dir.child("asset.tar.gz");
        file_path.write_binary(b"test content").unwrap();

        let mock_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/checksums.txt"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&mock_server)
            .await;

        let client = reqwest::Client::new();
        let checksum_url = format!("{}/checksums.txt", mock_server.uri());
        let result =
            fetch_and_verify_checksum("asset.tar.gz", &checksum_url, None, client, &file_path)
                .await;

        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), VerifyError::Request(_)));
    }
}

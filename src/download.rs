use std::io::Write;

use camino_tempfile::NamedUtf8TempFile;
use futures_util::StreamExt;
use reqwest_middleware::{ClientBuilder, ClientWithMiddleware};
use reqwest_retry::{RetryTransientMiddleware, policies::ExponentialBackoff};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum DownloadError {
    #[error("HTTP request error: {0}")]
    Request(#[from] reqwest::Error),

    #[error("HTTP middleware error: {0}")]
    Middleware(#[from] reqwest_middleware::Error),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Insecure URL: Authorization header cannot be sent over non-HTTPS connection")]
    InsecureUrl,
}

pub type Result<T> = std::result::Result<T, DownloadError>;

const MAX_RETRIES: u32 = 3;

/// Downloads a file from the specified URL with automatic retry on transient failures.
///
/// Streams the response body to a temporary file and ensures data is fsynced before returning.
/// Uses exponential backoff retry strategy for transient HTTP errors (5xx status codes).
///
/// # Security
///
/// By default, this function enforces HTTPS to prevent sending the Authorization header
/// over unencrypted connections. Set `allow_insecure` to `true` only for testing or
/// development environments where HTTP is acceptable.
///
/// # Errors
///
/// Returns `DownloadError` if:
/// - The URL scheme is not HTTPS (unless `allow_insecure` is true)
/// - The HTTP request fails after all retries
/// - The server returns a non-success status code
/// - Writing to the temporary file fails
/// - Fsyncing the file fails
pub async fn fetch(url: &str, token: &str, allow_insecure: bool) -> Result<NamedUtf8TempFile> {
    if !allow_insecure && !url.starts_with("https://") {
        return Err(DownloadError::InsecureUrl);
    }

    let retry_policy = ExponentialBackoff::builder().build_with_max_retries(MAX_RETRIES);
    let retry_middleware = RetryTransientMiddleware::new_with_policy(retry_policy);

    let client: ClientWithMiddleware = ClientBuilder::new(reqwest::Client::new())
        .with(retry_middleware)
        .build();

    let response = client
        .get(url)
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await?
        .error_for_status()?;

    let mut temp_file = NamedUtf8TempFile::new()?;
    let mut stream = response.bytes_stream();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        temp_file.write_all(&chunk)?;
    }

    temp_file.as_file().sync_all()?;

    Ok(temp_file)
}

#[cfg(test)]
mod tests {
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };

    use super::*;

    #[tokio::test]
    async fn test_retry_on_server_errors() {
        let mock_server = MockServer::start().await;
        let body_content = b"success payload";

        Mock::given(method("GET"))
            .and(path("/asset.tar.gz"))
            .respond_with(ResponseTemplate::new(500).set_body_string("Internal Server Error"))
            .up_to_n_times(2)
            .mount(&mock_server)
            .await;

        Mock::given(method("GET"))
            .and(path("/asset.tar.gz"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body_content))
            .expect(1)
            .mount(&mock_server)
            .await;

        let url = format!("{}/asset.tar.gz", mock_server.uri());
        let result = fetch(&url, "test-token", true).await;

        assert!(result.is_ok());
        let temp_file = result.unwrap();
        let contents = std::fs::read(temp_file.path()).unwrap();
        assert_eq!(contents, body_content);
    }

    #[tokio::test]
    async fn test_downloads_without_content_length() {
        let mock_server = MockServer::start().await;
        let body_content = b"chunked content without length header";

        Mock::given(method("GET"))
            .and(path("/asset.tar.gz"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body_content))
            .expect(1)
            .mount(&mock_server)
            .await;

        let url = format!("{}/asset.tar.gz", mock_server.uri());
        let result = fetch(&url, "test-token", true).await;

        assert!(result.is_ok());
        let temp_file = result.unwrap();
        let contents = std::fs::read(temp_file.path()).unwrap();
        assert_eq!(contents, body_content);
    }

    #[tokio::test]
    async fn test_sends_authorization_header() {
        use wiremock::matchers::header;

        let mock_server = MockServer::start().await;
        let test_token = "test-secret-token";
        let body_content = b"authenticated response";

        Mock::given(method("GET"))
            .and(path("/asset.tar.gz"))
            .and(header("Authorization", format!("Bearer {test_token}")))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body_content))
            .expect(1)
            .mount(&mock_server)
            .await;

        let url = format!("{}/asset.tar.gz", mock_server.uri());
        let result = fetch(&url, test_token, true).await;

        assert!(result.is_ok());
        let temp_file = result.unwrap();
        let contents = std::fs::read(temp_file.path()).unwrap();
        assert_eq!(contents, body_content);
    }

    #[tokio::test]
    async fn test_rejects_non_https_urls() {
        let result = fetch("http://example.com/file.tar.gz", "secret-token", false).await;

        assert!(result.is_err());
        match result {
            Err(DownloadError::InsecureUrl) => {}
            other => panic!("Expected InsecureUrl error, got: {:?}", other),
        }
    }
}

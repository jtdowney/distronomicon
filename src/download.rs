use std::io::Write;

use camino_tempfile::NamedUtf8TempFile;
use futures_util::StreamExt;
use reqwest_middleware::{ClientBuilder, ClientWithMiddleware};
use reqwest_retry::{RetryTransientMiddleware, policies::ExponentialBackoff};
use thiserror::Error;

use crate::DEFAULT_TIMEOUT;

#[derive(Debug, Error)]
pub enum DownloadError {
    #[error("HTTP request error: {0}")]
    Request(#[from] reqwest::Error),

    #[error("HTTP middleware error: {0}")]
    Middleware(#[from] reqwest_middleware::Error),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, DownloadError>;

const MAX_RETRIES: u32 = 3;

#[bon::builder(derive(IntoFuture(Box)))]
pub async fn fetch(
    url: &str,
    token: Option<&str>,
    #[builder(default = crate::build_http_client(DEFAULT_TIMEOUT).unwrap())]
    client: reqwest::Client,
    #[builder(default = MAX_RETRIES)] max_retries: u32,
    retry_base: Option<u32>,
) -> Result<NamedUtf8TempFile> {
    let mut retry_builder = ExponentialBackoff::builder();
    if let Some(base) = retry_base {
        retry_builder = retry_builder.base(base);
    }
    let retry_policy = retry_builder.build_with_max_retries(max_retries);
    let retry_middleware = RetryTransientMiddleware::new_with_policy(retry_policy);

    let client_with_middleware: ClientWithMiddleware = ClientBuilder::new(client.clone())
        .with(retry_middleware)
        .build();

    let mut request = client_with_middleware.get(url);
    if let Some(token) = token {
        request = request.header("Authorization", format!("Bearer {token}"));
    }

    let response = request.send().await?.error_for_status()?;

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
    use std::{fs, time::Duration};

    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{header, method, path},
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
        let result = fetch().url(&url).token("test-token").retry_base(1).await;

        assert!(result.is_ok());

        let temp_file = result.unwrap();
        let contents = fs::read(temp_file.path()).unwrap();
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
        let result = fetch().url(&url).token("test-token").await;

        assert!(result.is_ok());

        let temp_file = result.unwrap();
        let contents = fs::read(temp_file.path()).unwrap();
        assert_eq!(contents, body_content);
    }

    #[tokio::test]
    async fn test_sends_authorization_header() {
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
        let result = fetch().url(&url).token(test_token).await;

        assert!(result.is_ok());

        let temp_file = result.unwrap();
        let contents = fs::read(temp_file.path()).unwrap();
        assert_eq!(contents, body_content);
    }

    #[tokio::test]
    async fn test_request_completes_within_timeout() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/slow.tar.gz"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_bytes(b"test data")
                    .set_delay(Duration::from_millis(500)),
            )
            .expect(1)
            .mount(&mock_server)
            .await;

        let url = format!("{}/slow.tar.gz", mock_server.uri());
        let result = fetch().url(&url).token("test-token").await;

        assert!(result.is_ok());

        let temp_file = result.unwrap();
        let contents = fs::read(temp_file.path()).unwrap();
        assert_eq!(contents, b"test data");
    }

    #[tokio::test]
    async fn test_fails_after_max_retries() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/asset.tar.gz"))
            .respond_with(ResponseTemplate::new(503).set_body_string("Service Unavailable"))
            .expect(4)
            .mount(&mock_server)
            .await;

        let url = format!("{}/asset.tar.gz", mock_server.uri());
        let result = fetch().url(&url).token("test-token").retry_base(1).await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_does_not_retry_client_errors() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/asset.tar.gz"))
            .respond_with(ResponseTemplate::new(404).set_body_string("Not Found"))
            .expect(1)
            .mount(&mock_server)
            .await;

        let url = format!("{}/asset.tar.gz", mock_server.uri());
        let result = fetch().url(&url).token("test-token").await;

        assert!(result.is_err());
    }
}

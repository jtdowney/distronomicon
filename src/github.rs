use anyhow::Result;
use jiff::Timestamp;
use regex::Regex;
use reqwest::{
    header::{ACCEPT, AUTHORIZATION, ETAG, IF_MODIFIED_SINCE, IF_NONE_MATCH, LAST_MODIFIED},
    StatusCode,
};
use serde::Deserialize;

use crate::{DEFAULT_GITHUB_HOST, DEFAULT_TIMEOUT};

#[derive(Debug, Clone, Deserialize)]
pub struct Release {
    pub tag_name: String,
    pub assets: Vec<Asset>,
    pub prerelease: bool,
    #[serde(default)]
    pub draft: bool,
    #[serde(default)]
    pub created_at: Option<Timestamp>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Asset {
    pub name: String,
    pub browser_download_url: String,
    pub size: u64,
}

#[derive(Debug, Clone, Default)]
pub struct Validators {
    pub etag: Option<String>,
    pub last_modified: Option<String>,
}

#[derive(Debug)]
pub struct ValidatorsOut {
    pub etag: Option<String>,
    pub last_modified: Option<String>,
}

#[derive(Debug)]
pub struct FetchResult {
    pub release: Option<Release>,
    pub validators: ValidatorsOut,
    pub was_modified: bool,
}

/// Fetches the latest release from GitHub.
///
/// Uses conditional requests via `ETag` and `Last-Modified` headers when validators
/// are provided. Returns an optional release (None on 304), updated validators, and
/// whether content changed.
///
/// # Errors
///
/// Returns an error if:
/// - Network request fails
/// - Response cannot be parsed as JSON
/// - No releases are found when `allow_prerelease` is true
#[bon::builder(derive(IntoFuture(Box)))]
pub async fn fetch_latest(
    repo: &str,
    token: Option<&str>,
    #[builder(default = crate::build_http_client(DEFAULT_TIMEOUT).unwrap())]
    client: reqwest::Client,
    #[builder(default = DEFAULT_GITHUB_HOST)] host: &str,
    #[builder(default = false)] allow_prerelease: bool,
    #[builder(default)] validators: Validators,
) -> Result<FetchResult> {
    let url = if allow_prerelease {
        format!("{host}/repos/{repo}/releases")
    } else {
        format!("{host}/repos/{repo}/releases/latest")
    };

    let mut request = client
        .get(&url)
        .header(ACCEPT, "application/vnd.github+json");

    if let Some(token) = token {
        request = request.header(AUTHORIZATION, format!("Bearer {token}"));
    }

    if let Some(etag) = &validators.etag {
        request = request.header(IF_NONE_MATCH, etag);
    }
    if let Some(last_modified) = &validators.last_modified {
        request = request.header(IF_MODIFIED_SINCE, last_modified);
    }

    let response = request.send().await?;
    let status = response.status();
    let headers = response.headers();
    let validators_out = ValidatorsOut {
        etag: headers
            .get(ETAG)
            .and_then(|h| h.to_str().ok())
            .map(String::from),
        last_modified: headers
            .get(LAST_MODIFIED)
            .and_then(|h| h.to_str().ok())
            .map(String::from),
    };

    if status == StatusCode::NOT_MODIFIED {
        return Ok(FetchResult {
            release: None,
            validators: validators_out,
            was_modified: false,
        });
    }

    let response = response.error_for_status()?;

    let release = if allow_prerelease {
        let mut releases = response.json::<Vec<Release>>().await?;
        releases.retain(|r| !r.draft);
        releases.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        releases
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("No releases found"))?
    } else {
        response.json::<Release>().await?
    };

    Ok(FetchResult {
        release: Some(release),
        validators: validators_out,
        was_modified: true,
    })
}

#[must_use]
pub fn select_asset<'a>(assets: &'a [Asset], pattern: &Regex) -> Option<&'a Asset> {
    assets.iter().find(|asset| pattern.is_match(&asset.name))
}

#[cfg(test)]
mod tests {
    use wiremock::{
        matchers::{header, header_exists, method, path},
        Mock, MockServer, ResponseTemplate,
    };

    use super::*;

    #[tokio::test]
    async fn test_fetch_latest_returns_release_with_etag() {
        let mock_server = MockServer::start().await;

        let release_json = serde_json::json!({
            "tag_name": "v0.1.3",
            "prerelease": false,
            "assets": [
                {
                    "name": "app-linux-amd64.tar.gz",
                    "browser_download_url": "https://github.com/owner/repo/releases/download/v0.1.3/app-linux-amd64.tar.gz",
                    "size": 1024
                }
            ]
        });

        Mock::given(method("GET"))
            .and(path("/repos/owner/repo/releases/latest"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(&release_json)
                    .insert_header("etag", "\"abc123\"")
                    .insert_header("last-modified", "Mon, 27 Oct 2025 12:00:00 GMT"),
            )
            .mount(&mock_server)
            .await;

        let result = fetch_latest()
            .repo("owner/repo")
            .host(&mock_server.uri())
            .await;

        assert!(result.is_ok());
        let fetch_result = result.unwrap();

        assert!(fetch_result.release.is_some());
        let release = fetch_result.release.unwrap();
        assert_eq!(release.tag_name, "v0.1.3");
        assert!(!release.prerelease);
        assert_eq!(release.assets.len(), 1);
        assert_eq!(release.assets[0].name, "app-linux-amd64.tar.gz");
        assert_eq!(release.assets[0].size, 1024);

        assert_eq!(fetch_result.validators.etag, Some("\"abc123\"".to_string()));
        assert_eq!(
            fetch_result.validators.last_modified,
            Some("Mon, 27 Oct 2025 12:00:00 GMT".to_string())
        );
        assert!(fetch_result.was_modified);
    }

    #[tokio::test]
    async fn test_fetch_latest_returns_not_modified_on_304() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/repos/owner/repo/releases/latest"))
            .respond_with(
                ResponseTemplate::new(304)
                    .insert_header("etag", "\"abc123\"")
                    .insert_header("last-modified", "Mon, 27 Oct 2025 12:00:00 GMT"),
            )
            .mount(&mock_server)
            .await;

        let validators = Validators {
            etag: Some("\"abc123\"".to_string()),
            last_modified: Some("Mon, 27 Oct 2025 12:00:00 GMT".to_string()),
        };

        let result = fetch_latest()
            .repo("owner/repo")
            .host(&mock_server.uri())
            .validators(validators)
            .await;

        assert!(result.is_ok());
        let fetch_result = result.unwrap();

        assert!(fetch_result.release.is_none());
        assert_eq!(fetch_result.validators.etag, Some("\"abc123\"".to_string()));
        assert_eq!(
            fetch_result.validators.last_modified,
            Some("Mon, 27 Oct 2025 12:00:00 GMT".to_string())
        );
        assert!(!fetch_result.was_modified);
    }

    #[tokio::test]
    async fn test_fetch_latest_sends_validators_in_request() {
        let mock_server = MockServer::start().await;

        let release_json = serde_json::json!({
            "tag_name": "v0.1.0",
            "prerelease": false,
            "assets": []
        });

        Mock::given(method("GET"))
            .and(path("/repos/owner/repo/releases/latest"))
            .and(header_exists("if-none-match"))
            .and(header_exists("if-modified-since"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&release_json))
            .expect(1)
            .mount(&mock_server)
            .await;

        let validators = Validators {
            etag: Some("\"etag-value\"".to_string()),
            last_modified: Some("Wed, 21 Oct 2015 07:28:00 GMT".to_string()),
        };

        let result = fetch_latest()
            .repo("owner/repo")
            .host(&mock_server.uri())
            .validators(validators)
            .await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_fetch_latest_selects_prerelease_when_newer() {
        let mock_server = MockServer::start().await;

        let releases_json = serde_json::json!([
            {
                "tag_name": "v0.2.0-beta.1",
                "prerelease": true,
                "created_at": "2025-10-27T12:00:00Z",
                "assets": [
                    {
                        "name": "app-beta.tar.gz",
                        "browser_download_url": "https://github.com/owner/repo/releases/download/v0.2.0-beta.1/app-beta.tar.gz",
                        "size": 2048
                    }
                ]
            },
            {
                "tag_name": "v0.1.5",
                "prerelease": false,
                "created_at": "2025-10-20T12:00:00Z",
                "assets": [
                    {
                        "name": "app-stable.tar.gz",
                        "browser_download_url": "https://github.com/owner/repo/releases/download/v0.1.5/app-stable.tar.gz",
                        "size": 1536
                    }
                ]
            }
        ]);

        Mock::given(method("GET"))
            .and(path("/repos/owner/repo/releases"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(&releases_json)
                    .insert_header("etag", "\"xyz789\""),
            )
            .mount(&mock_server)
            .await;

        let result = fetch_latest()
            .repo("owner/repo")
            .host(&mock_server.uri())
            .allow_prerelease(true)
            .await;

        assert!(result.is_ok());
        let fetch_result = result.unwrap();

        assert!(fetch_result.release.is_some());
        let release = fetch_result.release.unwrap();
        assert_eq!(release.tag_name, "v0.2.0-beta.1");
        assert!(release.prerelease);
        assert_eq!(release.assets[0].name, "app-beta.tar.gz");

        assert_eq!(fetch_result.validators.etag, Some("\"xyz789\"".to_string()));
        assert!(fetch_result.was_modified);
    }

    #[tokio::test]
    async fn test_fetch_latest_includes_bearer_token_when_provided() {
        let mock_server = MockServer::start().await;

        let release_json = serde_json::json!({
            "tag_name": "v0.1.0",
            "prerelease": false,
            "assets": []
        });

        Mock::given(method("GET"))
            .and(path("/repos/owner/repo/releases/latest"))
            .and(header("Authorization", "Bearer secret-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&release_json))
            .expect(1)
            .mount(&mock_server)
            .await;

        let result = fetch_latest()
            .repo("owner/repo")
            .token("secret-token")
            .host(&mock_server.uri())
            .await;

        assert!(result.is_ok());
        let fetch_result = result.unwrap();
        assert!(fetch_result.release.is_some());
        assert_eq!(fetch_result.release.unwrap().tag_name, "v0.1.0");
    }

    #[tokio::test]
    async fn test_fetch_latest_no_auth_header_when_token_absent() {
        let mock_server = MockServer::start().await;

        let release_json = serde_json::json!({
            "tag_name": "v0.1.0",
            "prerelease": false,
            "assets": []
        });

        Mock::given(method("GET"))
            .and(path("/repos/owner/repo/releases/latest"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&release_json))
            .mount(&mock_server)
            .await;

        let result = fetch_latest()
            .repo("owner/repo")
            .host(&mock_server.uri())
            .await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_fetch_latest_skips_draft_releases() {
        let mock_server = MockServer::start().await;

        let releases_json = serde_json::json!([
            {
                "tag_name": "v0.3.0",
                "prerelease": false,
                "draft": true,
                "created_at": "2025-10-28T12:00:00Z",
                "assets": [
                    {
                        "name": "app-draft.tar.gz",
                        "browser_download_url": "https://github.com/owner/repo/releases/download/v0.3.0/app-draft.tar.gz",
                        "size": 3072
                    }
                ]
            },
            {
                "tag_name": "v0.2.0",
                "prerelease": false,
                "draft": false,
                "created_at": "2025-10-27T12:00:00Z",
                "assets": [
                    {
                        "name": "app-stable.tar.gz",
                        "browser_download_url": "https://github.com/owner/repo/releases/download/v0.2.0/app-stable.tar.gz",
                        "size": 2048
                    }
                ]
            }
        ]);

        Mock::given(method("GET"))
            .and(path("/repos/owner/repo/releases"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(&releases_json)
                    .insert_header("etag", "\"draft789\""),
            )
            .mount(&mock_server)
            .await;

        let result = fetch_latest()
            .repo("owner/repo")
            .host(&mock_server.uri())
            .allow_prerelease(true)
            .await;

        assert!(result.is_ok());
        let fetch_result = result.unwrap();

        assert!(fetch_result.release.is_some());
        let release = fetch_result.release.unwrap();
        assert_eq!(release.tag_name, "v0.2.0");
        assert!(!release.draft);
        assert_eq!(release.assets[0].name, "app-stable.tar.gz");
    }

    #[tokio::test]
    async fn test_fetch_latest_returns_error_for_404() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/repos/owner/repo/releases/latest"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&mock_server)
            .await;

        let result = fetch_latest()
            .repo("owner/repo")
            .host(&mock_server.uri())
            .await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("404"));
    }

    #[tokio::test]
    async fn test_fetch_latest_returns_error_for_403() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/repos/owner/repo/releases/latest"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&mock_server)
            .await;

        let result = fetch_latest()
            .repo("owner/repo")
            .host(&mock_server.uri())
            .await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("403"));
    }

    #[test]
    fn test_select_asset_returns_first_match() {
        let assets = vec![
            Asset {
                name: "app-linux-amd64.tar.gz".to_string(),
                browser_download_url: "https://example.com/app-linux-amd64.tar.gz".to_string(),
                size: 1024,
            },
            Asset {
                name: "app-darwin-amd64.tar.gz".to_string(),
                browser_download_url: "https://example.com/app-darwin-amd64.tar.gz".to_string(),
                size: 2048,
            },
            Asset {
                name: "app-linux-arm64.tar.gz".to_string(),
                browser_download_url: "https://example.com/app-linux-arm64.tar.gz".to_string(),
                size: 3072,
            },
        ];

        let pattern = Regex::new(r"app-linux-.*\.tar\.gz").unwrap();
        let result = select_asset(&assets, &pattern);

        assert!(result.is_some());
        assert_eq!(result.unwrap().name, "app-linux-amd64.tar.gz");
    }

    #[test]
    fn test_select_asset_returns_none_when_no_match() {
        let assets = vec![
            Asset {
                name: "app-darwin-amd64.tar.gz".to_string(),
                browser_download_url: "https://example.com/app-darwin-amd64.tar.gz".to_string(),
                size: 1024,
            },
            Asset {
                name: "app-windows-amd64.zip".to_string(),
                browser_download_url: "https://example.com/app-windows-amd64.zip".to_string(),
                size: 2048,
            },
        ];

        let pattern = Regex::new(r"app-linux-.*\.tar\.gz").unwrap();
        let result = select_asset(&assets, &pattern);

        assert!(result.is_none());
    }

    #[test]
    fn test_select_asset_returns_first_when_multiple_matches() {
        let assets = vec![
            Asset {
                name: "checksums.txt".to_string(),
                browser_download_url: "https://example.com/checksums.txt".to_string(),
                size: 128,
            },
            Asset {
                name: "SHA256SUMS".to_string(),
                browser_download_url: "https://example.com/SHA256SUMS".to_string(),
                size: 256,
            },
            Asset {
                name: "checksums.sha256".to_string(),
                browser_download_url: "https://example.com/checksums.sha256".to_string(),
                size: 200,
            },
        ];

        let pattern = Regex::new(r"checksum|SHA256").unwrap();
        let result = select_asset(&assets, &pattern);

        assert!(result.is_some());
        assert_eq!(result.unwrap().name, "checksums.txt");
    }
}

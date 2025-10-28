use anyhow::Result;
use jiff::Timestamp;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Release {
    pub tag_name: String,
    pub assets: Vec<Asset>,
    pub prerelease: bool,
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

#[derive(Debug, PartialEq)]
pub enum WasModified {
    Yes,
    No,
}

/// Fetches the latest release from GitHub.
///
/// Uses conditional requests via `ETag` and `Last-Modified` headers when validators
/// are provided. Returns the release, updated validators, and whether content changed.
///
/// # Errors
///
/// Returns an error if:
/// - Network request fails
/// - Response cannot be parsed as JSON
/// - No releases are found when `allow_prerelease` is true
pub async fn fetch_latest(
    repo: &str,
    token: Option<&str>,
    host: Option<&str>,
    allow_prerelease: bool,
    validators_in: &Validators,
) -> Result<(Release, ValidatorsOut, WasModified)> {
    let base_url = host.unwrap_or("https://api.github.com");
    let url = if allow_prerelease {
        format!("{base_url}/repos/{repo}/releases")
    } else {
        format!("{base_url}/repos/{repo}/releases/latest")
    };

    let client = reqwest::Client::builder()
        .user_agent("distronomicon/0.1")
        .build()?;

    let mut request = client.get(&url);

    if let Some(token) = token {
        request = request.header("Authorization", format!("Bearer {token}"));
    }

    if let Some(etag) = &validators_in.etag {
        request = request.header("If-None-Match", etag);
    }
    if let Some(last_modified) = &validators_in.last_modified {
        request = request.header("If-Modified-Since", last_modified);
    }

    let response = request.send().await?;
    let headers = response.headers();
    let validators_out = ValidatorsOut {
        etag: headers
            .get("etag")
            .and_then(|h| h.to_str().ok())
            .map(String::from),
        last_modified: headers
            .get("last-modified")
            .and_then(|h| h.to_str().ok())
            .map(String::from),
    };

    if response.status() == 304 {
        let empty_release = Release {
            tag_name: String::new(),
            assets: Vec::new(),
            prerelease: false,
            created_at: None,
        };
        return Ok((empty_release, validators_out, WasModified::No));
    }

    let release = if allow_prerelease {
        let mut releases = response.json::<Vec<Release>>().await?;
        releases.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        releases
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("No releases found"))?
    } else {
        response.json::<Release>().await?
    };

    Ok((release, validators_out, WasModified::Yes))
}

#[cfg(test)]
mod tests {
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
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

        let validators = Validators::default();
        let result = fetch_latest(
            "owner/repo",
            None,
            Some(&mock_server.uri()),
            false,
            &validators,
        )
        .await;

        assert!(result.is_ok());
        let (release, validators_out, was_modified) = result.unwrap();

        assert_eq!(release.tag_name, "v0.1.3");
        assert!(!release.prerelease);
        assert_eq!(release.assets.len(), 1);
        assert_eq!(release.assets[0].name, "app-linux-amd64.tar.gz");
        assert_eq!(release.assets[0].size, 1024);

        assert_eq!(validators_out.etag, Some("\"abc123\"".to_string()));
        assert_eq!(
            validators_out.last_modified,
            Some("Mon, 27 Oct 2025 12:00:00 GMT".to_string())
        );
        assert_eq!(was_modified, WasModified::Yes);
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

        let result = fetch_latest(
            "owner/repo",
            None,
            Some(&mock_server.uri()),
            false,
            &validators,
        )
        .await;

        assert!(result.is_ok());
        let (_release, validators_out, was_modified) = result.unwrap();

        assert_eq!(validators_out.etag, Some("\"abc123\"".to_string()));
        assert_eq!(
            validators_out.last_modified,
            Some("Mon, 27 Oct 2025 12:00:00 GMT".to_string())
        );
        assert_eq!(was_modified, WasModified::No);
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

        let validators = Validators::default();
        let result = fetch_latest(
            "owner/repo",
            None,
            Some(&mock_server.uri()),
            true,
            &validators,
        )
        .await;

        assert!(result.is_ok());
        let (release, validators_out, was_modified) = result.unwrap();

        assert_eq!(release.tag_name, "v0.2.0-beta.1");
        assert!(release.prerelease);
        assert_eq!(release.assets[0].name, "app-beta.tar.gz");

        assert_eq!(validators_out.etag, Some("\"xyz789\"".to_string()));
        assert_eq!(was_modified, WasModified::Yes);
    }

    #[tokio::test]
    async fn test_fetch_latest_includes_bearer_token_when_provided() {
        use wiremock::matchers::header;

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

        let validators = Validators::default();
        let result = fetch_latest(
            "owner/repo",
            Some("secret-token"),
            Some(&mock_server.uri()),
            false,
            &validators,
        )
        .await;

        assert!(result.is_ok());
        let (release, _, _) = result.unwrap();
        assert_eq!(release.tag_name, "v0.1.0");
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

        let validators = Validators::default();
        let result = fetch_latest(
            "owner/repo",
            None,
            Some(&mock_server.uri()),
            false,
            &validators,
        )
        .await;

        assert!(result.is_ok());
    }
}

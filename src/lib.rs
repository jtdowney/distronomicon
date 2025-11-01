pub mod cli;
pub mod download;
pub mod extract;
pub mod fsops;
pub mod github;
pub mod lock;
pub mod restart;
pub mod state;
pub mod verify;
pub mod version;

use std::time::Duration;

const DEFAULT_GITHUB_HOST: &str = "https://api.github.com";
const DEFAULT_INSTALL_ROOT: &str = "/opt";
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(300);

/// Builds a configured HTTP client with timeout and user agent.
///
/// # Errors
///
/// Returns an error if the reqwest client builder fails.
pub fn build_http_client(timeout: Duration) -> anyhow::Result<reqwest::Client> {
    let client = reqwest::Client::builder()
        .user_agent(concat!("distronomicon/", env!("CARGO_PKG_VERSION")))
        .timeout(timeout)
        .build()?;
    Ok(client)
}

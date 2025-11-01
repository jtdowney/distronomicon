use std::fs::{self, File};

use anyhow::{anyhow, ensure};
use camino::{Utf8Path, Utf8PathBuf};
use camino_tempfile::NamedUtf8TempFile;
use clap::{Parser, Subcommand};
use jiff::Timestamp;
use regex::Regex;
use tracing::{info, info_span, warn};

use crate::{
    DEFAULT_GITHUB_HOST, DEFAULT_INSTALL_ROOT, download, extract, fsops, github, lock, restart,
    state::{self, State},
    verify, version,
};

fn validate_app_name(s: &str) -> Result<String, String> {
    if s.is_empty() {
        return Err("app name cannot be empty".to_string());
    }
    if s.contains('/') {
        return Err("app name cannot contain '/'".to_string());
    }
    if s.contains('\\') {
        return Err("app name cannot contain '\\'".to_string());
    }
    if s.contains("..") {
        return Err("app name cannot contain '..'".to_string());
    }
    if s.contains('\0') {
        return Err("app name cannot contain null bytes".to_string());
    }
    Ok(s.to_string())
}

#[derive(Parser, Debug)]
pub struct Args {
    #[arg(long, value_parser = validate_app_name, help = "Application name (used for directory structure under install root)")]
    pub app: String,

    #[arg(
        long,
        env = "PREFIX",
        default_value = DEFAULT_INSTALL_ROOT,
        help = "Root directory for installations (creates <root>/<app>/{bin,releases,staging})"
    )]
    pub install_root: Utf8PathBuf,

    #[arg(
        long,
        default_value = "300",
        help = "HTTP request timeout in seconds (applies to downloads, GitHub API, checksum verification)"
    )]
    pub http_timeout: u64,

    #[arg(short, long, action = clap::ArgAction::Count, help = "Increase logging verbosity (-v for debug, -vv for trace)")]
    pub verbose: u8,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    #[command(about = "Check for updates without installing (updates cached state validators)")]
    Check(CheckArgs),

    #[command(
        about = "Update to latest release (download, verify, extract, install, and optionally restart)"
    )]
    Update(UpdateArgs),

    #[command(about = "Show currently installed version (derived from symlinks in bin directory)")]
    Version,

    #[command(about = "Forcibly remove the lock file (use with caution)")]
    Unlock(UnlockArgs),
}

#[derive(Parser, Debug)]
pub struct GitHubConfig {
    #[arg(
        long = "github-token",
        env = "GITHUB_TOKEN",
        hide_env_values = true,
        help = "GitHub API token (required for private repos or higher rate limits)"
    )]
    pub token: Option<String>,

    #[arg(
        long = "github-host",
        env = "GITHUB_HOST",
        default_value = DEFAULT_GITHUB_HOST,
        help = "GitHub API hostname (use for GitHub Enterprise)"
    )]
    pub host: String,

    #[arg(
        long = "allow-prerelease",
        help = "Include prerelease versions when checking for updates"
    )]
    pub allow_prerelease: bool,
}

#[derive(Parser, Debug)]
pub struct CheckArgs {
    #[arg(
        long,
        help = "GitHub repository in owner/repo format (e.g., 'rust-lang/rust')"
    )]
    pub repo: String,

    #[arg(
        long,
        env = "STATE_DIRECTORY",
        help = "Directory for storing state.json with ETags and timestamps"
    )]
    pub state_directory: Utf8PathBuf,

    #[command(flatten)]
    pub github: GitHubConfig,
}

#[derive(Parser, Debug)]
pub struct UpdateArgs {
    #[arg(
        long,
        help = "GitHub repository in owner/repo format (e.g., 'rust-lang/rust')"
    )]
    pub repo: String,

    #[arg(
        long,
        help = "Regex pattern to match release asset filename (e.g., '.*\\.tar\\.gz$')"
    )]
    pub pattern: String,

    #[arg(
        long,
        env = "STATE_DIRECTORY",
        help = "Directory for storing state.json with ETags and timestamps"
    )]
    pub state_directory: Utf8PathBuf,

    #[arg(
        long,
        required_unless_present = "skip_verification",
        help = "Regex pattern to match checksum file (e.g., 'SHA256SUMS'); required unless --skip-verification"
    )]
    pub checksum_pattern: Option<String>,

    #[command(flatten)]
    pub github: GitHubConfig,

    #[arg(
        long,
        help = "Shell command to execute after successful update (e.g., 'systemctl restart myapp')"
    )]
    pub restart_command: Option<String>,

    #[arg(
        long,
        default_value = "3",
        help = "Number of old releases to keep after update (older releases are pruned)"
    )]
    pub retain: u32,

    #[arg(
        long,
        help = "Skip checksum verification (not recommended; use only for testing)"
    )]
    pub skip_verification: bool,

    #[arg(
        long,
        help = "Forcibly remove lock file before starting update (use with caution)"
    )]
    pub force_unlock: bool,

    #[arg(
        long,
        default_value = "30",
        help = "Maximum seconds to wait for lock acquisition (default: 30)"
    )]
    pub lock_timeout: u64,
}

#[derive(Parser, Debug)]
pub struct UnlockArgs {
    #[arg(
        long,
        env = "STATE_DIRECTORY",
        help = "Directory containing the lock file"
    )]
    pub state_directory: Utf8PathBuf,
}

fn is_up_to_date(
    current_tag: Option<&String>,
    release_opt: Option<&github::Release>,
    existing_state: Option<&State>,
    was_modified: bool,
) -> bool {
    if !was_modified
        && let (Some(current), Some(state)) = (current_tag, existing_state)
        && *current == state.latest_tag
    {
        return true;
    }

    if let (Some(current), Some(release)) = (current_tag, release_opt)
        && *current == release.tag_name
    {
        return true;
    }

    false
}

async fn download_and_verify_asset(
    release: &github::Release,
    asset_pattern: &Regex,
    checksum_pattern: Option<&Regex>,
    github_token: Option<&str>,
    http_client: reqwest::Client,
    skip_verification: bool,
) -> anyhow::Result<(NamedUtf8TempFile, String)> {
    let asset = github::select_asset(&release.assets, asset_pattern)
        .ok_or_else(|| anyhow!("No asset matching pattern"))?;
    info!("Selected asset: {}", asset.name);

    let downloaded_file = {
        let _span = info_span!("download", url = %asset.url).entered();
        download::fetch()
            .url(&asset.url)
            .maybe_token(github_token)
            .client(http_client.clone())
            .await?
    };

    if !skip_verification && let Some(checksum_regex) = checksum_pattern {
        let _span = info_span!("verify", asset = %asset.name).entered();
        let checksum_asset = github::select_asset(&release.assets, checksum_regex)
            .ok_or_else(|| anyhow!("No checksum asset matching pattern"))?;
        verify::fetch_and_verify_checksum(
            &asset.name,
            &checksum_asset.url,
            github_token,
            http_client,
            downloaded_file.path(),
        )
        .await?;
        info!("Checksum verified");
    }

    Ok((downloaded_file, asset.name.clone()))
}

fn install_release(
    install_root: &Utf8Path,
    app: &str,
    tag: &str,
    downloaded_file: &NamedUtf8TempFile,
    asset_name: &str,
) -> anyhow::Result<()> {
    let staging_dir = fsops::make_staging(install_root, app, tag)?;

    {
        let _span = info_span!("extract", archive = %asset_name, dest = %staging_dir).entered();
        let temp_with_ext = staging_dir.join(asset_name);
        fs::copy(downloaded_file.path(), &temp_with_ext)?;
        extract::unpack(&temp_with_ext, &staging_dir)?;
        fs::remove_file(&temp_with_ext)?;
    }

    {
        let _span = info_span!("fsync", dir = %staging_dir).entered();
        fsops::fsync_directory_tree(&staging_dir)?;
        info!("Staged content synced to disk");
    }

    let releases_dir = install_root.join(app).join("releases");
    fs::create_dir_all(&releases_dir)?;
    File::open(&releases_dir)?.sync_all()?;
    let installed_dir = fsops::atomic_move(&staging_dir, &releases_dir, tag)?;

    {
        let _span = info_span!("switch", tag = %tag).entered();
        let bin_dir = install_root.join(app).join("bin");
        fs::create_dir_all(&bin_dir)?;
        fsops::link_binaries(&installed_dir, &bin_dir)?;
        info!("Symlinks updated");
    }

    Ok(())
}

fn finalize_update(
    releases_dir: &Utf8Path,
    state_path: &Utf8Path,
    tag: &str,
    validators_out: &github::ValidatorsOut,
    restart_cmd: Option<&str>,
    retain: usize,
) -> anyhow::Result<()> {
    let mut restart_failed = false;
    if let Some(cmd) = restart_cmd {
        let _span = info_span!("restart", command = %cmd).entered();
        match restart::execute(cmd) {
            Ok(()) => {
                info!("Restart command succeeded");
            }
            Err(e) => {
                warn!("Restart command failed: {}", e);
                restart_failed = true;
            }
        }
    }

    {
        let _span = info_span!("prune", retain = %retain).entered();
        let (deleted, failed) = fsops::prune_old_releases(releases_dir, tag, retain)?;
        if !deleted.is_empty() {
            info!("Pruned {} old release(s): {:?}", deleted.len(), deleted);
        }
        if !failed.is_empty() {
            warn!("Failed to prune {} release(s): {:?}", failed.len(), failed);
        }
    }

    let now = Timestamp::now();
    let new_state = State {
        latest_tag: tag.to_string(),
        etag: validators_out.etag.clone().unwrap_or_default(),
        last_modified: validators_out
            .last_modified
            .as_ref()
            .and_then(|s| s.parse().ok())
            .unwrap_or(now),
        installed_at: now,
    };
    state::save_atomic(state_path, &new_state)?;

    ensure!(
        !restart_failed,
        "Update completed but restart command failed"
    );

    Ok(())
}

/// Handles the `check` subcommand to query for updates without installing.
///
/// # Errors
///
/// Returns an error if:
/// - State file cannot be read or written
/// - GitHub API request fails
/// - Network errors occur
pub async fn handle_check(
    args: &Args,
    check_args: &CheckArgs,
    http_client: reqwest::Client,
) -> anyhow::Result<()> {
    let state_path = check_args
        .state_directory
        .join(&args.app)
        .join("state.json");
    let existing_state = state::load(&state_path)?;

    let validators = if let Some(state) = existing_state.as_ref() {
        github::Validators {
            etag: Some(state.etag.clone()),
            last_modified: Some(state.last_modified.to_string()),
        }
    } else {
        github::Validators {
            etag: None,
            last_modified: None,
        }
    };

    let fetch_result = github::fetch_latest()
        .repo(&check_args.repo)
        .maybe_token(check_args.github.token.as_deref())
        .client(http_client)
        .host(&check_args.github.host)
        .allow_prerelease(check_args.github.allow_prerelease)
        .validators(validators)
        .await?;

    let current_tag = version::current_tag(&args.install_root, &args.app)?;

    match (current_tag.as_ref(), fetch_result.release) {
        (Some(current), None) => {
            println!("up-to-date: {current}");
        }
        (Some(current), Some(release)) => {
            if *current == release.tag_name {
                println!("up-to-date: {current}");
            } else {
                println!("update-available: {} -> {}", current, release.tag_name);
            }
        }
        (None, Some(release)) => {
            println!("install-available: {}", release.tag_name);
        }
        (None, None) => {
            println!("No version installed");
        }
    }

    if let (Some(_current), Some(existing)) = (current_tag, existing_state) {
        let etag_changed = fetch_result.validators.etag.as_ref() != Some(&existing.etag);
        let last_mod_changed = fetch_result.validators.last_modified.as_ref()
            != Some(&existing.last_modified.to_string());

        if etag_changed || last_mod_changed {
            let updated_state = State {
                latest_tag: existing.latest_tag,
                etag: fetch_result.validators.etag.unwrap_or(existing.etag),
                last_modified: fetch_result
                    .validators
                    .last_modified
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(existing.last_modified),
                installed_at: existing.installed_at,
            };
            state::save_atomic(&state_path, &updated_state)?;
        }
    }

    Ok(())
}

/// Handles the `update` subcommand to download, verify, extract, and install a new release.
///
/// # Errors
///
/// Returns an error if:
/// - Lock acquisition fails (another update in progress)
/// - GitHub API request fails
/// - No matching asset found for the pattern
/// - Download fails or times out
/// - Checksum verification fails
/// - Archive extraction fails
/// - Filesystem operations fail (staging, moving, symlinking)
/// - Restart command fails (after successful installation)
pub async fn handle_update(
    args: &Args,
    update_args: &UpdateArgs,
    http_client: reqwest::Client,
) -> anyhow::Result<()> {
    let _span = info_span!("update", app = %args.app, repo = %update_args.repo).entered();

    if update_args.force_unlock {
        info!("Force unlock requested, removing lock file");
        lock::unlock(&args.app, Some(&update_args.state_directory))?;
    }

    let timeout = std::time::Duration::from_secs(update_args.lock_timeout);
    let _lock = lock::acquire(&args.app, Some(&update_args.state_directory), Some(timeout))?;

    let state_path = update_args
        .state_directory
        .join(&args.app)
        .join("state.json");
    let existing_state = state::load(&state_path)?;

    let validators = existing_state.as_ref().map_or_else(
        || github::Validators {
            etag: None,
            last_modified: None,
        },
        |state| github::Validators {
            etag: Some(state.etag.clone()),
            last_modified: Some(state.last_modified.to_string()),
        },
    );

    let fetch_result = github::fetch_latest()
        .repo(&update_args.repo)
        .maybe_token(update_args.github.token.as_deref())
        .client(http_client.clone())
        .host(&update_args.github.host)
        .allow_prerelease(update_args.github.allow_prerelease)
        .validators(validators)
        .await?;

    let current_tag = version::current_tag(&args.install_root, &args.app)?;

    if is_up_to_date(
        current_tag.as_ref(),
        fetch_result.release.as_ref(),
        existing_state.as_ref(),
        fetch_result.was_modified,
    ) {
        if let Some(tag) = current_tag.as_ref() {
            println!("Already up-to-date: {tag}");
        }
        return Ok(());
    }

    let release = fetch_result
        .release
        .ok_or_else(|| anyhow!("No release available"))?;
    let tag = &release.tag_name;

    info!("Updating to {tag}");

    let asset_pattern = Regex::new(&update_args.pattern)?;
    let checksum_pattern = update_args
        .checksum_pattern
        .as_ref()
        .map(|p| Regex::new(p))
        .transpose()?;

    let (downloaded_file, asset_name) = download_and_verify_asset(
        &release,
        &asset_pattern,
        checksum_pattern.as_ref(),
        update_args.github.token.as_deref(),
        http_client,
        update_args.skip_verification,
    )
    .await?;

    install_release(
        &args.install_root,
        &args.app,
        tag,
        &downloaded_file,
        &asset_name,
    )?;

    let releases_dir = args.install_root.join(&args.app).join("releases");
    finalize_update(
        &releases_dir,
        &state_path,
        tag,
        &fetch_result.validators,
        update_args.restart_command.as_deref(),
        update_args.retain as usize,
    )?;

    println!("Successfully updated to {tag}");
    Ok(())
}

/// Handles the `version` subcommand to display the currently installed version.
///
/// # Errors
///
/// Returns an error if:
/// - Installation directory cannot be accessed
/// - Symlink resolution fails
pub fn handle_version(args: &Args) -> anyhow::Result<()> {
    let current_tag = version::current_tag(&args.install_root, &args.app)?;

    if args.verbose > 0 {
        version::print_diagnostics(&args.install_root, &args.app, current_tag.as_deref())?;
    } else if let Some(tag) = current_tag {
        println!("{tag}");
    }

    Ok(())
}

/// Handles the `unlock` subcommand to forcibly remove the lock file.
///
/// This function removes the lock file without checking if a process is holding
/// the lock. Use with caution as it may disrupt a running update process.
///
/// # Errors
///
/// Returns an error if:
/// - The lock file exists but cannot be removed
pub fn handle_unlock(args: &Args, unlock_args: &UnlockArgs) -> anyhow::Result<()> {
    info!("Removing lock file for app: {}", args.app);
    lock::unlock(&args.app, Some(&unlock_args.state_directory))?;
    println!("Lock file removed for app: {}", args.app);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_all_flags() {
        let args = Args::try_parse_from([
            "distronomicon",
            "--app",
            "myapp",
            "--install-root",
            "/custom/opt/myapp",
            "-vv",
            "update",
            "--repo",
            "owner/name",
            "--pattern",
            ".*\\.tar\\.gz",
            "--state-directory",
            "/custom/state",
            "--checksum-pattern",
            "SHA256SUMS",
            "--github-token",
            "ghp_test123",
            "--github-host",
            "github.example.com",
            "--allow-prerelease",
            "--restart-command",
            "systemctl restart myapp",
            "--retain",
            "5",
            "--skip-verification",
        ]);

        assert!(args.is_ok());
        let args = args.unwrap();

        assert_eq!(args.app, "myapp");
        assert_eq!(args.install_root, Utf8PathBuf::from("/custom/opt/myapp"));
        assert_eq!(args.verbose, 2);

        if let Commands::Update(update_args) = args.command {
            assert_eq!(update_args.repo, "owner/name");
            assert_eq!(update_args.pattern, ".*\\.tar\\.gz");
            assert_eq!(
                update_args.state_directory,
                Utf8PathBuf::from("/custom/state")
            );
            assert_eq!(update_args.checksum_pattern.as_deref(), Some("SHA256SUMS"));
            assert_eq!(update_args.github.token.as_deref(), Some("ghp_test123"));
            assert_eq!(update_args.github.host, "github.example.com");
            assert!(update_args.github.allow_prerelease);
            assert_eq!(
                update_args.restart_command.as_deref(),
                Some("systemctl restart myapp")
            );
            assert_eq!(update_args.retain, 5);
            assert!(update_args.skip_verification);
            assert!(!update_args.force_unlock);
            assert_eq!(update_args.lock_timeout, 30);
        } else {
            panic!("Expected Update command");
        }
    }

    #[test]
    fn test_default_values() {
        let args = Args::try_parse_from([
            "distronomicon",
            "--app",
            "myapp",
            "check",
            "--repo",
            "owner/name",
            "--state-directory",
            "/var/lib/distronomicon/myapp",
        ]);

        assert!(args.is_ok());
        let args = args.unwrap();

        assert_eq!(args.app, "myapp");
        assert_eq!(args.install_root, Utf8PathBuf::from("/opt"));
        assert_eq!(args.verbose, 0);

        if let Commands::Check(check_args) = args.command {
            assert_eq!(check_args.repo, "owner/name");
            assert_eq!(
                check_args.state_directory,
                Utf8PathBuf::from("/var/lib/distronomicon/myapp")
            );
            assert_eq!(check_args.github.host, "https://api.github.com");
            assert!(!check_args.github.allow_prerelease);
            assert!(check_args.github.token.is_none());
        } else {
            panic!("Expected Check command");
        }
    }

    #[test]
    fn test_reject_app_name_with_slash() {
        let result = Args::try_parse_from([
            "distronomicon",
            "--app",
            "app/name",
            "check",
            "--repo",
            "owner/name",
            "--state-directory",
            "/var/lib",
        ]);

        assert!(result.is_err());
    }

    #[test]
    fn test_reject_app_name_with_backslash() {
        let result = Args::try_parse_from([
            "distronomicon",
            "--app",
            "app\\name",
            "check",
            "--repo",
            "owner/name",
            "--state-directory",
            "/var/lib",
        ]);

        assert!(result.is_err());
    }

    #[test]
    fn test_reject_app_name_with_dot_dot() {
        let result = Args::try_parse_from([
            "distronomicon",
            "--app",
            "../app",
            "check",
            "--repo",
            "owner/name",
            "--state-directory",
            "/var/lib",
        ]);

        assert!(result.is_err());
    }

    #[test]
    fn test_reject_empty_app_name() {
        let result = Args::try_parse_from([
            "distronomicon",
            "--app",
            "",
            "check",
            "--repo",
            "owner/name",
            "--state-directory",
            "/var/lib",
        ]);

        assert!(result.is_err());
    }

    #[test]
    fn test_reject_app_name_with_null_byte() {
        let result = Args::try_parse_from([
            "distronomicon",
            "--app",
            "app\0name",
            "check",
            "--repo",
            "owner/name",
            "--state-directory",
            "/var/lib",
        ]);

        assert!(result.is_err());
    }

    #[test]
    fn test_accept_valid_app_names() {
        for app in ["myapp", "my-app", "my_app", "app123", "APP"] {
            let result = Args::try_parse_from([
                "distronomicon",
                "--app",
                app,
                "check",
                "--repo",
                "owner/name",
                "--state-directory",
                "/var/lib",
            ]);

            assert!(result.is_ok());
        }
    }

    #[test]
    fn test_update_requires_checksum_pattern_unless_skip_verification() {
        let result = Args::try_parse_from([
            "distronomicon",
            "--app",
            "myapp",
            "update",
            "--repo",
            "owner/name",
            "--pattern",
            ".*\\.tar\\.gz",
            "--state-directory",
            "/var/lib/distronomicon",
        ]);

        assert!(result.is_err());
    }

    #[test]
    fn test_update_allows_missing_checksum_pattern_with_skip_verification() {
        let result = Args::try_parse_from([
            "distronomicon",
            "--app",
            "myapp",
            "update",
            "--repo",
            "owner/name",
            "--pattern",
            ".*\\.tar\\.gz",
            "--state-directory",
            "/var/lib/distronomicon",
            "--skip-verification",
        ]);

        assert!(result.is_ok());
    }

    #[test]
    fn test_update_accepts_both_checksum_pattern_and_skip_verification() {
        let result = Args::try_parse_from([
            "distronomicon",
            "--app",
            "myapp",
            "update",
            "--repo",
            "owner/name",
            "--pattern",
            ".*\\.tar\\.gz",
            "--state-directory",
            "/var/lib/distronomicon",
            "--checksum-pattern",
            "SHA256SUMS",
            "--skip-verification",
        ]);

        assert!(result.is_ok());
    }
}

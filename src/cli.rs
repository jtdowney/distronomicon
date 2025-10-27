use camino::Utf8PathBuf;
use clap::{Parser, Subcommand};

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
        default_value = "/opt",
        help = "Root directory for installations (creates <root>/<app>/{bin,releases,staging})"
    )]
    pub install_root: Utf8PathBuf,

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
        default_value = "api.github.com",
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

        assert!(args.is_ok(), "Failed to parse args: {:?}", args.err());
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
            assert_eq!(check_args.github.host, "api.github.com");
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

            assert!(result.is_ok(), "Valid app name '{app}' should be accepted");
        }
    }
}

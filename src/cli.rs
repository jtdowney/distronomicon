use camino::Utf8PathBuf;
use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
pub struct Args {
    #[arg(long, help = "GitHub repository (owner/name)")]
    pub repo: String,

    #[arg(long, help = "Application name")]
    pub app: String,

    #[arg(long, help = "Asset filename pattern (regex)")]
    pub pattern: String,

    #[arg(long, help = "Checksum filename pattern (optional)")]
    pub checksum_pattern: Option<String>,

    #[arg(long, help = "Allow prerelease versions")]
    pub allow_prerelease: bool,

    #[arg(
        long,
        env = "GITHUB_TOKEN",
        hide_env_values = true,
        help = "GitHub API token"
    )]
    pub github_token: Option<String>,

    #[arg(
        long,
        env = "GITHUB_HOST",
        default_value = "api.github.com",
        help = "GitHub API host"
    )]
    pub github_host: String,

    #[arg(long, help = "Command to run after successful update")]
    pub restart_command: Option<String>,

    #[arg(long, default_value = "3", help = "Number of releases to retain")]
    pub retain: u32,

    #[arg(long, help = "Skip checksum verification")]
    pub skip_verification: bool,

    #[arg(long, env = "STATE_DIRECTORY", help = "State directory")]
    pub state_directory: Utf8PathBuf,

    #[arg(long, help = "Install root directory (default: /opt/<app>)")]
    pub install_root: Option<Utf8PathBuf>,

    #[arg(short, long, action = clap::ArgAction::Count, help = "Verbose output (-v, -vv)")]
    pub verbose: u8,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    #[command(about = "Check for updates without installing")]
    Check,

    #[command(about = "Update to the latest release")]
    Update,

    #[command(about = "Show currently installed version")]
    Version,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_all_global_flags() {
        let args = Args::try_parse_from([
            "distronomicon",
            "--repo",
            "owner/name",
            "--app",
            "myapp",
            "--pattern",
            ".*\\.tar\\.gz",
            "--checksum-pattern",
            "SHA256SUMS",
            "--allow-prerelease",
            "--github-token",
            "ghp_test123",
            "--github-host",
            "github.example.com",
            "--restart-command",
            "systemctl restart myapp",
            "--retain",
            "5",
            "--skip-verification",
            "--state-directory",
            "/custom/state",
            "--install-root",
            "/custom/opt/myapp",
            "-vv",
            "update",
        ]);

        assert!(args.is_ok(), "Failed to parse args: {:?}", args.err());
        let args = args.unwrap();

        assert_eq!(args.repo, "owner/name");
        assert_eq!(args.app, "myapp");
        assert_eq!(args.pattern, ".*\\.tar\\.gz");
        assert_eq!(args.checksum_pattern.as_deref(), Some("SHA256SUMS"));
        assert!(args.allow_prerelease);
        assert_eq!(args.github_token.as_deref(), Some("ghp_test123"));
        assert_eq!(args.github_host, "github.example.com");
        assert_eq!(
            args.restart_command.as_deref(),
            Some("systemctl restart myapp")
        );
        assert_eq!(args.retain, 5);
        assert!(args.skip_verification);
        assert_eq!(args.state_directory, Utf8PathBuf::from("/custom/state"));
        assert_eq!(
            args.install_root.as_deref(),
            Some(Utf8PathBuf::from("/custom/opt/myapp").as_path())
        );
        assert_eq!(args.verbose, 2);

        assert!(matches!(args.command, Commands::Update));
    }

    #[test]
    fn test_default_values() {
        let args = Args::try_parse_from([
            "distronomicon",
            "--repo",
            "owner/name",
            "--app",
            "myapp",
            "--pattern",
            ".*\\.tar\\.gz",
            "--state-directory",
            "/var/lib/distronomicon/myapp",
            "check",
        ]);

        assert!(args.is_ok(),);
        let args = args.unwrap();

        assert_eq!(args.github_host, "api.github.com");
        assert_eq!(args.retain, 3);
        assert!(!args.allow_prerelease);
        assert!(!args.skip_verification);
        assert_eq!(args.verbose, 0);
        assert!(args.github_token.is_none());
        assert!(args.checksum_pattern.is_none());
        assert!(args.restart_command.is_none());
        assert_eq!(
            args.state_directory,
            Utf8PathBuf::from("/var/lib/distronomicon/myapp")
        );
        assert!(args.install_root.is_none());

        assert!(matches!(args.command, Commands::Check));
    }
}

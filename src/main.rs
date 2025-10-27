use clap::{Parser, Subcommand};
use tracing::{Level, info};
use tracing_subscriber::FmtSubscriber;

#[derive(Parser, Debug)]
struct Cli {
    #[arg(short, long, action = clap::ArgAction::Count)]
    verbose: u8,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    #[command(about = "Check for updates without installing")]
    Check,

    #[command(about = "Update to the latest release")]
    Update,

    #[command(about = "Show currently installed version")]
    Version,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let log_level = match cli.verbose {
        0 => Level::INFO,
        1 => Level::DEBUG,
        _ => Level::TRACE,
    };

    let subscriber = FmtSubscriber::builder().with_max_level(log_level).finish();
    tracing::subscriber::set_global_default(subscriber)?;

    match cli.command {
        Commands::Check => {
            info!("check command (stub)");
        }
        Commands::Update => {
            info!("update command (stub)");
        }
        Commands::Version => {
            info!("version command (stub)");
        }
    }

    Ok(())
}

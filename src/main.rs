use std::time::Duration;

use clap::Parser;
use distronomicon::cli::{self, Args, Commands};
use tracing::Level;
use tracing_subscriber::FmtSubscriber;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    let log_level = match args.verbose {
        0 => Level::INFO,
        1 => Level::DEBUG,
        _ => Level::TRACE,
    };

    let subscriber = FmtSubscriber::builder().with_max_level(log_level).finish();
    tracing::subscriber::set_global_default(subscriber)?;

    let http_client = distronomicon::build_http_client(Duration::from_secs(args.http_timeout))?;

    match &args.command {
        Commands::Check(check_args) => cli::handle_check(&args, check_args, http_client).await?,
        Commands::Update(update_args) => {
            cli::handle_update(&args, update_args, http_client).await?;
        }
        Commands::Version => cli::handle_version(&args)?,
        Commands::Unlock(unlock_args) => cli::handle_unlock(&args, unlock_args)?,
    }

    Ok(())
}

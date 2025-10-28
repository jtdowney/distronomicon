use camino::Utf8PathBuf;
use clap::Parser;
use tracing::{Level, trace};
use tracing_subscriber::FmtSubscriber;

use crate::cli::{Args, Commands};

mod cli;

fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    let log_level = match args.verbose {
        0 => Level::INFO,
        1 => Level::DEBUG,
        _ => Level::TRACE,
    };

    let subscriber = FmtSubscriber::builder().with_max_level(log_level).finish();
    tracing::subscriber::set_global_default(subscriber)?;

    match args.command {
        Commands::Check => {
            trace!("Subcommand: check");
        }
        Commands::Update => {
            trace!("Subcommand: update");
        }
        Commands::Version => {
            trace!("Subcommand: version");

            let opt_root = args
                .install_root
                .unwrap_or_else(|| Utf8PathBuf::from("/opt"));

            if let Some(tag) = distronomicon::version::current_tag(&opt_root, &args.app)? {
                println!("{tag}");
            } else {
                eprintln!("No version installed");
                std::process::exit(1);
            }
        }
    }

    Ok(())
}

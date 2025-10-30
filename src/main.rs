use clap::Parser;
use distronomicon::github;
use tracing::{Level, trace};
use tracing_subscriber::FmtSubscriber;

use crate::cli::{Args, Commands};

mod cli;

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

    match args.command {
        Commands::Check => {
            let state_path = args.state_directory.join(&args.app).join("state.json");
            let existing_state = distronomicon::state::load(&state_path)?;

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

            let (release_opt, validators_out, _was_modified) = github::fetch_latest(
                &args.repo,
                args.github_token.as_deref(),
                Some(&args.github_host),
                args.allow_prerelease,
                &validators,
            )
            .await?;

            let current_tag = distronomicon::version::current_tag(&args.install_root, &args.app)?;

            match (current_tag.as_ref(), release_opt) {
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
                let etag_changed = validators_out.etag.as_ref() != Some(&existing.etag);
                let last_mod_changed = validators_out.last_modified.as_ref()
                    != Some(&existing.last_modified.to_string());

                if etag_changed || last_mod_changed {
                    let updated_state = distronomicon::state::State {
                        latest_tag: existing.latest_tag,
                        etag: validators_out.etag.unwrap_or(existing.etag),
                        last_modified: validators_out
                            .last_modified
                            .and_then(|s| s.parse().ok())
                            .unwrap_or(existing.last_modified),
                        installed_at: existing.installed_at,
                    };
                    distronomicon::state::save_atomic(&state_path, &updated_state)?;
                }
            }
        }
        Commands::Update => {
            trace!("Subcommand: update");
        }
        Commands::Version => {
            trace!("Subcommand: version");

            if let Some(tag) = distronomicon::version::current_tag(&args.install_root, &args.app)? {
                println!("{tag}");
            } else {
                eprintln!("No version installed");
                std::process::exit(1);
            }
        }
    }

    Ok(())
}

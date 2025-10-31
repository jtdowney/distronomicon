use std::fs;

use anyhow::{anyhow, ensure};
use camino_tempfile::NamedUtf8TempFile;
use clap::Parser;
use distronomicon::{
    download, extract, fsops, github, lock, restart,
    state::{self, State},
    verify, version,
};
use jiff::Timestamp;
use regex::Regex;
use tracing::{Level, info, trace, warn};
use tracing_subscriber::FmtSubscriber;

use crate::cli::{Args, Commands};

mod cli;

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
    skip_verification: bool,
) -> anyhow::Result<(NamedUtf8TempFile, String)> {
    let asset = github::select_asset(&release.assets, asset_pattern)
        .ok_or_else(|| anyhow!("No asset matching pattern"))?;
    info!("Selected asset: {}", asset.name);

    let downloaded_file = download::fetch()
        .url(&asset.browser_download_url)
        .token(github_token.unwrap_or(""))
        .call()
        .await?;

    if let Some(checksum_regex) = checksum_pattern
        && !skip_verification
    {
        let checksum_asset = github::select_asset(&release.assets, checksum_regex)
            .ok_or_else(|| anyhow!("No checksum asset matching pattern"))?;
        verify::fetch_and_verify_checksum(
            &asset.name,
            &checksum_asset.browser_download_url,
            github_token,
            downloaded_file.path(),
        )
        .await?;
        info!("Checksum verified");
    }

    Ok((downloaded_file, asset.name.clone()))
}

async fn handle_update(args: &Args) -> anyhow::Result<()> {
    let _lock = lock::acquire(&args.app, None)?;

    let state_path = args.state_directory.join(&args.app).join("state.json");
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

    let (release_opt, validators_out, was_modified) = github::fetch_latest(
        &args.repo,
        args.github_token.as_deref(),
        Some(&args.github_host),
        args.allow_prerelease,
        &validators,
    )
    .await?;

    let current_tag = version::current_tag(&args.install_root, &args.app)?;

    if is_up_to_date(
        current_tag.as_ref(),
        release_opt.as_ref(),
        existing_state.as_ref(),
        was_modified,
    ) {
        if let Some(tag) = current_tag.as_ref() {
            println!("Already up-to-date: {tag}");
        }
        return Ok(());
    }

    let release = release_opt.ok_or_else(|| anyhow!("No release available"))?;
    let tag = &release.tag_name;

    info!("Updating to {tag}");

    let asset_pattern = Regex::new(&args.pattern)?;
    let checksum_pattern = args
        .checksum_pattern
        .as_ref()
        .map(|p| Regex::new(p))
        .transpose()?;

    let (downloaded_file, asset_name) = download_and_verify_asset(
        &release,
        &asset_pattern,
        checksum_pattern.as_ref(),
        args.github_token.as_deref(),
        args.skip_verification,
    )
    .await?;

    let staging_dir = fsops::make_staging(&args.install_root, &args.app, tag)?;

    let temp_with_ext = staging_dir.join(&asset_name);
    fs::copy(downloaded_file.path(), &temp_with_ext)?;
    extract::unpack(&temp_with_ext, &staging_dir)?;
    fs::remove_file(&temp_with_ext)?;

    let releases_dir = args.install_root.join(&args.app).join("releases");
    let installed_dir = fsops::atomic_move(&staging_dir, &releases_dir, tag)?;

    let bin_dir = args.install_root.join(&args.app).join("bin");
    fs::create_dir_all(&bin_dir)?;
    fsops::link_binaries(&installed_dir, &bin_dir)?;
    info!("Symlinks updated");

    let mut restart_failed = false;
    if let Some(restart_cmd) = args.restart_command.as_ref() {
        match restart::execute(restart_cmd) {
            Ok(()) => {
                info!("Restart command succeeded");
            }
            Err(e) => {
                warn!("Restart command failed: {}", e);
                restart_failed = true;
            }
        }
    }

    let deleted = fsops::prune_old_releases(&releases_dir, tag, args.retain as usize)?;
    if !deleted.is_empty() {
        info!("Pruned {} old release(s): {:?}", deleted.len(), deleted);
    }

    let now = Timestamp::now();
    let new_state = State {
        latest_tag: tag.clone(),
        etag: validators_out.etag.unwrap_or_default(),
        last_modified: validators_out
            .last_modified
            .and_then(|s| s.parse().ok())
            .unwrap_or(now),
        installed_at: now,
    };
    state::save_atomic(&state_path, &new_state)?;

    ensure!(
        !restart_failed,
        "Update completed but restart command failed"
    );

    println!("Successfully updated to {tag}");
    Ok(())
}

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

            let (release_opt, validators_out, _was_modified) = github::fetch_latest(
                &args.repo,
                args.github_token.as_deref(),
                Some(&args.github_host),
                args.allow_prerelease,
                &validators,
            )
            .await?;

            let current_tag = version::current_tag(&args.install_root, &args.app)?;

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
                    let updated_state = State {
                        latest_tag: existing.latest_tag,
                        etag: validators_out.etag.unwrap_or(existing.etag),
                        last_modified: validators_out
                            .last_modified
                            .and_then(|s| s.parse().ok())
                            .unwrap_or(existing.last_modified),
                        installed_at: existing.installed_at,
                    };
                    state::save_atomic(&state_path, &updated_state)?;
                }
            }
        }
        Commands::Update => {
            handle_update(&args).await?;
        }
        Commands::Version => {
            trace!("Subcommand: version");

            if let Some(tag) = version::current_tag(&args.install_root, &args.app)? {
                println!("{tag}");
            } else {
                eprintln!("No version installed");
                std::process::exit(1);
            }
        }
    }

    Ok(())
}

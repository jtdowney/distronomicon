# distronomicon

[![Crates.io](https://img.shields.io/crates/v/distronomicon)](https://crates.io/crates/distronomicon)
[![License](https://img.shields.io/crates/l/distronomicon)](LICENSE)
[![CI](https://github.com/jtdowney/distronomicon/actions/workflows/ci.yml/badge.svg)](https://github.com/jtdowney/distronomicon/actions/workflows/ci.yml)

A Linux tool that checks GitHub for repository releases and performs atomic updates under `/opt/<app>`. Designed for use with systemd timers.

## Installation

### From crates.io

```bash
cargo install distronomicon
```

### From source

```bash
cargo build --release
sudo cp target/release/distronomicon /usr/local/bin/
```

## Usage

All commands require `--app <name>` to specify the application being managed.

### Check for updates

Query GitHub for the latest release without installing:

```bash
distronomicon --app myapp check \
  --repo owner/repo \
  --state-directory /var/lib/distronomicon
```

Prints `up-to-date: v1.2.3`, `update-available: v1.2.3 -> v1.2.4`, or `install-available: v1.2.4`.

### Update to latest release

Download, verify, and install the latest release:

```bash
distronomicon --app myapp update \
  --repo owner/repo \
  --pattern 'myapp-.*\.tar\.gz' \
  --checksum-pattern 'SHA256SUMS' \
  --state-directory /var/lib/distronomicon \
  --restart-command 'systemctl restart myapp'
```

This will:

1. Download the matching release asset
2. Verify the checksum
3. Extract to `/opt/myapp/releases/<tag>`
4. Update symlinks in `/opt/myapp/bin`
5. Run the restart command (if provided)
6. Prune old releases (keeps 3 by default, configurable with `--retain`)

### Show installed version

```bash
distronomicon --app myapp version
```

Prints the currently installed tag (e.g., `v1.2.3`), derived from symlinks in the bin directory.

## Filesystem Layout

```
/opt/<app>/
  bin/                    # Symlinks to current release binaries
    myapp -> ../releases/v1.2.3/myapp
  releases/
    v1.2.2/              # Previous release
    v1.2.3/              # Current release
  staging/               # Temporary extraction (cleaned after install)

/var/lib/distronomicon/<app>/state.json   # Tracks latest tag, ETag, Last-Modified
```

The `--install-root` flag changes the base from `/opt` to another location.

## GitHub Authentication

For private repositories or higher rate limits, provide a token:

```bash
export GITHUB_TOKEN=ghp_...
distronomicon --app myapp update ...
```

Or use `--github-token` flag.

## Systemd Timer

Example service and timer files are in the `systemd/` directory.

### Setup

1. Copy the systemd files:

```bash
sudo cp systemd/distronomicon@.{service,timer} /etc/systemd/system/
sudo systemctl daemon-reload
```

2. Configure the service using a drop-in override:

```bash
sudo systemctl edit distronomicon@myapp.service
```

3. Add the required environment variables:

```ini
[Service]
Environment="DISTRONOMICON_REPO=owner/repo"
Environment="DISTRONOMICON_PATTERN=myapp-.*\.tar\.gz"
```

4. Enable and start the timer:

```bash
sudo systemctl enable --now distronomicon@myapp.timer
```

### Environment Variables

**Required:**
- `DISTRONOMICON_REPO` - GitHub repository in `owner/repo` format
- `DISTRONOMICON_PATTERN` - Regex pattern to match release assets

**Optional:**
- `GITHUB_TOKEN` - GitHub API token (for private repos or higher rate limits)
- `GITHUB_HOST` - GitHub Enterprise host (default: `https://api.github.com`)
- `STATE_DIRECTORY` - State directory (auto-set by systemd via `StateDirectory=`)
- `DISTRONOMICON_CHECKSUM_PATTERN` - Checksum file pattern (e.g., `SHA256SUMS`)
- `DISTRONOMICON_RESTART_COMMAND` - Command to run after update (e.g., `systemctl restart myapp`)
- `DISTRONOMICON_RETAIN` - Number of old releases to keep (default: `3`)
- `DISTRONOMICON_INSTALL_ROOT` - Install base directory (default: `/opt`)
- `DISTRONOMICON_ALLOW_PRERELEASE` - Include prereleases (set to `true`)

**⚠️ Note:** If you change `DISTRONOMICON_INSTALL_ROOT`, you must also override `ReadWritePaths` in your drop-in configuration to grant write access to the custom location (required by `ProtectSystem=strict`).

**Example with optional variables:**

```ini
[Service]
Environment="DISTRONOMICON_REPO=owner/repo"
Environment="DISTRONOMICON_PATTERN=myapp-.*\.tar\.gz"
Environment="GITHUB_TOKEN=ghp_..."
Environment="DISTRONOMICON_CHECKSUM_PATTERN=SHA256SUMS"
Environment="DISTRONOMICON_RESTART_COMMAND=systemctl restart myapp"
Environment="DISTRONOMICON_RETAIN=5"
Environment="DISTRONOMICON_INSTALL_ROOT=/custom/opt"
ReadWritePaths=/custom/opt
```

### Customizing the Timer

By default, the timer checks every 3 minutes. To change the interval:

```bash
sudo systemctl edit distronomicon@myapp.timer
```

```ini
[Timer]
OnBootSec=5m
OnUnitActiveSec=10m
```

### Monitoring

Check status:

```bash
systemctl status distronomicon@myapp.timer
systemctl list-timers distronomicon@*
journalctl -u distronomicon@myapp.service
```

## Options

- `--install-root` - Change base directory (default: `/opt`)
- `--skip-verification` - Skip checksum verification (not recommended)
- `--retain N` - Keep N old releases after update (default: 3)
- `--allow-prerelease` - Include prerelease versions
- `--github-host` - Use GitHub Enterprise (default: `https://api.github.com`)
- `-v`, `-vv` - Increase logging verbosity

## Future Ideas

Features under consideration for future development

### Safety & Reliability

- **Dry-run mode** - Preview updates without making changes (`--dry-run`)
- **Rollback command** - Revert to previous releases when issues are discovered
- **Version pinning** - Lock to specific versions during maintenance windows
- **Health checks** - Doctor command to validate symlink integrity
- **Update policies** - Enforce semantic constraints (max major version, maintenance windows)

### Observability

- **Metrics** - Prometheus/OpenTelemetry exports for monitoring
- **Audit logs** - Structured event logs (JSONL) for debugging
- **Notifications** - Webhook/Slack/email alerts for update results
- **History queries** - Commands to inspect past update attempts

### Flexibility & Extensibility

- **Pluggable sources** - Support GitLab, S3, OCI registries, generic HTTP
- **Additional checksums** - BLAKE2, BLAKE3, SHA-512 algorithm support
- **Signature verification** - Sigstore/cosign or GPG signature checks
- **Custom extraction** - Flexible archive handling and post-install hooks
- **Systemd generation** - Generate service/timer templates per app

### Fleet & Orchestration

- **Staggered updates** - Coordinate batch updates across multiple hosts
- **Control API** - REST/gRPC daemon for remote management

### Networking & Resilience

- **Resumable downloads** - Continue interrupted downloads of large artifacts
- **Mirror support** - Configure fallback sources for high availability

## License

MIT

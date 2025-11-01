# distronomicon

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

Example service and timer files are in the `systemd/` directory. To set up:

```bash
sudo cp systemd/distronomicon@.{service,timer} /etc/systemd/system/
sudo mkdir -p /etc/distronomicon
# Create /etc/distronomicon/myapp.conf with environment variables
sudo systemctl daemon-reload
sudo systemctl enable --now distronomicon@myapp.timer
```

Check status:

```bash
systemctl status distronomicon@myapp.timer
systemctl list-timers distronomicon@*
```

## Options

- `--install-root` - Change base directory (default: `/opt`)
- `--skip-verification` - Skip checksum verification (not recommended)
- `--retain N` - Keep N old releases after update (default: 3)
- `--allow-prerelease` - Include prerelease versions
- `--github-host` - Use GitHub Enterprise (default: `api.github.com`)
- `-v`, `-vv` - Increase logging verbosity

## License

MIT

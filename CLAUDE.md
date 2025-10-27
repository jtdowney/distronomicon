# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

**distronomicon** is a Linux tool that periodically checks GitHub for a repository's latest release and performs atomic, verified updates under `/opt/<app>`. Designed for use under systemd timers, it handles downloading, checksum verification, atomic installation, symlink switching, and optional restart commands.

**Key characteristics:**
- CLI-only configuration (clap, no config files)
- Atomic operations with exclusive locking
- Safe extraction with strict path validation
- Conditional HTTP requests (ETag/Last-Modified)
- Retention-based pruning of old releases

## Development Commands

**Build:**
```bash
cargo build
cargo build --release
```

**Run:**
```bash
cargo run -- <subcommand> [flags]
# Example:
cargo run -- check --repo owner/name --app myapp --pattern 'asset-.*\.tar\.gz'
```

**Test:**
```bash
cargo test
cargo test --lib              # unit tests only
cargo test --test <name>      # specific integration test
```

**Format & Lint:**
```bash
cargo fmt
cargo clippy
```

## Architecture

The codebase follows a library-first design with a thin CLI wrapper:

- **`src/main.rs`** — CLI entry point; parses args and delegates to lib
- **`src/lib.rs`** — Public API for update operations
- **`src/cli.rs`** — Clap argument parsing and CLI structs

**Core modules** (planned, per `docs/spec.md`):
- `github` — GitHub API client, release queries, conditional requests
- `download` — Asset fetching with reqwest-retry
- `verify` — SHA256 checksum parsing and validation
- `extract` — Archive detection and safe extraction (tar.gz, zip, etc.)
- `fsops` — Atomic moves, symlink updates, retention pruning
- `state` — JSON state file (ETag, Last-Modified, installed_at) with atomic writes
- `lock` — Exclusive process locking via `/var/lock/distronomicon-<app>.lock`
- `restart` — Execute optional `--restart-cmd` via shell
- `version` — Discover currently installed version from symlinks

**Data flow (update subcommand):**
1. Acquire exclusive lock
2. Query GitHub `/repos/{owner}/{name}/releases/latest` (with conditional headers)
3. Compare latest tag with current version (via symlink resolution)
4. Download matching asset (first match by `--pattern`)
5. Verify checksum (unless `--skip-verification`)
6. Extract to staging under `/opt/<app>/staging/<tag>.[random]`
7. Atomic `rename()` to `/opt/<app>/releases/<tag>`
8. Update symlinks in `/opt/<app>/bin` to point to new release
9. Run `--restart-cmd` if provided
10. Prune old releases (keep `--retain` most recent, default 3)
11. Write state.json atomically
12. Release lock

**Key safety invariants:**
- Extraction rejects absolute paths, `..`, symlink escapes, device/pipe files
- Checksum verification required by default
- Atomic directory moves prevent torn installs
- Exclusive locking prevents concurrent updates
- Previous version remains untouched until new version is fully verified

## Filesystem Layout

Default paths (configurable via `--opt-root` and `--state-dir`):

```
/opt/<app>/
  bin/                       # Stable symlink targets (e.g., bin/myapp -> ../releases/v0.1.3/myapp)
  releases/
    v0.1.2/                  # Installed release directories
    v0.1.3/
  staging/                   # Temporary extraction (cleaned after success/failure)

/var/lib/distronomicon/<app>/state.json   # Persistent state (latest_tag, etag, last_modified, installed_at)
/var/lock/distronomicon-<app>.lock        # Exclusive lock file
```

## Subcommands

- **`check`** — Query GitHub for updates; print status; update state validators (ETag/Last-Modified); no install side effects
- **`update`** — Full update lifecycle (lock → check → download → verify → extract → switch → restart → prune)
- **`version`** — Print currently active tag (derived from `/opt/<app>/bin` symlinks)

Exit codes: `0` = success or no-op; `1` = any failure

## Testing Strategy

- **Unit tests** in same file under `mod tests`:
  - `verify` — SHA256SUMS parsing variants
  - `extract` — Path sanitization (reject `..`, absolute paths, symlink escapes)
  - `fsops` — Pruning logic, symlink updates
  - `state` — Atomic write/read integrity
- **Integration tests**:
  - Mock GitHub API (wiremock)
  - End-to-end update flows (happy path, checksum mismatch, restart-cmd failures)
  - Check-only 304 behavior
- Use temp directories under `/tmp` for filesystem tests

## Implementation Notes

- Use `tracing` spans for major steps (update, download, verify, extract, switch, restart)
- Return rich errors (consider `thiserror`) and map to `std::process::exit(1)` in `main`
- Favor small, pure functions in lib modules
- Archive format detection by file extension + magic bytes
- Strip top-level directory from archives if single-root
- Preserve executable bits; default to `0755` for binaries if needed
- Use `camino-tempfile` for staging extraction
- Use `reqwest` with `rustls-tls` (no native TLS)
- Conditional requests use `If-None-Match` / `If-Modified-Since` headers

## Dependencies (per spec)

Core:
- `clap` (derive), `regex`, `tokio` (rt-multi-thread, macros)
- `reqwest` (rustls-tls, json, stream), `reqwest-retry`
- `serde`, `serde_json`, `camino`, `camino-tempfile`
- `sha2`, `tar`, `zip`, `flate2`, `tracing`

Optional (enable for additional archive formats):
- `xz2` for tar.xz
- `zstd` for tar.zst

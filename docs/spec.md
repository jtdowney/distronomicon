**Purpose:**
A Rust program for Linux that periodically checks GitHub for a repository's latest release and, if newer, downloads the matching asset, verifies it, atomically installs under `/opt/<app>`, switches symlinks, and optionally runs an arbitrary restart command. Designed for use under a systemd timer. Configuration is CLI-only (clap); no config files.

---

## 1. Command-Line Interface (CLI)

### 1.1 Subcommands

- `check`
  - Query GitHub "latest" release (optionally including prereleases).
  - Decide if an update is needed.
  - Update cached HTTP validators (ETag/Last-Modified) in state.
  - **No install side effects**.
  - Exit 0 in all cases (success or up-to-date), 1 on failures.
- `update`
  - Full lifecycle: lock → check → download → verify → extract → atomic switch → optional restart-cmd → prune.
  - Exit 0 on success/no-op, 1 on any failure.
- `version`
  - Print currently active tag (derived from symlink/dir names).
  - Exit 0 unless an internal error occurs.

### 1.2 Global Flags (apply to All Subcommands unless noted)

- `--repo <owner/name>` **(required)**—GitHub repo slug.
- `--app <name>` **(required)**—Directory anchor, e.g., `/opt/<app>`.
- `--pattern <regex>` **(required)**—Selects the asset by **filename**; **first match wins**.
- `--checksum-pattern <regex>`—Optional regex for a **separate checksum asset** filename.
- `--allow-prerelease`—Include prereleases in "latest" determination (default: false).
- `--token <string>`—GitHub token (if omitted, read `GITHUB_TOKEN` env).
- `--github-host <host>`—Default `api.github.com` (allow GHES).
- `--restart-cmd <shell-cmd>`—Arbitrary command to run after successful switch.
- `--retain <N>`—Release retention count (default: 3).
- `--skip-verification`—Allow install without checksum verification.
- `-v` / `-vv`—`-v`=debug, `-vv`=trace (default level: info).
- `--state-dir <path>`—Default `/var/lib/distronomicon/<app>`.
- `--opt-root <path>`—Default `/opt/<app>`.

**Exit codes:** `0` = success or no-op; `1` = any failure (network, lock, API, checksum, extraction, etc.).

**Notes:**
- Target triples are **not** used in v1 (pattern strategy only).
- "Latest" means GitHub's latest release (respecting `--allow-prerelease`).

---

## 2. Filesystem Layout

Given `--app lantern` and default roots:

```
/opt/lantern/
  bin/                    # Stable symlink targets
  releases/
    v0.1.2/               # Atomically moved in place after verify
    v0.1.3/
  staging/                # Temporary during update (cleaned)
```

**State & Lock:**

```
/var/lib/distronomicon/lantern/state.json   # Minimal state, atomic writes
/var/lock/distronomicon-lantern.lock        # Exclusive lock file
```

**Ownership/Perms (defaults):**
- Directories: `root:root`, `0755`
- Files: `0644`
- Executables: preserve mode if available; else set `0755`
- Strict extraction sandbox: never create paths outside release dir.

---

## 3. State File

**Path:** `/var/lib/distronomicon/<app>/state.json`
**Atomic write:** write to temp (camino-tempfile) → `fsync` dir → `rename()`.

**Schema (v1, minimal):**

```json
{
  "latest_tag": "v0.1.2",
  "etag": "\"abc123\"",
  "last_modified": "Mon, 27 Oct 2025 15:00:00 GMT",
  "installed_at": "2025-10-27T15:03:21Z"
}
```

- `installed_at` is informational; may be omitted.

---

## 4. Update Flow (subcommand: `update`)

1. **Acquire lock**
   - Exclusive lock at `/var/lock/distronomicon-<app>.lock`.
   - Block until acquired (no timeout).
2. **Query GitHub "latest"**
   - `GET /repos/{owner}/{name}/releases/latest` on `--github-host` with `--token`.
   - If `--allow-prerelease`, use an endpoint pattern or filter to allow prereleases (implementation detail: fetch releases and pick latest honoring flag).
   - Use conditional headers from state (`If-None-Match` / `If-Modified-Since`) when present.
   - Update `etag`, `last_modified` in state on 200/304.
3. **Determine current active tag**
   - Resolve `/opt/<app>/bin` symlinks back to `/opt/<app>/releases/<tag>` (if present).
4. **Compare tags**
   - If latest tag == current tag → **no-op**, exit 0 after releasing lock.
5. **Select asset**
   - Iterate release assets in GitHub response; **first filename matching `--pattern`** wins.
   - If none match → error (exit 1).
6. **Download asset**
   - `reqwest` + `reqwest-retry` (default 3 w/ exponential backoff).
   - No partial/resume; always full transfer.
7. **Checksum verification (required unless `--skip-verification`)**
   - If `--checksum-pattern` provided, download matching asset (first match by filename).
   - Expect **sha256** entries (format tolerant: `<hex>  <filename>`).
   - Verify downloaded asset **exactly** matches checksum.
   - On mismatch/missing: fail (exit 1) unless `--skip-verification`.
8. **Extract to temp**
   - Create temp dir under `/opt/<app>/staging/<tag>.[random]` using `camino-tempfile`.
   - Supported formats:
	 - `tar.gz`, `tar.xz`, `tar.zst` (via `flate2`/`xz2`/`zstd` if included), `zip`, and **raw single binary**.
	 - **Spec:** "all the formats"—in v1 include `tar.gz`, `zip`; include `tar.xz` and `tar.zst` if deps are enabled (see §9).
   - **Path policy:** strip top-level dir **based on archive content** (if a single top-level directory exists, strip it); otherwise preserve relative paths.
   - **Safety:** reject absolute paths, `..`, symlinks escaping target, device/pipe files.
   - **Perms:** preserve exec bits where present; set `0755` on files that look like executables if needed.
9. **Atomic move**
   - `rename(temp_dir, /opt/<app>/releases/<tag>)`.
   - If target exists, fail (no overwrite).
10. **Update `/opt/<app>/bin` symlinks**
	- **Link all binaries** found in the new release dir:
	  - Discover executables: files with exec bit set or common bin names.
	  - For each file `X` in release: create/replace symlink `/opt/<app>/bin/X -> ../releases/<tag>/X`.
	  - Ensure operation is as atomic as possible per file (temporary link + rename).
11. **Run restart command (optional)**
	- If `--restart-cmd` provided, execute via shell (`/bin/sh -c "<cmd>"`) and wait.
	- Non-zero exit → fail (exit 1) **after** leaving new version installed (no rollback).
12. **Prune old releases**
	- Keep most recent `--retain N` directories in `/opt/<app>/releases` (default 3).
	- Delete older ones.
13. **Write state**
	- Update `latest_tag` and `installed_at` timestamp.
	- Preserve `etag` and `last_modified` values.
14. **Release lock**, exit 0.

**Failure behavior:**
- Cleanup temp files/dirs best-effort.
- Leave previous working version unchanged (we switch links only after extract/verify succeed).
- **No rollback** is needed for the failure categories we allow past the switch (only `restart-cmd` could fail post-switch; we accept that trade-off).

---

## 5. Check Flow (subcommand: `check`)

- Same GitHub query logic (conditional requests).
- Print human-readable status:
  - `up-to-date: <tag>` or
  - `update-available: <current> -> <latest>` (if current known) or
  - `install-available: <latest>` (if no current).
- Update `etag`/`last_modified` in state.
- No install side effects.

---

## 6. Version Flow (subcommand: `version`)

- Resolve symlinks under `/opt/<app>/bin` to discover current tag (e.g., `../releases/v0.1.2`).
- Print the tag only (default), or add detail under `-v`.

---

## 7. Asset & Checksum Parsing

- **Asset selection:** filename regex (Rust `regex`), **first match wins** in GitHub's returned order.
- **Checksum parsing:** tolerate common formats:

  ```
  <hex>  <filename>
  <hex> *<filename>
  ```

- **Strictness:** must match the actual downloaded asset filename exactly.
- **Algorithm:** **sha256** only in v1.

---

## 8. Logging, Idempotency, and Locking

- **Logging:** `tracing` with levels:
  - info (default), debug (`-v`), trace (`-vv`).
- **Stdout-only** logs; no files.
- **Idempotent behavior:** If latest already installed, **no-op** with exit 0.
- **Locking:** process-wide exclusive lock file; block until available.
- **Retries:** only downloads (reqwest-retry). If retries fail, exit 1. No whole-run retries.

---

## 9. Implementation Guidance

### 9.1 Crate Metadata

- **Crate name:** `distronomicon`
- **Binary:** `distronomicon`
- **License:** MIT
- **Edition:** 2024
- **MSRV:** not pinned for now

### 9.2 Dependencies (Cargo.toml)

```toml
[package]
name = "distronomicon"
version = "0.1.0"
edition = "2024"
license = "MIT"

[dependencies]
clap = { version = "4", features = ["derive"] }
regex = "1"
tokio = { version = "1", features = ["rt-multi-thread", "macros"] }
reqwest = { version = "0.12", default-features = false, features = ["rustls-tls", "json", "stream"] }
reqwest-retry = "0.5"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
camino = "1"
camino-tempfile = "1"
sha2 = "0.10"
tar = "0.4"
zip = { version = "0.6", default-features = false, features = ["deflate"] }
flate2 = "1"
tracing = "0.1"

# Optional extras if you want xz/zstd:
# xz2 = "0.1"      # for tar.xz
# zstd = "0.13"    # for tar.zst
```

> Note: Enable `xz2` and/or `zstd` if you want `tar.xz`/`tar.zst` in v1; otherwise, document that those formats are planned.

### 9.3 Crate Structure (binary/library split)

```
src/
  main.rs                 # CLI entry; thin wrapper around lib
  lib.rs                  # Public API for operations
  cli.rs                  # clap structs and parsing
  github.rs               # GitHub API client + release model
  download.rs             # Asset download + retries
  verify.rs               # SHA256 + checksum-file parsing
  extract.rs              # Archive detection + safe extraction
  fsops.rs                # Atomic moves, symlink updates, pruning
  state.rs                # JSON state (read/atomic-write)
  lock.rs                 # Lock acquisition/release
  restart.rs              # Execute --restart-cmd
  version.rs              # Current version discovery
```

**Key modules & contracts:**

- `github::fetch_latest(repo, token, host, allow_prerelease, validators) -> LatestRelease { tag, assets, validators }`
  - `validators` carries `{ etag, last_modified }` in/out.
- `download::fetch(asset_url, token) -> TempFile` (with reqwest-retry).
- `verify::checksum(asset_path, checksum_text) -> bool`
- `extract::unpack(archive_path, temp_dir) -> ()`
  - Detect type by file extension & magic; reject unsafe entries.
  - Stripping top-level dir if single-root.
- `fsops::{atomic_move(temp_dir, release_dir), link_binaries(release_dir, bin_dir), prune(releases_dir, retain)}`
- `state::{load(path) -> State, save_atomic(path, &State)}`
- `lock::acquire(app) -> Guard`
- `restart::run(cmd: &str) -> Result<()>`

**Error type:** a crate-wide error enum with context (`thiserror` optional). All failures map to exit 1.

---

## 10. Security Considerations

- Always require TLS (`rustls`) and authenticated requests when a token is provided.
- Enforce safe extraction: no absolute paths, no `..`, no symlink escapes, no special files.
- Verify checksums (unless `--skip-verification`).
- Atomic directory `rename()` prevents torn installs.
- Run `--restart-cmd` only **after** symlinks point to the new version.

---

## 11. Example Usage

### Install Lantern Latest x86_64 GNU Build Asset by Pattern, Verify via SHA256 File

```bash
distronomicon update \
  --repo getlantern/lantern \
  --app lantern \
  --pattern 'lantern-v[0-9]+\.[0-9]+\.[0-9]+-x86_64-unknown-linux-gnu\.tar\.(gz|xz|zst|zip)$' \
  --checksum-pattern 'SHA256SUMS(\.txt)?$' \
  --token "$GITHUB_TOKEN"
```

### Check only (no changes)

```bash
distronomicon check \
  --repo getlantern/lantern \
  --app lantern \
  --pattern 'lantern-.*-x86_64.*'
```

### With a Restart Command

```bash
distronomicon update \
  --repo getlantern/lantern \
  --app lantern \
  --pattern 'lantern-.*-x86_64.*' \
  --checksum-pattern 'SHA256SUMS(\.txt)?$' \
  --restart-cmd 'systemctl restart lantern.service'
```

---

## 12. Systemd Examples (docs Only; You Manage units)

**Service (one-app-per-instance):** `/etc/systemd/system/distronomicon@.service`

```ini
[Unit]
Description=Distronomicon updater for %i
Wants=network-online.target
After=network-online.target

[Service]
Type=oneshot
ExecStart=/usr/local/bin/distronomicon update \
  --repo <owner/name> \
  --app %i \
  --pattern '<your-regex-here>' \
  --checksum-pattern 'SHA256SUMS(\.txt)?$' \
  --token ${GITHUB_TOKEN}
# Example: restart lantern after switch
# ExecStartPost=/bin/sh -c "/usr/local/bin/distronomicon version --app %i && systemctl restart %i.service"
```

**Timer (default schedule = every minute):** `/etc/systemd/system/distronomicon@.timer`

```ini
[Unit]
Description=Schedule distronomicon for %i

[Timer]
OnCalendar=*:*:00
Unit=distronomicon@%i.service
AccuracySec=30s
Persistent=true

[Install]
WantedBy=timers.target
```

Enable:

```bash
systemctl daemon-reload
systemctl enable --now distronomicon@lantern.timer
```

---

## 13. Testing Strategy

- **Unit tests** (library-first design):
  - `verify` parsing for typical SHA256SUMS formats.
  - `extract` path sanitization: reject `..`, abs paths, symlink escapes.
  - `fsops` prune logic and symlink updates.
  - `state` atomic write/read integrity.
- **Integration tests:**
  - Fake GitHub server (wiremock) → asset + checksum → success path.
  - Check-only 304 behavior updates validators without install.
  - Failure modes: checksum mismatch, bad archive, restart-cmd non-zero.
- **e2e smoke (optional):**
  - Temp dirs under `/tmp/opt/<app>` and `/tmp/var/lib/distronomicon/<app>`.

---

## 14. Non-Goals / Future Work

- Multiple apps per single run (v1 is one-on-one invocation).
- Signature verification (PGP/minisign)—future.
- Partial download resume—future if needed.
- Cargo-dist manifest strategy—**explicitly out** for v1 per design (pattern-only).
- Rollbacks beyond pre-switch failures.

---

## 15. Developer Notes

- Favor small, pure functions in `lib.rs` modules; `main.rs` should only parse CLI and call into lib.
- Use `tracing` spans for major steps (`update`, `download`, `verify`, `extract`, `switch`, `restart`).
- Return rich errors (optionally `thiserror`) and map to `std::process::exit(1)` in `main`.
- When adding new archive formats, extend `extract::detect()` and gate with optional features.

---

## 16. Acceptance Criteria

- Running `update` on a host with no prior install creates `/opt/<app>/releases/<tag>`, symlinks binaries to `/opt/<app>/bin`, writes `state.json`, exits 0.
- Re-running `update` when up-to-date performs no changes and exits 0.
- Supplied checksum validates and mismatches cause exit 1 unless `--skip-verification`.
- `check` never mutates `/opt/<app>` and updates only state validators.
- `version` prints the active tag correctly.
- Locking prevents concurrent interleaving.
- Retention correctly prunes to `--retain`.

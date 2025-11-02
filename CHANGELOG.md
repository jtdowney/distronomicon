# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.2] - 2025-01-11

### Fixed

- Replace autocompress with niffler for XZ decompression support
- Add Accept header to checksum download requests
- Use GitHub asset API URL with octet-stream header

### Changed

- Upgrade to Rust edition 2024 and use let-chains syntax
- Enable cargo-dist updater for automated release infrastructure updates

## [0.1.1] - 2025-11-01

### Changed

- Updated release infrastructure configuration

## [0.1.0] - 2025-11-01

### Added

- Initial implementation of GitHub release updater
- `check` subcommand to query GitHub for updates without installing
- `update` subcommand for full update lifecycle (download, verify, extract, install)
- `version` subcommand to display currently active release tag
- `unlock` subcommand to forcibly remove stale lock files
- GitHub API client with conditional HTTP requests (ETag/Last-Modified support)
- Asset downloading with automatic retry and backoff
- SHA256 checksum verification with optional skip flag
- Archive extraction supporting tar.gz, tar.bz2, tar.xz, tar.zst, and zip formats
- Atomic filesystem operations for safe installation
- Exclusive process locking to prevent concurrent updates
- State persistence (latest tag, etag, last modified timestamp)
- Symlink management in `/opt/<app>/bin`
- Retention-based pruning of old releases (configurable, default 3)
- Optional restart command execution after successful updates
- Configurable paths for opt root and state directory
- Safe extraction with path validation (rejects absolute paths, `..`, symlinks, device files)
- Extraction limits (max files, max size, max decompression ratio)
- Comprehensive tracing and logging support

### Security

- Path sanitization prevents directory traversal attacks
- Checksum verification enabled by default
- Atomic operations prevent partially installed releases
- Exclusive locking prevents race conditions

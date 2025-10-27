use std::{
    fs,
    io::Write as _,
    os::unix::{self, fs::PermissionsExt},
};

use assert_cmd::cargo::cargo_bin_cmd;
use camino::Utf8Path;
use camino_tempfile::tempdir;
use camino_tempfile_ext::prelude::*;
use flate2::{Compression, write::GzEncoder};
use jiff::Timestamp;
use sha2::{Digest as _, Sha256};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

fn create_state_file(state_dir: impl AsRef<Utf8Path>, app: &str, tag: &str, etag: &str) {
    let state_dir = state_dir.as_ref();
    let app_dir = state_dir.join(app);
    fs::create_dir_all(&app_dir).unwrap();

    let now = Timestamp::now();
    let state = serde_json::json!({
        "latest_tag": tag,
        "etag": etag,
        "last_modified": now.to_string(),
        "installed_at": now.to_string(),
    });

    let state_path = app_dir.join("state.json");
    fs::write(state_path, serde_json::to_string_pretty(&state).unwrap()).unwrap();
}

fn create_installed_version(install_root: impl AsRef<Utf8Path>, app: &str, tag: &str) {
    let install_root = install_root.as_ref();
    let releases_dir = install_root.join(app).join("releases").join(tag);
    let bin_dir = install_root.join(app).join("bin");

    fs::create_dir_all(&releases_dir).unwrap();
    fs::create_dir_all(&bin_dir).unwrap();

    let binary_path = releases_dir.join(app);
    fs::write(&binary_path, "fake binary").unwrap();
    let mut perms = fs::metadata(&binary_path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&binary_path, perms).unwrap();

    let symlink_path = bin_dir.join(app);
    unix::fs::symlink(format!("../releases/{tag}/{app}"), symlink_path).unwrap();
}

fn create_tar_gz_with_binary(app_name: &str, content: &[u8]) -> Vec<u8> {
    let mut tar_data = Vec::new();
    {
        let mut tar = tar::Builder::new(&mut tar_data);
        let mut header = tar::Header::new_gnu();
        header.set_path(app_name).unwrap();
        header.set_size(content.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        tar.append(&header, content).unwrap();
        tar.finish().unwrap();
    }

    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&tar_data).unwrap();
    encoder.finish().unwrap()
}

fn calculate_sha256(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    format!("{:x}", hasher.finalize())
}

fn create_checksum_file(filename: &str, hash: &str) -> String {
    format!("{hash}  {filename}\n")
}

#[tokio::test]
async fn update_happy_path_with_checksum() {
    let mock_server = MockServer::start().await;

    let binary_content = b"#!/bin/sh\necho 'myapp v1.1.0'\n";
    let tar_gz = create_tar_gz_with_binary("myapp", binary_content);
    let checksum = calculate_sha256(&tar_gz);
    let checksum_file = create_checksum_file("myapp-1.1.0.tar.gz", &checksum);

    let release_json = serde_json::json!({
        "tag_name": "v1.1.0",
        "prerelease": false,
        "draft": false,
        "assets": [
            {
                "name": "myapp-1.1.0.tar.gz",
                "browser_download_url": format!("{}/download/myapp-1.1.0.tar.gz", mock_server.uri()),
                "size": tar_gz.len()
            },
            {
                "name": "SHA256SUMS",
                "browser_download_url": format!("{}/download/SHA256SUMS", mock_server.uri()),
                "size": checksum_file.len()
            }
        ]
    });

    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/releases/latest"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(&release_json)
                .insert_header("etag", "\"new-etag\"")
                .insert_header("last-modified", "Tue, 28 Oct 2025 12:00:00 GMT"),
        )
        .mount(&mock_server)
        .await;

    Mock::given(method("GET"))
        .and(path("/download/myapp-1.1.0.tar.gz"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(tar_gz))
        .mount(&mock_server)
        .await;

    Mock::given(method("GET"))
        .and(path("/download/SHA256SUMS"))
        .respond_with(ResponseTemplate::new(200).set_body_string(checksum_file))
        .mount(&mock_server)
        .await;

    let temp_dir = tempdir().unwrap();
    let state_dir = temp_dir.child("state");
    let install_root = temp_dir.child("opt");

    create_state_file(&state_dir, "myapp", "v1.0.0", "\"old-etag\"");
    create_installed_version(&install_root, "myapp", "v1.0.0");

    let mut cmd = cargo_bin_cmd!("distronomicon");
    let output = cmd
        .arg("--app")
        .arg("myapp")
        .arg("--install-root")
        .arg(install_root.as_str())
        .arg("update")
        .arg("--repo")
        .arg("owner/repo")
        .arg("--pattern")
        .arg("myapp-.*\\.tar\\.gz")
        .arg("--checksum-pattern")
        .arg("SHA256SUMS")
        .arg("--state-directory")
        .arg(state_dir.as_str())
        .arg("--github-host")
        .arg(mock_server.uri())
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0));

    // Verify new version installed
    let new_release_dir = install_root.join("myapp").join("releases").join("v1.1.0");
    assert!(
        new_release_dir.exists(),
        "New release directory should exist"
    );

    let binary_path = new_release_dir.join("myapp");
    assert!(
        binary_path.exists(),
        "Binary should exist in release directory"
    );

    // Verify symlinks updated
    let symlink_path = install_root.join("myapp").join("bin").join("myapp");
    assert!(symlink_path.exists(), "Symlink should exist");
    let link_target = fs::read_link(&symlink_path).unwrap();
    assert!(
        link_target.to_string_lossy().contains("v1.1.0"),
        "Symlink should point to new version"
    );

    // Verify state updated
    let state_path = state_dir.join("myapp").join("state.json");
    let state_contents = fs::read_to_string(&state_path).unwrap();
    let state: serde_json::Value = serde_json::from_str(&state_contents).unwrap();
    assert_eq!(state["latest_tag"].as_str(), Some("v1.1.0"));
    assert_eq!(state["etag"].as_str(), Some("\"new-etag\""));
}

#[tokio::test]
async fn update_no_matching_asset() {
    let mock_server = MockServer::start().await;

    let release_json = serde_json::json!({
        "tag_name": "v1.1.0",
        "prerelease": false,
        "draft": false,
        "assets": [
            {
                "name": "different-app.tar.gz",
                "browser_download_url": format!("{}/download/different-app.tar.gz", mock_server.uri()),
                "size": 1024
            }
        ]
    });

    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/releases/latest"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(&release_json)
                .insert_header("etag", "\"new-etag\"")
                .insert_header("last-modified", "Tue, 28 Oct 2025 12:00:00 GMT"),
        )
        .mount(&mock_server)
        .await;

    let temp_dir = tempdir().unwrap();
    let state_dir = temp_dir.child("state");
    let install_root = temp_dir.child("opt");

    create_state_file(&state_dir, "myapp", "v1.0.0", "\"old-etag\"");
    create_installed_version(&install_root, "myapp", "v1.0.0");

    let mut cmd = cargo_bin_cmd!("distronomicon");
    let output = cmd
        .arg("--app")
        .arg("myapp")
        .arg("--install-root")
        .arg(install_root.as_str())
        .arg("update")
        .arg("--repo")
        .arg("owner/repo")
        .arg("--pattern")
        .arg("myapp-.*\\.tar\\.gz")
        .arg("--state-directory")
        .arg(state_dir.as_str())
        .arg("--github-host")
        .arg(mock_server.uri())
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));

    // Verify old version still in place
    let old_release_dir = install_root.join("myapp").join("releases").join("v1.0.0");
    assert!(old_release_dir.exists(), "Old release should still exist");

    // Verify state unchanged
    let state_path = state_dir.join("myapp").join("state.json");
    let state_contents = fs::read_to_string(&state_path).unwrap();
    let state: serde_json::Value = serde_json::from_str(&state_contents).unwrap();
    assert_eq!(
        state["latest_tag"].as_str(),
        Some("v1.0.0"),
        "State should not be updated"
    );
}

#[tokio::test]
async fn update_checksum_mismatch() {
    let mock_server = MockServer::start().await;

    let binary_content = b"#!/bin/sh\necho 'myapp v1.1.0'\n";
    let tar_gz = create_tar_gz_with_binary("myapp", binary_content);
    let wrong_checksum = "0000000000000000000000000000000000000000000000000000000000000000";
    let checksum_file = create_checksum_file("myapp-1.1.0.tar.gz", wrong_checksum);

    let release_json = serde_json::json!({
        "tag_name": "v1.1.0",
        "prerelease": false,
        "draft": false,
        "assets": [
            {
                "name": "myapp-1.1.0.tar.gz",
                "browser_download_url": format!("{}/download/myapp-1.1.0.tar.gz", mock_server.uri()),
                "size": tar_gz.len()
            },
            {
                "name": "SHA256SUMS",
                "browser_download_url": format!("{}/download/SHA256SUMS", mock_server.uri()),
                "size": checksum_file.len()
            }
        ]
    });

    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/releases/latest"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(&release_json)
                .insert_header("etag", "\"new-etag\"")
                .insert_header("last-modified", "Tue, 28 Oct 2025 12:00:00 GMT"),
        )
        .mount(&mock_server)
        .await;

    Mock::given(method("GET"))
        .and(path("/download/myapp-1.1.0.tar.gz"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(tar_gz))
        .mount(&mock_server)
        .await;

    Mock::given(method("GET"))
        .and(path("/download/SHA256SUMS"))
        .respond_with(ResponseTemplate::new(200).set_body_string(checksum_file))
        .mount(&mock_server)
        .await;

    let temp_dir = tempdir().unwrap();
    let state_dir = temp_dir.child("state");
    let install_root = temp_dir.child("opt");

    create_state_file(&state_dir, "myapp", "v1.0.0", "\"old-etag\"");
    create_installed_version(&install_root, "myapp", "v1.0.0");

    let mut cmd = cargo_bin_cmd!("distronomicon");
    let output = cmd
        .arg("--app")
        .arg("myapp")
        .arg("--install-root")
        .arg(install_root.as_str())
        .arg("update")
        .arg("--repo")
        .arg("owner/repo")
        .arg("--pattern")
        .arg("myapp-.*\\.tar\\.gz")
        .arg("--checksum-pattern")
        .arg("SHA256SUMS")
        .arg("--state-directory")
        .arg(state_dir.as_str())
        .arg("--github-host")
        .arg(mock_server.uri())
        .output()
        .unwrap();

    assert!(
        !output.status.success(),
        "Expected failure due to checksum mismatch, got status: {:?}",
        output.status
    );
    assert_eq!(output.status.code(), Some(1));

    // Verify new version NOT installed
    let new_release_dir = install_root.join("myapp").join("releases").join("v1.1.0");
    assert!(
        !new_release_dir.exists(),
        "New release should not be installed"
    );

    // Verify state unchanged
    let state_path = state_dir.join("myapp").join("state.json");
    let state_contents = fs::read_to_string(&state_path).unwrap();
    let state: serde_json::Value = serde_json::from_str(&state_contents).unwrap();
    assert_eq!(state["latest_tag"].as_str(), Some("v1.0.0"));
}

#[tokio::test]
async fn update_restart_command_failure() {
    let mock_server = MockServer::start().await;

    let binary_content = b"#!/bin/sh\necho 'myapp v1.1.0'\n";
    let tar_gz = create_tar_gz_with_binary("myapp", binary_content);

    let release_json = serde_json::json!({
        "tag_name": "v1.1.0",
        "prerelease": false,
        "draft": false,
        "assets": [
            {
                "name": "myapp-1.1.0.tar.gz",
                "browser_download_url": format!("{}/download/myapp-1.1.0.tar.gz", mock_server.uri()),
                "size": tar_gz.len()
            }
        ]
    });

    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/releases/latest"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(&release_json)
                .insert_header("etag", "\"new-etag\"")
                .insert_header("last-modified", "Tue, 28 Oct 2025 12:00:00 GMT"),
        )
        .mount(&mock_server)
        .await;

    Mock::given(method("GET"))
        .and(path("/download/myapp-1.1.0.tar.gz"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(tar_gz))
        .mount(&mock_server)
        .await;

    let temp_dir = tempdir().unwrap();
    let state_dir = temp_dir.child("state");
    let install_root = temp_dir.child("opt");

    create_state_file(&state_dir, "myapp", "v1.0.0", "\"old-etag\"");
    create_installed_version(&install_root, "myapp", "v1.0.0");

    let mut cmd = cargo_bin_cmd!("distronomicon");
    let output = cmd
        .arg("--app")
        .arg("myapp")
        .arg("--install-root")
        .arg(install_root.as_str())
        .arg("update")
        .arg("--repo")
        .arg("owner/repo")
        .arg("--pattern")
        .arg("myapp-.*\\.tar\\.gz")
        .arg("--skip-verification")
        .arg("--restart-command")
        .arg("false") // Command that always fails
        .arg("--state-directory")
        .arg(state_dir.as_str())
        .arg("--github-host")
        .arg(mock_server.uri())
        .output()
        .unwrap();

    // Should exit with code 1 due to restart failure
    assert!(
        !output.status.success(),
        "Expected failure due to restart command, got status: {:?}",
        output.status
    );
    assert_eq!(output.status.code(), Some(1));

    // But symlinks SHOULD point to new version (no rollback)
    let symlink_path = install_root.join("myapp").join("bin").join("myapp");
    assert!(symlink_path.exists(), "Symlink should exist");
    let link_target = fs::read_link(&symlink_path).unwrap();
    assert!(
        link_target.to_string_lossy().contains("v1.1.0"),
        "Symlink should point to new version even though restart failed"
    );

    // And state SHOULD be saved
    let state_path = state_dir.join("myapp").join("state.json");
    let state_contents = fs::read_to_string(&state_path).unwrap();
    let state: serde_json::Value = serde_json::from_str(&state_contents).unwrap();
    assert_eq!(
        state["latest_tag"].as_str(),
        Some("v1.1.0"),
        "State should be updated even though restart failed"
    );
}

#[tokio::test]
async fn update_already_up_to_date() {
    let mock_server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/releases/latest"))
        .respond_with(
            ResponseTemplate::new(304)
                .insert_header("etag", "\"abc123\"")
                .insert_header("last-modified", "Mon, 27 Oct 2025 12:00:00 GMT"),
        )
        .mount(&mock_server)
        .await;

    let temp_dir = tempdir().unwrap();
    let state_dir = temp_dir.path().join("state");
    let install_root = temp_dir.path().join("opt");

    create_state_file(&state_dir, "myapp", "v1.0.0", "\"abc123\"");
    create_installed_version(&install_root, "myapp", "v1.0.0");

    let mut cmd = cargo_bin_cmd!("distronomicon");
    let output = cmd
        .arg("--app")
        .arg("myapp")
        .arg("--install-root")
        .arg(install_root.as_str())
        .arg("update")
        .arg("--repo")
        .arg("owner/repo")
        .arg("--pattern")
        .arg("myapp-.*\\.tar\\.gz")
        .arg("--state-directory")
        .arg(state_dir.as_str())
        .arg("--github-host")
        .arg(mock_server.uri())
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "Expected success (no-op), got status: {:?}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("up-to-date") || stdout.contains("already"),
        "Should indicate no update needed"
    );
}

#[tokio::test]
async fn update_skip_verification() {
    let mock_server = MockServer::start().await;

    let binary_content = b"#!/bin/sh\necho 'myapp v1.1.0'\n";
    let tar_gz = create_tar_gz_with_binary("myapp", binary_content);

    let release_json = serde_json::json!({
        "tag_name": "v1.1.0",
        "prerelease": false,
        "draft": false,
        "assets": [
            {
                "name": "myapp-1.1.0.tar.gz",
                "browser_download_url": format!("{}/download/myapp-1.1.0.tar.gz", mock_server.uri()),
                "size": tar_gz.len()
            }
        ]
    });

    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/releases/latest"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(&release_json)
                .insert_header("etag", "\"new-etag\"")
                .insert_header("last-modified", "Tue, 28 Oct 2025 12:00:00 GMT"),
        )
        .mount(&mock_server)
        .await;

    Mock::given(method("GET"))
        .and(path("/download/myapp-1.1.0.tar.gz"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(tar_gz))
        .mount(&mock_server)
        .await;

    let temp_dir = tempdir().unwrap();
    let state_dir = temp_dir.child("state");
    let install_root = temp_dir.child("opt");

    create_state_file(&state_dir, "myapp", "v1.0.0", "\"old-etag\"");
    create_installed_version(&install_root, "myapp", "v1.0.0");

    let mut cmd = cargo_bin_cmd!("distronomicon");
    let output = cmd
        .arg("--app")
        .arg("myapp")
        .arg("--install-root")
        .arg(install_root.as_str())
        .arg("update")
        .arg("--repo")
        .arg("owner/repo")
        .arg("--pattern")
        .arg("myapp-.*\\.tar\\.gz")
        .arg("--skip-verification")
        .arg("--state-directory")
        .arg(state_dir.as_str())
        .arg("--github-host")
        .arg(mock_server.uri())
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0));

    // Verify installation succeeded without checksum
    let new_release_dir = install_root.join("myapp").join("releases").join("v1.1.0");
    assert!(
        new_release_dir.exists(),
        "New release should be installed even without checksum verification"
    );

    let state_path = state_dir.join("myapp").join("state.json");
    let state_contents = fs::read_to_string(&state_path).unwrap();
    let state: serde_json::Value = serde_json::from_str(&state_contents).unwrap();
    assert_eq!(state["latest_tag"].as_str(), Some("v1.1.0"));
}

use std::{fs, io::Write as _, process::Output};

use assert_cmd::cargo::cargo_bin_cmd;
use camino_tempfile::tempdir;
use camino_tempfile_ext::prelude::*;
use insta::assert_snapshot;
use regex::Regex;
use sha2::{Digest as _, Sha256};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

fn create_zip_with_binary(app_name: &str, content: &[u8]) -> Vec<u8> {
    let mut zip_data = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut zip_data));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .unix_permissions(0o755);
        zip.start_file(app_name, options).unwrap();
        zip.write_all(content).unwrap();
        zip.finish().unwrap();
    }
    zip_data
}

fn calculate_sha256(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    format!("{:x}", hasher.finalize())
}

fn create_checksum_file(filename: &str, hash: &str) -> String {
    format!("{hash}  {filename}\n")
}

async fn setup_mock_server(
    mock_server: &MockServer,
    zip: &[u8],
    checksum_file: &str,
    status_code: u16,
) {
    let release_json = serde_json::json!({
        "tag_name": "v1.0.0",
        "prerelease": false,
        "draft": false,
        "assets": [
            {
                "name": "testapp-1.0.0.zip",
                "url": format!("{}/download/testapp-1.0.0.zip", mock_server.uri()),
                "browser_download_url": format!("{}/download/testapp-1.0.0.zip", mock_server.uri()),
                "size": zip.len()
            },
            {
                "name": "SHA256SUMS",
                "url": format!("{}/download/SHA256SUMS", mock_server.uri()),
                "browser_download_url": format!("{}/download/SHA256SUMS", mock_server.uri()),
                "size": checksum_file.len()
            }
        ]
    });

    let mut response = ResponseTemplate::new(status_code)
        .insert_header("etag", "\"v1.0.0-etag\"")
        .insert_header("last-modified", "Mon, 27 Oct 2025 10:00:00 GMT");

    if status_code == 200 {
        response = response.set_body_json(&release_json);
    }

    let mut mock = Mock::given(method("GET"))
        .and(path("/repos/owner/repo/releases/latest"))
        .respond_with(response);

    if status_code == 200 {
        mock = mock.up_to_n_times(1);
    }

    mock.mount(mock_server).await;

    if status_code == 200 {
        Mock::given(method("GET"))
            .and(path("/download/testapp-1.0.0.zip"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(zip))
            .up_to_n_times(1)
            .mount(mock_server)
            .await;

        Mock::given(method("GET"))
            .and(path("/download/SHA256SUMS"))
            .respond_with(ResponseTemplate::new(200).set_body_string(checksum_file))
            .up_to_n_times(1)
            .mount(mock_server)
            .await;
    }
}

fn run_update_command(install_root: &str, state_dir: &str, mock_server_uri: &str) -> Output {
    let mut cmd = cargo_bin_cmd!("distronomicon");
    cmd.env("NO_COLOR", "1")
        .arg("--app")
        .arg("testapp")
        .arg("--install-root")
        .arg(install_root)
        .arg("update")
        .arg("--repo")
        .arg("owner/repo")
        .arg("--pattern")
        .arg("testapp-.*\\.zip")
        .arg("--checksum-pattern")
        .arg("SHA256SUMS")
        .arg("--state-directory")
        .arg(state_dir)
        .arg("--github-host")
        .arg(mock_server_uri)
        .output()
        .unwrap()
}

fn run_version_command(install_root: &str) -> Output {
    let mut cmd = cargo_bin_cmd!("distronomicon");
    cmd.env("NO_COLOR", "1")
        .arg("--app")
        .arg("testapp")
        .arg("--install-root")
        .arg(install_root)
        .arg("version")
        .output()
        .unwrap()
}

fn run_unlock_command(state_dir: &str) -> Output {
    let mut cmd = cargo_bin_cmd!("distronomicon");
    cmd.env("NO_COLOR", "1")
        .arg("--app")
        .arg("testapp")
        .arg("unlock")
        .arg("--state-directory")
        .arg(state_dir)
        .output()
        .unwrap()
}

fn normalize_output(output: &Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout);

    let timestamp_re = Regex::new(r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d+Z").unwrap();
    let normalized = timestamp_re.replace_all(&stdout, "[TIMESTAMP]");

    let temp_path_re = Regex::new(r"dir=/[^\s:]+/\.tmp[^\s:]+").unwrap();
    temp_path_re
        .replace_all(&normalized, "dir=[TMPDIR]")
        .to_string()
}

#[tokio::test]
async fn update_then_version_then_noop_update() {
    let mock_server = MockServer::start().await;

    let binary_content = b"#!/bin/sh\necho 'testapp v1.0.0'\n";
    let zip = create_zip_with_binary("testapp", binary_content);
    let checksum = calculate_sha256(&zip);
    let checksum_file = create_checksum_file("testapp-1.0.0.zip", &checksum);

    setup_mock_server(&mock_server, &zip, &checksum_file, 200).await;

    let temp_dir = tempdir().unwrap();
    let state_dir = temp_dir.child("state");
    let install_root = temp_dir.child("opt");

    fs::create_dir_all(install_root.join("testapp").join("releases")).unwrap();
    fs::create_dir_all(state_dir.join("testapp")).unwrap();

    let output = run_update_command(
        install_root.as_str(),
        state_dir.as_str(),
        &mock_server.uri(),
    );
    assert_eq!(output.status.code(), Some(0));
    assert_snapshot!(normalize_output(&output));

    let release_dir = install_root.join("testapp").join("releases").join("v1.0.0");
    assert!(release_dir.exists());
    assert!(release_dir.join("testapp").exists());

    let symlink_path = install_root.join("testapp").join("bin").join("testapp");
    assert!(symlink_path.exists());
    let link_target = fs::read_link(&symlink_path).unwrap();
    assert!(link_target.ends_with("releases/v1.0.0/testapp"));

    let state_path = state_dir.join("testapp").join("state.json");
    let state_contents = fs::read_to_string(&state_path).unwrap();
    let state: serde_json::Value = serde_json::from_str(&state_contents).unwrap();
    assert_eq!(state["latest_tag"].as_str(), Some("v1.0.0"));
    assert_eq!(state["etag"].as_str(), Some("\"v1.0.0-etag\""));

    let output = run_version_command(install_root.as_str());
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "v1.0.0");

    setup_mock_server(&mock_server, &zip, &checksum_file, 304).await;

    let output = run_update_command(
        install_root.as_str(),
        state_dir.as_str(),
        &mock_server.uri(),
    );
    assert_eq!(output.status.code(), Some(0));
    assert_snapshot!(normalize_output(&output));
}

#[tokio::test]
async fn fresh_install_without_existing_directories() {
    let mock_server = MockServer::start().await;

    let binary_content = b"#!/bin/sh\necho 'testapp v1.0.0'\n";
    let zip = create_zip_with_binary("testapp", binary_content);
    let checksum = calculate_sha256(&zip);
    let checksum_file = create_checksum_file("testapp-1.0.0.zip", &checksum);

    setup_mock_server(&mock_server, &zip, &checksum_file, 200).await;

    let temp_dir = tempdir().unwrap();
    let state_dir = temp_dir.child("state");
    let install_root = temp_dir.child("opt");

    let output = run_update_command(
        install_root.as_str(),
        state_dir.as_str(),
        &mock_server.uri(),
    );
    assert_eq!(output.status.code(), Some(0));
    assert_snapshot!(normalize_output(&output));

    let release_dir = install_root.join("testapp").join("releases").join("v1.0.0");
    assert!(release_dir.exists());
    assert!(release_dir.join("testapp").exists());

    let symlink_path = install_root.join("testapp").join("bin").join("testapp");
    assert!(symlink_path.exists());
    let link_target = fs::read_link(&symlink_path).unwrap();
    assert!(link_target.ends_with("releases/v1.0.0/testapp"));

    let state_path = state_dir.join("testapp").join("state.json");
    assert!(state_path.exists());
    let state_contents = fs::read_to_string(&state_path).unwrap();
    let state: serde_json::Value = serde_json::from_str(&state_contents).unwrap();
    assert_eq!(state["latest_tag"].as_str(), Some("v1.0.0"));
    assert_eq!(state["etag"].as_str(), Some("\"v1.0.0-etag\""));
}

#[tokio::test]
async fn unlock_removes_stuck_lock_file() {
    let temp_dir = tempdir().unwrap();
    let state_dir = temp_dir.child("state");

    fs::create_dir_all(&state_dir).unwrap();

    let lock_file = state_dir.join("distronomicon-testapp.lock");
    fs::write(&lock_file, "").unwrap();
    assert!(lock_file.exists());

    let output = run_unlock_command(state_dir.as_str());

    assert_eq!(output.status.code(), Some(0));
    assert_snapshot!(normalize_output(&output));
    assert!(!lock_file.exists());
}

#[tokio::test]
async fn unlock_succeeds_when_no_lock_exists() {
    let temp_dir = tempdir().unwrap();
    let state_dir = temp_dir.child("state");

    fs::create_dir_all(&state_dir).unwrap();

    let lock_file = state_dir.join("distronomicon-testapp.lock");
    assert!(!lock_file.exists());

    let output = run_unlock_command(state_dir.as_str());

    assert_eq!(output.status.code(), Some(0));
    assert_snapshot!(normalize_output(&output));
}

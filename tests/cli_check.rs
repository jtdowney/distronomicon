use std::{fs, os::unix};

use assert_cmd::cargo::cargo_bin_cmd;
use camino::Utf8PathBuf;
use camino_tempfile::Utf8TempDir;
use jiff::Timestamp;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

fn create_state_file(state_dir: &Utf8PathBuf, app: &str, tag: &str, etag: &str) {
    let state_path = state_dir.join(app).join("state.json");
    fs::create_dir_all(state_path.parent().unwrap()).unwrap();

    let now = Timestamp::now();
    let state = serde_json::json!({
        "latest_tag": tag,
        "etag": etag,
        "last_modified": now.to_string(),
        "installed_at": now.to_string(),
    });

    fs::write(state_path, serde_json::to_string_pretty(&state).unwrap()).unwrap();
}

fn create_installed_version(install_root: &Utf8PathBuf, app: &str, tag: &str) {
    let releases_dir = install_root.join(app).join("releases").join(tag);
    let bin_dir = install_root.join(app).join("bin");

    fs::create_dir_all(&releases_dir).unwrap();
    fs::create_dir_all(&bin_dir).unwrap();

    let binary_path = releases_dir.join(app);
    fs::write(&binary_path, "fake binary").unwrap();

    let symlink_path = bin_dir.join(app);
    unix::fs::symlink(format!("../releases/{tag}/{app}"), symlink_path).unwrap();
}

#[tokio::test]
async fn check_with_304_not_modified() {
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

    let temp_dir = Utf8TempDir::new().unwrap();
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
        .arg("check")
        .arg("--repo")
        .arg("owner/repo")
        .arg("--state-directory")
        .arg(state_dir.as_str())
        .arg("--github-host")
        .arg(mock_server.uri())
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0));

    let stdout = String::from_utf8_lossy(&output.stdout);
    insta::assert_snapshot!(stdout);
}

#[tokio::test]
async fn check_with_update_available() {
    let mock_server = MockServer::start().await;

    let release_json = serde_json::json!({
        "tag_name": "v1.1.0",
        "prerelease": false,
        "draft": false,
        "assets": [{
            "name": "myapp.tar.gz",
            "browser_download_url": "https://github.com/owner/repo/releases/download/v1.1.0/myapp.tar.gz",
            "size": 1024
        }]
    });

    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/releases/latest"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(&release_json)
                .insert_header("etag", "\"def456\"")
                .insert_header("last-modified", "Tue, 28 Oct 2025 12:00:00 GMT"),
        )
        .mount(&mock_server)
        .await;

    let temp_dir = Utf8TempDir::new().unwrap();
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
        .arg("check")
        .arg("--repo")
        .arg("owner/repo")
        .arg("--state-directory")
        .arg(state_dir.as_str())
        .arg("--github-host")
        .arg(mock_server.uri())
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0));

    let stdout = String::from_utf8_lossy(&output.stdout);
    insta::assert_snapshot!(stdout);
}

#[tokio::test]
async fn check_no_current_version() {
    let mock_server = MockServer::start().await;

    let release_json = serde_json::json!({
        "tag_name": "v1.0.0",
        "prerelease": false,
        "draft": false,
        "assets": [{
            "name": "myapp.tar.gz",
            "browser_download_url": "https://github.com/owner/repo/releases/download/v1.0.0/myapp.tar.gz",
            "size": 1024
        }]
    });

    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/releases/latest"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(&release_json)
                .insert_header("etag", "\"abc123\"")
                .insert_header("last-modified", "Mon, 27 Oct 2025 12:00:00 GMT"),
        )
        .mount(&mock_server)
        .await;

    let temp_dir = Utf8TempDir::new().unwrap();
    let state_dir = temp_dir.path().join("state");
    let install_root = temp_dir.path().join("opt");

    let mut cmd = cargo_bin_cmd!("distronomicon");
    let output = cmd
        .arg("--app")
        .arg("myapp")
        .arg("--install-root")
        .arg(install_root.as_str())
        .arg("check")
        .arg("--repo")
        .arg("owner/repo")
        .arg("--state-directory")
        .arg(state_dir.as_str())
        .arg("--github-host")
        .arg(mock_server.uri())
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0));

    let stdout = String::from_utf8_lossy(&output.stdout);
    insta::assert_snapshot!(stdout);
}

#[tokio::test]
async fn check_network_error_exits_1() {
    let mock_server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/releases/latest"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&mock_server)
        .await;

    let temp_dir = Utf8TempDir::new().unwrap();
    let state_dir = temp_dir.path().join("state");
    let install_root = temp_dir.path().join("opt");

    let mut cmd = cargo_bin_cmd!("distronomicon");
    let output = cmd
        .arg("--app")
        .arg("myapp")
        .arg("--install-root")
        .arg(install_root.as_str())
        .arg("check")
        .arg("--repo")
        .arg("owner/repo")
        .arg("--state-directory")
        .arg(state_dir.as_str())
        .arg("--github-host")
        .arg(mock_server.uri())
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
}

#[tokio::test]
async fn check_first_run_no_state() {
    let mock_server = MockServer::start().await;

    let release_json = serde_json::json!({
        "tag_name": "v1.0.0",
        "prerelease": false,
        "draft": false,
        "assets": [{
            "name": "myapp.tar.gz",
            "browser_download_url": "https://github.com/owner/repo/releases/download/v1.0.0/myapp.tar.gz",
            "size": 1024
        }]
    });

    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/releases/latest"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(&release_json)
                .insert_header("etag", "\"abc123\"")
                .insert_header("last-modified", "Mon, 27 Oct 2025 12:00:00 GMT"),
        )
        .mount(&mock_server)
        .await;

    let temp_dir = Utf8TempDir::new().unwrap();
    let state_dir = temp_dir.path().join("state");
    let install_root = temp_dir.path().join("opt");

    let mut cmd = cargo_bin_cmd!("distronomicon");
    let output = cmd
        .arg("--app")
        .arg("myapp")
        .arg("--install-root")
        .arg(install_root.as_str())
        .arg("check")
        .arg("--repo")
        .arg("owner/repo")
        .arg("--state-directory")
        .arg(state_dir.as_str())
        .arg("--github-host")
        .arg(mock_server.uri())
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0));

    let stdout = String::from_utf8_lossy(&output.stdout);
    insta::assert_snapshot!(stdout);

    let state_path = state_dir.join("myapp").join("state.json");
    assert!(!state_path.exists());
}

#[tokio::test]
async fn state_validators_updated_on_304() {
    let mock_server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/releases/latest"))
        .respond_with(
            ResponseTemplate::new(304)
                .insert_header("etag", "\"new-etag\"")
                .insert_header("last-modified", "Wed, 29 Oct 2025 12:00:00 GMT"),
        )
        .mount(&mock_server)
        .await;

    let temp_dir = Utf8TempDir::new().unwrap();
    let state_dir = temp_dir.path().join("state");
    let install_root = temp_dir.path().join("opt");

    create_state_file(&state_dir, "myapp", "v1.0.0", "\"old-etag\"");
    create_installed_version(&install_root, "myapp", "v1.0.0");

    let mut cmd = cargo_bin_cmd!("distronomicon");
    let output = cmd
        .arg("--app")
        .arg("myapp")
        .arg("--install-root")
        .arg(install_root.as_str())
        .arg("check")
        .arg("--repo")
        .arg("owner/repo")
        .arg("--state-directory")
        .arg(state_dir.as_str())
        .arg("--github-host")
        .arg(mock_server.uri())
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0));

    let stdout = String::from_utf8_lossy(&output.stdout);
    insta::assert_snapshot!(stdout);

    let state_path = state_dir.join("myapp").join("state.json");
    assert!(state_path.exists());

    let state_contents = fs::read_to_string(&state_path).unwrap();
    let state: serde_json::Value = serde_json::from_str(&state_contents).unwrap();

    assert_eq!(state["etag"].as_str(), Some("\"new-etag\""));
}

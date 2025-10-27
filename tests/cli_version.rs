use std::{fs, os::unix};

use assert_cmd::cargo::cargo_bin_cmd;
use camino::Utf8PathBuf;
use camino_tempfile::tempdir;
use insta::assert_snapshot;

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

#[test]
fn version_with_no_installation() {
    let temp_dir = tempdir().unwrap();
    let install_root = temp_dir.path().join("opt");

    let mut cmd = cargo_bin_cmd!();
    cmd.arg("--app")
        .arg("myapp")
        .arg("--install-root")
        .arg(install_root.as_str())
        .arg("version");

    let output = cmd.output().unwrap();
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(stdout, "");
}

#[test]
fn version_with_installed_tag() {
    let temp_dir = tempdir().unwrap();
    let install_root = temp_dir.path().join("opt");

    create_installed_version(&install_root, "myapp", "v1.2.3");

    let mut cmd = cargo_bin_cmd!();
    cmd.arg("--app")
        .arg("myapp")
        .arg("--install-root")
        .arg(install_root.as_str())
        .arg("version");

    let output = cmd.output().unwrap();
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(stdout, "v1.2.3\n");
}

#[test]
fn version_verbose_shows_diagnostics() {
    let temp_dir = tempdir().unwrap();
    let install_root = temp_dir.path().join("opt");

    create_installed_version(&install_root, "myapp", "v1.2.3");

    let mut cmd = cargo_bin_cmd!();
    cmd.arg("--app")
        .arg("myapp")
        .arg("--install-root")
        .arg(install_root.as_str())
        .arg("-v")
        .arg("version");

    let output = cmd.output().unwrap();
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8(output.stdout).unwrap();

    let normalized = stdout.replace(install_root.as_str(), "/tmp/test");

    assert_snapshot!(normalized);
}

#[test]
fn version_works_without_github_flags() {
    let temp_dir = tempdir().unwrap();
    let install_root = temp_dir.path().join("opt");

    create_installed_version(&install_root, "myapp", "v1.2.3");

    let mut cmd = cargo_bin_cmd!();
    cmd.arg("--app")
        .arg("myapp")
        .arg("--install-root")
        .arg(install_root.as_str())
        .arg("version");

    let output = cmd.output().unwrap();
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(stdout, "v1.2.3\n");
}

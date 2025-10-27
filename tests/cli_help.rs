use assert_cmd::cargo::cargo_bin_cmd;
use insta::assert_snapshot;

#[test]
fn test_help_output() {
    let mut cmd = cargo_bin_cmd!("distronomicon");
    let output = cmd.arg("--help").output().unwrap();

    assert_eq!(output.status.code(), Some(0));

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_snapshot!(stdout);
}

#[test]
fn test_check_subcommand_help() {
    let mut cmd = cargo_bin_cmd!("distronomicon");
    let output = cmd.arg("check").arg("--help").output().unwrap();

    assert_eq!(output.status.code(), Some(0));

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_snapshot!(stdout);
}

#[test]
fn test_update_subcommand_help() {
    let mut cmd = cargo_bin_cmd!("distronomicon");
    let output = cmd.arg("update").arg("--help").output().unwrap();

    assert_eq!(output.status.code(), Some(0));

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_snapshot!(stdout);
}

#[test]
fn test_version_subcommand_help() {
    let mut cmd = cargo_bin_cmd!("distronomicon");
    let output = cmd.arg("version").arg("--help").output().unwrap();

    assert_eq!(output.status.code(), Some(0));

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_snapshot!(stdout);
}

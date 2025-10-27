use assert_cmd::Command;

#[test]
fn test_help_output() {
    let mut cmd = Command::cargo_bin("distronomicon").unwrap();
    let output = cmd.arg("--help").output().unwrap();

    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    insta::assert_snapshot!(stdout);
}

#[test]
fn test_check_subcommand_help() {
    let mut cmd = Command::cargo_bin("distronomicon").unwrap();
    let output = cmd.arg("check").arg("--help").output().unwrap();

    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    insta::assert_snapshot!(stdout);
}

#[test]
fn test_update_subcommand_help() {
    let mut cmd = Command::cargo_bin("distronomicon").unwrap();
    let output = cmd.arg("update").arg("--help").output().unwrap();

    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    insta::assert_snapshot!(stdout);
}

#[test]
fn test_version_subcommand_help() {
    let mut cmd = Command::cargo_bin("distronomicon").unwrap();
    let output = cmd.arg("version").arg("--help").output().unwrap();

    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    insta::assert_snapshot!(stdout);
}

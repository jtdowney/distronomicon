use assert_cmd::Command;
use insta::assert_snapshot;

#[test]
fn test_help_shows_subcommands() {
    let mut cmd = Command::cargo_bin("distronomicon").unwrap();
    let output = cmd.arg("--help").output().unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);

    assert_snapshot!(stdout);
}

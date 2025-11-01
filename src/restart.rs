use std::{io, process::Command};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum RestartError {
    #[error("command '{command}' failed with exit code {code}")]
    CommandFailed {
        command: String,
        code: i32,
        stdout: String,
        stderr: String,
    },
    #[error("failed to execute command: {0}")]
    Io(#[from] io::Error),
}

pub type Result<T> = std::result::Result<T, RestartError>;

/// Execute a shell command via `/bin/sh -c`.
///
/// Empty commands are treated as no-op and return `Ok(())`.
///
/// # Errors
///
/// Returns `RestartError::CommandFailed` if the command exits with a non-zero status code.
/// The error includes the command and exit code. Stdout and stderr are captured in the error
/// struct for debugging but not shown in the error message.
///
/// Returns `RestartError::Io` if the command cannot be executed (e.g., `/bin/sh` not found).
pub fn execute(cmd: &str) -> Result<()> {
    let output = Command::new("/bin/sh").arg("-c").arg(cmd).output()?;

    if !output.status.success() {
        let code = output.status.code().unwrap_or(-1);
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        return Err(RestartError::CommandFailed {
            command: cmd.to_string(),
            code,
            stdout,
            stderr,
        });
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use assert_matches::assert_matches;

    use super::*;

    #[test]
    fn test_execute_success() {
        let result = execute("true");
        assert!(result.is_ok());
    }

    #[test]
    fn test_execute_failure() {
        let result = execute("false");
        assert_matches!(result, Err(RestartError::CommandFailed { code: 1, .. }));
    }

    #[test]
    fn test_execute_empty_noop() {
        let result = execute("");
        assert!(result.is_ok());
    }

    #[test]
    fn test_execute_captures_stderr() {
        let result = execute("echo 'error message' >&2 && false");
        assert_matches!(
            result,
            Err(RestartError::CommandFailed { ref stderr, .. }) if stderr.contains("error message")
        );
    }

    #[test]
    fn test_execute_captures_stdout() {
        let result = execute("echo 'output' && false");
        assert_matches!(
            result,
            Err(RestartError::CommandFailed { ref stdout, .. }) if stdout.contains("output")
        );
    }

    #[test]
    fn test_execute_includes_command_in_error() {
        let cmd = "exit 42";
        let result = execute(cmd);
        assert_matches!(
            result,
            Err(RestartError::CommandFailed { ref command, code: 42, .. }) if command == cmd
        );
    }

    #[test]
    fn test_error_display_is_single_line() {
        let error = RestartError::CommandFailed {
            command: "systemctl restart myapp".to_string(),
            code: 1,
            stdout: "Some output".to_string(),
            stderr: "Permission denied".to_string(),
        };

        let display = format!("{error}");

        assert!(!display.contains('\n'));
        assert!(display.contains("systemctl restart myapp"));
        assert!(display.contains("exit code 1"));
    }
}

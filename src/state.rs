use std::{
    fs,
    io::{self, Write},
};

use camino::Utf8Path;
use camino_tempfile::NamedUtf8TempFile;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum StateError {
    #[error("IO error: {0}")]
    Io(#[from] io::Error),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, StateError>;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct State {
    pub latest_tag: String,
    pub etag: String,
    pub last_modified: jiff::Timestamp,
    pub installed_at: jiff::Timestamp,
}

/// Loads state from a JSON file.
///
/// Returns `Ok(None)` if the file does not exist.
///
/// # Errors
///
/// Returns an error if:
/// - The file cannot be read due to I/O errors
/// - The file contents are not valid JSON or don't match the `State` structure
pub fn load<P: AsRef<Utf8Path>>(path: P) -> Result<Option<State>> {
    let path = path.as_ref();
    if !path.exists() {
        return Ok(None);
    }

    let contents = fs::read_to_string(path)?;
    let state: State = serde_json::from_str(&contents)?;
    Ok(Some(state))
}

/// Atomically saves state to a JSON file.
///
/// Creates a temporary file in the parent directory, writes the state as JSON,
/// syncs both the file and parent directory, then atomically renames to the target path.
///
/// # Errors
///
/// Returns an error if:
/// - The path has no parent directory
/// - A temporary file cannot be created
/// - The state cannot be serialized to JSON
/// - Writing, syncing, or persisting the file fails
pub fn save_atomic<P: AsRef<Utf8Path>>(path: P, state: &State) -> Result<()> {
    let path = path.as_ref();
    let parent = path.parent().ok_or_else(|| {
        StateError::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            "path has no parent directory",
        ))
    })?;

    fs::create_dir_all(parent)?;

    let mut temp_file = NamedUtf8TempFile::new_in(parent)?;

    let json = serde_json::to_string_pretty(state)?;
    temp_file.write_all(json.as_bytes())?;
    temp_file.as_file().sync_all()?;
    temp_file.persist(path).map_err(|e| e.error)?;

    fs::File::open(parent)?.sync_all()?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use assert_matches::assert_matches;
    use camino_tempfile::tempdir;
    use camino_tempfile_ext::prelude::*;

    use super::*;

    #[test]
    fn test_load_missing_file() {
        let temp_dir = tempdir().unwrap();
        let state_path = temp_dir.child("state.json");

        let result = load(state_path);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), None);
    }

    #[test]
    fn test_save_and_load_roundtrip() {
        let temp_dir = tempdir().unwrap();
        let state_path = temp_dir.child("state.json");

        let original = State {
            latest_tag: "v1.2.3".to_string(),
            etag: "abc123".to_string(),
            last_modified: jiff::Timestamp::from_second(1_234_567_890).unwrap(),
            installed_at: jiff::Timestamp::from_second(1_234_567_900).unwrap(),
        };

        save_atomic(&state_path, &original).unwrap();
        let loaded = load(&state_path).unwrap().expect("state should exist");

        assert_eq!(loaded, original);
    }

    #[test]
    fn test_load_invalid_json_syntax() {
        let temp_dir = tempdir().unwrap();
        let state_path = temp_dir.child("state.json");

        state_path.write_str("{ invalid json syntax ").unwrap();

        let result = load(&state_path);
        assert_matches!(result, Err(StateError::Serialization(_)));
    }

    #[test]
    fn test_load_wrong_structure() {
        let temp_dir = tempdir().unwrap();
        let state_path = temp_dir.child("state.json");

        state_path.write_str(r#"{"wrong": "structure"}"#).unwrap();

        let result = load(&state_path);
        assert_matches!(result, Err(StateError::Serialization(_)));
    }

    #[test]
    fn test_save_atomic_no_parent_directory() {
        let state = State {
            latest_tag: "v1.0.0".to_string(),
            etag: "xyz789".to_string(),
            last_modified: jiff::Timestamp::from_second(1_000_000_000).unwrap(),
            installed_at: jiff::Timestamp::from_second(1_000_000_010).unwrap(),
        };

        let result = save_atomic("/", &state);
        assert_matches!(result, Err(StateError::Io(_)));
    }
}

//! Append-only provider output and normalized event ingestion.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::id::SessionId;

const TRANSCRIPT_DIRECTORY: &str = "transcripts";

/// A failure to create or extend a provider transcript.
#[derive(Debug, Error)]
pub(crate) enum TranscriptError {
    /// The transcript could not be written durably.
    #[error("could not write provider transcript: {0}")]
    Io(#[from] std::io::Error),
}

/// File-backed append-only transcript storage for one run.
pub(crate) struct TranscriptStore {
    run_state_directory: PathBuf,
}

impl TranscriptStore {
    /// Uses the run's durable state directory as the transcript root.
    pub(crate) fn new(run_state_directory: impl Into<PathBuf>) -> Self {
        Self {
            run_state_directory: run_state_directory.into(),
        }
    }

    /// Returns the database-safe path relative to the run state directory.
    pub(crate) fn relative_path(session_id: SessionId) -> PathBuf {
        Path::new(TRANSCRIPT_DIRECTORY).join(format!("{session_id}.jsonl"))
    }

    /// Appends provider bytes without rewriting any existing transcript data.
    pub(crate) fn append(
        &self,
        session_id: SessionId,
        bytes: &[u8],
    ) -> Result<(), TranscriptError> {
        let directory = self.run_state_directory.join(TRANSCRIPT_DIRECTORY);
        fs::create_dir_all(&directory)?;

        let path = self
            .run_state_directory
            .join(Self::relative_path(session_id));
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(bytes)?;
        file.sync_data()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::TranscriptStore;
    use crate::id::{RunId, SessionId};

    const SESSION_ID: &str = "cs-01ARZ3NDEKTSV4RRFFQ69G5FAY";

    #[test]
    fn provider_output_is_appended_to_a_file_outside_sqlite() {
        let directory = TestDirectory::new();
        let store = TranscriptStore::new(&directory.0);
        let session_id =
            SESSION_ID.parse::<SessionId>().expect("valid session ID");

        store
            .append(session_id, b"{\"turn\":1}\n")
            .expect("the first frame should append");
        store
            .append(session_id, b"{\"turn\":2}\n")
            .expect("the second frame should append");

        let relative_path = TranscriptStore::relative_path(session_id);
        assert_eq!(
            relative_path,
            PathBuf::from(format!("transcripts/{session_id}.jsonl"))
        );
        assert_eq!(
            fs::read(directory.0.join(relative_path))
                .expect("the transcript should be readable"),
            b"{\"turn\":1}\n{\"turn\":2}\n"
        );
    }

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let path = std::env::temp_dir()
                .join(format!("coterie-test-{}", RunId::generate()));
            fs::create_dir(&path).expect("the test directory should be unique");
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0)
                .expect("the test directory should be removable");
        }
    }
}

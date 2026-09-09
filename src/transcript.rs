//! Append-only provider output and normalized event ingestion.

use std::io::{Read, Seek, SeekFrom, Write};
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

    /// Reads a bounded byte page, preserving an incomplete final JSONL frame as data.
    pub(crate) fn read(
        &self,
        session_id: SessionId,
        after: u64,
        limit: u32,
    ) -> Result<TranscriptPage, TranscriptError> {
        crate::private_fs::check_directory(&self.run_state_directory)?;
        if !(1..=65536).contains(&limit) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "transcript limit must be between 1 and 65536",
            )
            .into());
        }
        let directory = self.run_state_directory.join(TRANSCRIPT_DIRECTORY);
        let result =
            crate::private_fs::check_directory(&directory).and_then(|()| {
                crate::private_fs::open(
                    &self
                        .run_state_directory
                        .join(Self::relative_path(session_id)),
                    false,
                    false,
                )
            });
        let mut file = match result {
            Ok(file) => file,
            Err(error)
                if error.kind() == std::io::ErrorKind::NotFound
                    && after == 0 =>
            {
                return Ok(TranscriptPage {
                    bytes: Vec::new(),
                    next_cursor: 0,
                    eof: true,
                    incomplete_tail: false,
                });
            }
            Err(error) => return Err(error.into()),
        };
        let length = file.metadata()?.len();
        if after > length {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "transcript cursor exceeds file length; the transcript may have been truncated").into());
        }
        file.seek(SeekFrom::Start(after))?;
        let mut bytes = Vec::new();
        Read::by_ref(&mut file)
            .take(u64::from(limit).min(length - after))
            .read_to_end(&mut bytes)?;
        // A byte limit may bisect a UTF-8 character. Read at most its three
        // remaining bytes so concatenating ordinary text pages remains lossless.
        for _ in 0..3 {
            let tail_start = bytes
                .iter()
                .rposition(|byte| byte & 0xc0 != 0x80)
                .unwrap_or(0);
            if std::str::from_utf8(&bytes[tail_start..])
                .is_err_and(|error| error.error_len().is_none())
                && after + (bytes.len() as u64) < length
            {
                let mut byte = [0];
                file.read_exact(&mut byte)?;
                bytes.push(byte[0]);
            } else {
                break;
            }
        }
        let next_cursor = after + bytes.len() as u64;
        let mut last = *b"\n";
        if length > 0 {
            file.seek(SeekFrom::Start(length - 1))?;
            file.read_exact(&mut last)?;
        }
        Ok(TranscriptPage {
            bytes,
            next_cursor,
            eof: next_cursor == length,
            incomplete_tail: last[0] != b'\n',
        })
    }

    /// Appends provider bytes without rewriting any existing transcript data.
    pub(crate) fn append(
        &self,
        session_id: SessionId,
        bytes: &[u8],
    ) -> Result<(), TranscriptError> {
        self.append_bytes(session_id, bytes)
    }

    fn append_bytes(
        &self,
        session_id: SessionId,
        bytes: &[u8],
    ) -> Result<(), TranscriptError> {
        crate::private_fs::check_directory(&self.run_state_directory)?;
        let directory = self.run_state_directory.join(TRANSCRIPT_DIRECTORY);
        crate::private_fs::directory(&directory)?;

        let path = self
            .run_state_directory
            .join(Self::relative_path(session_id));
        let mut file = crate::private_fs::open(&path, true, true)?;
        file.seek(SeekFrom::End(0))?;
        file.write_all(bytes)?;
        file.sync_data()?;
        Ok(())
    }
}

pub(crate) struct TranscriptPage {
    pub(crate) bytes: Vec<u8>,
    pub(crate) next_cursor: u64,
    pub(crate) eof: bool,
    pub(crate) incomplete_tail: bool,
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
            crate::private_fs::directory(&path)
                .expect("the test directory should be unique");
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0)
                .expect("the test directory should be removable");
        }
    }

    #[test]
    fn tail_pages_resume_without_loss_and_refuse_truncation_or_symlinks() {
        let directory = TestDirectory::new();
        let store = TranscriptStore::new(&directory.0);
        let session = SESSION_ID.parse().unwrap();
        store
            .append(session, b"{\"turn\":1}\n{\"partial\":")
            .unwrap();
        let mut bytes = Vec::new();
        let mut cursor = 0;
        loop {
            let page = store.read(session, cursor, 3).unwrap();
            assert!(page.incomplete_tail);
            assert!(page.bytes.len() <= 3);
            bytes.extend(page.bytes);
            cursor = page.next_cursor;
            if page.eof {
                break;
            }
        }
        assert_eq!(bytes, b"{\"turn\":1}\n{\"partial\":");
        store.append(session, b"2}\n").unwrap();
        let page = store.read(session, cursor, 3).unwrap();
        assert_eq!(page.bytes, b"2}\n");
        assert!(!page.incomplete_tail);
        let path = directory.0.join(TranscriptStore::relative_path(session));
        fs::write(&path, b"short").unwrap();
        assert!(store.read(session, cursor, 3).is_err());
        let preserved = directory.0.join("preserved");
        fs::rename(&path, &preserved).unwrap();
        std::os::unix::fs::symlink(&preserved, &path).unwrap();
        assert!(store.read(session, 0, 3).is_err());
        assert!(store.append(session, b"untrusted").is_err());
        assert_eq!(fs::read(preserved).unwrap(), b"short");
    }

    #[test]
    fn invalid_bytes_do_not_split_valid_utf8_in_later_pages() {
        let directory = TestDirectory::new();
        let store = TranscriptStore::new(&directory.0);
        let session = SESSION_ID.parse().unwrap();
        let bytes = b"\xff\xc3\xa9\n\xff\xe2\x82\xac\xff\xf0\x9f\xa6\x80\n";
        store.append(session, bytes).unwrap();
        for limit in 1..=bytes.len() as u32 {
            let mut cursor = 0;
            let mut result = String::new();
            loop {
                let page = store.read(session, cursor, limit).unwrap();
                assert!(page.bytes.len() <= limit as usize + 3);
                assert!(page.next_cursor > cursor);
                result.push_str(&String::from_utf8_lossy(&page.bytes));
                cursor = page.next_cursor;
                if page.eof {
                    break;
                }
            }
            assert_eq!(result, String::from_utf8_lossy(bytes), "limit {limit}");
        }
    }

    #[test]
    fn byte_pages_keep_utf8_characters_whole() {
        let directory = TestDirectory::new();
        let store = TranscriptStore::new(&directory.0);
        let session = SESSION_ID.parse().unwrap();
        let expected = "café 🦀\n";
        store.append(session, expected.as_bytes()).unwrap();
        let mut cursor = 0;
        let mut result = String::new();
        loop {
            let page = store.read(session, cursor, 1).unwrap();
            assert!(page.bytes.len() <= 4);
            result.push_str(std::str::from_utf8(&page.bytes).unwrap());
            cursor = page.next_cursor;
            if page.eof {
                break;
            }
        }
        assert_eq!(result, expected);
    }
}

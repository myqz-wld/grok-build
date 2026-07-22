use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, Write};
use std::path::{Path, PathBuf};

use fs2::FileExt;

use super::{ANNOTATION_SCHEMA_VERSION, AnnotationEvent, AnnotationState, AnnotationWarning};

pub const ANNOTATION_THREADS_FILE: &str = "annotation_threads.jsonl";

#[derive(Debug)]
pub struct AnnotationLoad {
    pub state: AnnotationState,
    pub warnings: Vec<AnnotationWarning>,
}

#[derive(Debug, Clone)]
pub struct AnnotationStore {
    path: PathBuf,
}

impl AnnotationStore {
    pub fn for_parent_session(session_id: &str) -> io::Result<Self> {
        if session_id.is_empty()
            || session_id.contains('/')
            || session_id.contains('\\')
            || session_id.contains("..")
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid parent session id",
            ));
        }
        let session_dir = xai_grok_shell::session::persistence::find_session_dir_by_id(session_id)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    "parent session directory not found",
                )
            })?;
        Ok(Self::at_session_dir(session_dir))
    }

    pub fn at_session_dir(session_dir: impl Into<PathBuf>) -> Self {
        Self {
            path: session_dir.into().join(ANNOTATION_THREADS_FILE),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn append(&self, event: &AnnotationEvent) -> io::Result<()> {
        if event.schema_version != ANNOTATION_SCHEMA_VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "unsupported annotation schema version",
            ));
        }
        let parent = self.path.parent().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "annotation log has no parent")
        })?;
        std::fs::create_dir_all(parent)?;
        let mut line = serde_json::to_vec(event)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        line.push(b'\n');

        let lock = self.open_lock()?;
        FileExt::lock_exclusive(&lock)?;
        let result = (|| {
            let mut file = OpenOptions::new()
                .read(true)
                .append(true)
                .create(true)
                .open(&self.path)?;
            let len = file.metadata()?.len();
            if len > 0 {
                file.seek(std::io::SeekFrom::Start(len - 1))?;
                let mut last = [0u8; 1];
                file.read_exact(&mut last)?;
                if last[0] != b'\n' {
                    line.insert(0, b'\n');
                }
            }
            file.write_all(&line)?;
            file.flush()?;
            file.sync_data()
        })();
        let _ = FileExt::unlock(&lock);
        result
    }

    pub fn load(&self) -> io::Result<AnnotationLoad> {
        if !self.path.exists() {
            return Ok(AnnotationLoad {
                state: AnnotationState::default(),
                warnings: Vec::new(),
            });
        }
        let lock = self.open_lock()?;
        FileExt::lock_shared(&lock)?;
        let bytes = std::fs::read(&self.path);
        let _ = FileExt::unlock(&lock);
        let bytes = bytes?;

        let ends_with_newline = bytes.last() == Some(&b'\n');
        let parts: Vec<&[u8]> = bytes.split(|byte| *byte == b'\n').collect();
        let mut events = Vec::new();
        let mut warnings = Vec::new();
        for (idx, line) in parts.iter().enumerate() {
            if line.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            let line_number = idx + 1;
            let is_incomplete_tail = !ends_with_newline && idx + 1 == parts.len();
            let value: serde_json::Value = match serde_json::from_slice(line) {
                Ok(value) => value,
                Err(error) if is_incomplete_tail => {
                    warnings.push(AnnotationWarning::IncompleteTail { line: line_number });
                    continue;
                }
                Err(error) => {
                    warnings.push(AnnotationWarning::MalformedLine {
                        line: line_number,
                        message: error.to_string(),
                    });
                    continue;
                }
            };
            let schema_version = value
                .get("schema_version")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or_default();
            if schema_version != u64::from(ANNOTATION_SCHEMA_VERSION) {
                warnings.push(AnnotationWarning::UnsupportedSchema {
                    line: line_number,
                    schema_version,
                });
                continue;
            }
            match serde_json::from_value::<AnnotationEvent>(value) {
                Ok(event) => events.push(event),
                Err(error) => warnings.push(AnnotationWarning::MalformedLine {
                    line: line_number,
                    message: error.to_string(),
                }),
            }
        }

        let (state, fold_warnings) = AnnotationState::fold(events);
        warnings.extend(fold_warnings);
        Ok(AnnotationLoad { state, warnings })
    }

    fn open_lock(&self) -> io::Result<File> {
        let lock_path = self.path.with_extension("jsonl.lock");
        OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(lock_path)
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use chrono::TimeZone;
    use tempfile::TempDir;

    use super::*;
    use crate::annotations::{
        AnnotationAnchor, AnnotationEntryRole, AnnotationEventKind, AnnotationExchangeStatus,
        AnnotationOrphanReason, AnnotationThreadAttachment, ThreadId, TranscriptEntry,
        TranscriptKey,
    };

    fn id(value: u128) -> uuid::Uuid {
        uuid::Uuid::from_u128(value)
    }

    fn anchor() -> AnnotationAnchor {
        let selected_text = "selected";
        let surrounding = "before\nselected\nafter";
        AnnotationAnchor {
            parent_session_id: "parent".to_string(),
            transcript_key: TranscriptKey {
                prompt_index: 3,
                role: AnnotationEntryRole::Assistant,
                ordinal: 0,
            },
            entry_role: AnnotationEntryRole::Assistant,
            target_prompt_index: 3,
            start_source_line: 2,
            end_source_line: 2,
            selected_text: selected_text.to_string(),
            selected_text_hash: blake3::hash(selected_text.as_bytes()).to_hex().to_string(),
            surrounding_text_hash: blake3::hash(surrounding.as_bytes()).to_hex().to_string(),
        }
    }

    fn event(event_id: u128, thread_id: ThreadId, kind: AnnotationEventKind) -> AnnotationEvent {
        AnnotationEvent {
            schema_version: ANNOTATION_SCHEMA_VERSION,
            event_id: id(event_id),
            timestamp: chrono::Utc.timestamp_opt(event_id as i64, 0).unwrap(),
            thread_id,
            kind,
        }
    }

    fn created(event_id: u128, thread_id: ThreadId) -> AnnotationEvent {
        event(
            event_id,
            thread_id,
            AnnotationEventKind::ThreadCreated {
                anchor: anchor(),
                child_session_id: "child".to_string(),
                first_question: "why?".to_string(),
            },
        )
    }

    #[test]
    fn round_trip_folds_streaming_exchange() {
        let temp = TempDir::new().unwrap();
        let store = AnnotationStore::at_session_dir(temp.path());
        let thread_id = id(10);
        let exchange_id = id(20);
        for event in [
            created(1, thread_id),
            event(
                2,
                thread_id,
                AnnotationEventKind::ExchangeStarted {
                    exchange_id,
                    question: "explain".to_string(),
                },
            ),
            event(
                3,
                thread_id,
                AnnotationEventKind::AnswerCheckpoint {
                    exchange_id,
                    markdown: "answer".to_string(),
                },
            ),
            event(
                4,
                thread_id,
                AnnotationEventKind::ExchangeCompleted { exchange_id },
            ),
        ] {
            store.append(&event).unwrap();
        }

        let loaded = store.load().unwrap();
        assert!(loaded.warnings.is_empty());
        let thread = &loaded.state.threads[&thread_id];
        assert_eq!(thread.first_question, "why?");
        assert_eq!(thread.exchanges.len(), 1);
        assert_eq!(thread.exchanges[0].answer_markdown, "answer");
        assert_eq!(
            thread.exchanges[0].status,
            AnnotationExchangeStatus::Completed
        );
    }

    #[test]
    fn incomplete_tail_is_ignored_and_next_append_is_healed() {
        let temp = TempDir::new().unwrap();
        let store = AnnotationStore::at_session_dir(temp.path());
        let thread_id = id(10);
        store.append(&created(1, thread_id)).unwrap();
        OpenOptions::new()
            .append(true)
            .open(store.path())
            .unwrap()
            .write_all(br#"{"schema_version":1,"event_id":"cut"#)
            .unwrap();

        let torn = store.load().unwrap();
        assert_eq!(torn.state.threads.len(), 1);
        assert!(matches!(
            torn.warnings.as_slice(),
            [AnnotationWarning::IncompleteTail { .. }]
        ));

        let exchange_id = id(20);
        store
            .append(&event(
                2,
                thread_id,
                AnnotationEventKind::ExchangeStarted {
                    exchange_id,
                    question: "follow-up".to_string(),
                },
            ))
            .unwrap();
        let healed = store.load().unwrap();
        assert_eq!(healed.state.threads[&thread_id].exchanges.len(), 1);
        assert!(
            healed
                .warnings
                .iter()
                .any(|warning| matches!(warning, AnnotationWarning::MalformedLine { .. }))
        );
    }

    #[test]
    fn delete_tombstone_and_duplicate_event_fold_deterministically() {
        let temp = TempDir::new().unwrap();
        let store = AnnotationStore::at_session_dir(temp.path());
        let thread_id = id(10);
        let create = created(1, thread_id);
        store.append(&create).unwrap();
        store.append(&create).unwrap();
        store
            .append(&event(2, thread_id, AnnotationEventKind::ThreadDeleted))
            .unwrap();

        let loaded = store.load().unwrap();
        assert_eq!(loaded.state.threads.len(), 1);
        assert!(loaded.state.threads[&thread_id].deleted);
        assert!(
            loaded
                .warnings
                .iter()
                .any(|warning| matches!(warning, AnnotationWarning::DuplicateEvent { .. }))
        );
    }

    #[test]
    fn missing_child_and_anchor_mismatch_become_explicit_orphans() {
        let thread_id = id(10);
        let (mut state, warnings) = AnnotationState::fold([created(1, thread_id)]);
        assert!(warnings.is_empty());
        let transcript = TranscriptEntry {
            key: anchor().transcript_key,
            role: AnnotationEntryRole::Assistant,
            target_prompt_index: 3,
            raw_text: "before\nselected\nafter".to_string(),
        };

        state.validate_threads(|_| Some(transcript.clone()), |_| false);
        assert_eq!(
            state.threads[&thread_id].attachment,
            AnnotationThreadAttachment::Orphaned(AnnotationOrphanReason::MissingChildSession)
        );

        let changed = TranscriptEntry {
            raw_text: "before\nchanged\nafter".to_string(),
            ..transcript
        };
        state.validate_threads(|_| Some(changed.clone()), |_| true);
        assert_eq!(
            state.threads[&thread_id].attachment,
            AnnotationThreadAttachment::Orphaned(AnnotationOrphanReason::AnchorMismatch)
        );
    }
}

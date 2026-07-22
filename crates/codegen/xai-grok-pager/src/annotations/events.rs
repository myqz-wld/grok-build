use std::collections::{BTreeMap, HashSet};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{AnnotationAnchor, TranscriptEntry, TranscriptKey, validate_annotation_anchor};

pub const ANNOTATION_SCHEMA_VERSION: u32 = 1;
pub const CHECKPOINT_MIN_BYTES: usize = 1024;
pub const CHECKPOINT_MAX_DELAY: Duration = Duration::from_secs(1);

pub type ThreadId = Uuid;
pub type ExchangeId = Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnnotationEvent {
    pub schema_version: u32,
    pub event_id: Uuid,
    pub timestamp: DateTime<Utc>,
    pub thread_id: ThreadId,
    #[serde(flatten)]
    pub kind: AnnotationEventKind,
}

impl AnnotationEvent {
    pub fn new(thread_id: ThreadId, kind: AnnotationEventKind) -> Self {
        Self {
            schema_version: ANNOTATION_SCHEMA_VERSION,
            event_id: Uuid::new_v4(),
            timestamp: Utc::now(),
            thread_id,
            kind,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum AnnotationEventKind {
    ThreadCreated {
        anchor: AnnotationAnchor,
        child_session_id: String,
        first_question: String,
    },
    ExchangeStarted {
        exchange_id: ExchangeId,
        question: String,
    },
    AnswerCheckpoint {
        exchange_id: ExchangeId,
        markdown: String,
    },
    ExchangeCompleted {
        exchange_id: ExchangeId,
    },
    ExchangeFailed {
        exchange_id: ExchangeId,
        message: String,
    },
    ThreadDeleted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnnotationExchangeStatus {
    Streaming,
    Completed,
    Failed { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnnotationExchange {
    pub exchange_id: ExchangeId,
    pub question: String,
    pub answer_markdown: String,
    pub status: AnnotationExchangeStatus,
    pub started_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnnotationOrphanReason {
    MissingChildSession,
    MissingTranscriptEntry,
    AnchorMismatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AnnotationThreadAttachment {
    #[default]
    Unvalidated,
    Attached,
    Orphaned(AnnotationOrphanReason),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnnotationThread {
    pub thread_id: ThreadId,
    pub anchor: AnnotationAnchor,
    pub child_session_id: String,
    pub first_question: String,
    pub exchanges: Vec<AnnotationExchange>,
    pub deleted: bool,
    pub attachment: AnnotationThreadAttachment,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnnotationWarning {
    IncompleteTail { line: usize },
    MalformedLine { line: usize, message: String },
    UnsupportedSchema { line: usize, schema_version: u64 },
    DuplicateEvent { event_id: Uuid },
    InvalidSequence { event_id: Uuid, message: String },
}

#[derive(Debug, Default)]
pub struct AnnotationState {
    pub threads: BTreeMap<ThreadId, AnnotationThread>,
    seen_event_ids: HashSet<Uuid>,
}

impl AnnotationState {
    pub fn fold(
        events: impl IntoIterator<Item = AnnotationEvent>,
    ) -> (Self, Vec<AnnotationWarning>) {
        let mut state = Self::default();
        let mut warnings = Vec::new();
        for event in events {
            if let Some(warning) = state.apply(event) {
                warnings.push(warning);
            }
        }
        (state, warnings)
    }

    pub fn apply(&mut self, event: AnnotationEvent) -> Option<AnnotationWarning> {
        if !self.seen_event_ids.insert(event.event_id) {
            return Some(AnnotationWarning::DuplicateEvent {
                event_id: event.event_id,
            });
        }

        let invalid = |message: &'static str| AnnotationWarning::InvalidSequence {
            event_id: event.event_id,
            message: message.to_string(),
        };
        match event.kind {
            AnnotationEventKind::ThreadCreated {
                anchor,
                child_session_id,
                first_question,
            } => {
                if self.threads.contains_key(&event.thread_id) {
                    return Some(invalid("thread was already created"));
                }
                self.threads.insert(
                    event.thread_id,
                    AnnotationThread {
                        thread_id: event.thread_id,
                        anchor,
                        child_session_id,
                        first_question,
                        exchanges: Vec::new(),
                        deleted: false,
                        attachment: AnnotationThreadAttachment::Unvalidated,
                        created_at: event.timestamp,
                        updated_at: event.timestamp,
                    },
                );
            }
            AnnotationEventKind::ExchangeStarted {
                exchange_id,
                question,
            } => {
                let Some(thread) = self.threads.get_mut(&event.thread_id) else {
                    return Some(invalid("exchange started before its thread"));
                };
                if thread.deleted {
                    return Some(invalid("exchange started after thread deletion"));
                }
                if thread
                    .exchanges
                    .iter()
                    .any(|exchange| exchange.exchange_id == exchange_id)
                {
                    return Some(invalid("exchange id was already started"));
                }
                thread.exchanges.push(AnnotationExchange {
                    exchange_id,
                    question,
                    answer_markdown: String::new(),
                    status: AnnotationExchangeStatus::Streaming,
                    started_at: event.timestamp,
                    updated_at: event.timestamp,
                });
                thread.updated_at = event.timestamp;
            }
            AnnotationEventKind::AnswerCheckpoint {
                exchange_id,
                markdown,
            } => {
                let Some(exchange) = self.exchange_mut(event.thread_id, exchange_id) else {
                    return Some(invalid("answer checkpoint references an unknown exchange"));
                };
                if !matches!(exchange.status, AnnotationExchangeStatus::Streaming) {
                    return Some(invalid("answer checkpoint arrived after terminal state"));
                }
                exchange.answer_markdown = markdown;
                exchange.updated_at = event.timestamp;
                self.touch_thread(event.thread_id, event.timestamp);
            }
            AnnotationEventKind::ExchangeCompleted { exchange_id } => {
                let Some(exchange) = self.exchange_mut(event.thread_id, exchange_id) else {
                    return Some(invalid("completion references an unknown exchange"));
                };
                exchange.status = AnnotationExchangeStatus::Completed;
                exchange.updated_at = event.timestamp;
                self.touch_thread(event.thread_id, event.timestamp);
            }
            AnnotationEventKind::ExchangeFailed {
                exchange_id,
                message,
            } => {
                let Some(exchange) = self.exchange_mut(event.thread_id, exchange_id) else {
                    return Some(invalid("failure references an unknown exchange"));
                };
                exchange.status = AnnotationExchangeStatus::Failed { message };
                exchange.updated_at = event.timestamp;
                self.touch_thread(event.thread_id, event.timestamp);
            }
            AnnotationEventKind::ThreadDeleted => {
                let Some(thread) = self.threads.get_mut(&event.thread_id) else {
                    return Some(invalid("deletion references an unknown thread"));
                };
                thread.deleted = true;
                thread.updated_at = event.timestamp;
            }
        }
        None
    }

    fn exchange_mut(
        &mut self,
        thread_id: ThreadId,
        exchange_id: ExchangeId,
    ) -> Option<&mut AnnotationExchange> {
        self.threads
            .get_mut(&thread_id)?
            .exchanges
            .iter_mut()
            .find(|exchange| exchange.exchange_id == exchange_id)
    }

    fn touch_thread(&mut self, thread_id: ThreadId, timestamp: DateTime<Utc>) {
        if let Some(thread) = self.threads.get_mut(&thread_id) {
            thread.updated_at = timestamp;
        }
    }

    pub fn validate_threads(
        &mut self,
        mut resolve_transcript: impl FnMut(&TranscriptKey) -> Option<TranscriptEntry>,
        mut child_exists: impl FnMut(&str) -> bool,
    ) {
        for thread in self.threads.values_mut().filter(|thread| !thread.deleted) {
            thread.attachment = if !child_exists(&thread.child_session_id) {
                AnnotationThreadAttachment::Orphaned(AnnotationOrphanReason::MissingChildSession)
            } else if let Some(transcript) = resolve_transcript(&thread.anchor.transcript_key) {
                if validate_annotation_anchor(&thread.anchor, &transcript).is_ok() {
                    AnnotationThreadAttachment::Attached
                } else {
                    AnnotationThreadAttachment::Orphaned(AnnotationOrphanReason::AnchorMismatch)
                }
            } else {
                AnnotationThreadAttachment::Orphaned(AnnotationOrphanReason::MissingTranscriptEntry)
            };
        }
    }
}

/// Coalesces streaming answer snapshots so persistence loses at most a bounded
/// time/byte suffix while avoiding one disk append per token.
#[derive(Debug, Clone)]
pub struct AnswerCheckpointGate {
    last_len: usize,
    last_at: Instant,
}

impl AnswerCheckpointGate {
    pub fn new(now: Instant) -> Self {
        Self {
            last_len: 0,
            last_at: now,
        }
    }

    pub fn checkpoint(&mut self, markdown: &str, now: Instant, force: bool) -> Option<String> {
        if markdown.len() <= self.last_len {
            return None;
        }
        let added = markdown.len() - self.last_len;
        let elapsed = now.saturating_duration_since(self.last_at);
        if !force && added < CHECKPOINT_MIN_BYTES && elapsed < CHECKPOINT_MAX_DELAY {
            return None;
        }
        self.last_len = markdown.len();
        self.last_at = now;
        Some(markdown.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checkpoint_gate_is_bounded_by_bytes_time_and_terminal_flush() {
        let start = Instant::now();
        let mut gate = AnswerCheckpointGate::new(start);
        assert_eq!(gate.checkpoint("small", start, false), None);
        assert_eq!(
            gate.checkpoint("small update", start + CHECKPOINT_MAX_DELAY, false),
            Some("small update".to_string())
        );

        let large = "x".repeat("small update".len() + CHECKPOINT_MIN_BYTES);
        assert_eq!(
            gate.checkpoint(&large, start + CHECKPOINT_MAX_DELAY, false),
            Some(large.clone())
        );
        let terminal = format!("{large}!");
        assert_eq!(
            gate.checkpoint(&terminal, start + CHECKPOINT_MAX_DELAY, true),
            Some(terminal)
        );
        assert_eq!(
            gate.checkpoint("terminal suffix", start + CHECKPOINT_MAX_DELAY, true),
            None,
            "a shrinking snapshot is rejected rather than corrupting the answer"
        );
    }
}

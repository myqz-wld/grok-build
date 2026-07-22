//! Persistent inline-annotation domain types and storage.

mod anchor;
mod events;
mod runtime;
mod storage;

pub(crate) use anchor::resolve_transcript_key_with_index;
pub use anchor::{
    AnchorValidationError, AnnotationAnchor, AnnotationEntryRole, AnnotationSelectionError,
    TranscriptEntry, TranscriptKey, build_annotation_anchor, resolve_transcript_entry,
    resolve_transcript_key, validate_annotation_anchor,
};
pub use events::{
    ANNOTATION_SCHEMA_VERSION, AnnotationEvent, AnnotationEventKind, AnnotationExchange,
    AnnotationExchangeStatus, AnnotationOrphanReason, AnnotationState, AnnotationThread,
    AnnotationThreadAttachment, AnnotationWarning, AnswerCheckpointGate, ExchangeId, ThreadId,
};
pub(crate) use runtime::{
    AnnotationExchangePhase, AnnotationInFlight, AnnotationPersistContinuation, AnnotationRuntime,
    PendingAnnotationFork,
};
pub use storage::{ANNOTATION_THREADS_FILE, AnnotationLoad, AnnotationStore};

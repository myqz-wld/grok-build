use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::time::Instant;

use super::{
    AnnotationAnchor, AnnotationEvent, AnnotationExchangeStatus, AnnotationState,
    AnnotationWarning, AnswerCheckpointGate, ExchangeId, ThreadId,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PendingAnnotationFork {
    Forking {
        anchor: AnnotationAnchor,
        question: String,
        exchange_id: ExchangeId,
    },
    Failed {
        anchor: AnnotationAnchor,
        question: String,
        exchange_id: ExchangeId,
        message: String,
    },
}

impl PendingAnnotationFork {
    pub(crate) fn anchor(&self) -> &AnnotationAnchor {
        match self {
            Self::Forking { anchor, .. } | Self::Failed { anchor, .. } => anchor,
        }
    }

    pub(crate) fn question(&self) -> &str {
        match self {
            Self::Forking { question, .. } | Self::Failed { question, .. } => question,
        }
    }

    pub(crate) fn exchange_id(&self) -> ExchangeId {
        match self {
            Self::Forking { exchange_id, .. } | Self::Failed { exchange_id, .. } => *exchange_id,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AnnotationExchangePhase {
    Persisting,
    LoadingChild,
    Prompting,
    Cancelling,
}

#[derive(Debug, Clone)]
pub(crate) struct AnnotationInFlight {
    pub(crate) exchange_id: ExchangeId,
    pub(crate) question: String,
    pub(crate) prompt_id: String,
    pub(crate) phase: AnnotationExchangePhase,
    pub(crate) checkpoint_gate: AnswerCheckpointGate,
}

impl AnnotationInFlight {
    pub(crate) fn new(
        exchange_id: ExchangeId,
        question: String,
        phase: AnnotationExchangePhase,
    ) -> Self {
        Self {
            exchange_id,
            question,
            prompt_id: format!("annotation-{exchange_id}"),
            phase,
            checkpoint_gate: AnswerCheckpointGate::new(Instant::now()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AnnotationPersistContinuation {
    None,
    StartExchange { thread_id: ThreadId },
}

#[derive(Debug, Clone)]
pub(crate) struct AnnotationPersistRequest {
    pub(crate) event: AnnotationEvent,
    pub(crate) continuation: AnnotationPersistContinuation,
}

#[derive(Debug, Default)]
pub(crate) struct AnnotationRuntime {
    pub(crate) state: AnnotationState,
    /// Child session id -> annotation thread id.
    pub(crate) sessions: HashMap<String, ThreadId>,
    pub(crate) pending_forks: BTreeMap<ThreadId, PendingAnnotationFork>,
    pub(crate) in_flight: HashMap<ThreadId, AnnotationInFlight>,
    pub(crate) loaded_sessions: HashSet<String>,
    pub(crate) loading_sessions: HashSet<String>,
    pub(crate) last_event_seq: HashMap<String, u64>,
    pub(crate) persist_queue: VecDeque<AnnotationPersistRequest>,
    pub(crate) persist_in_flight: bool,
    pub(crate) warnings: Vec<AnnotationWarning>,
    pub(crate) last_error: Option<String>,
    pub(crate) restoring: bool,
}

impl AnnotationRuntime {
    pub(crate) fn restore(&mut self, mut state: AnnotationState, warnings: Vec<AnnotationWarning>) {
        for exchange in state
            .threads
            .values_mut()
            .flat_map(|thread| thread.exchanges.iter_mut())
        {
            if matches!(exchange.status, AnnotationExchangeStatus::Streaming) {
                exchange.status = AnnotationExchangeStatus::Failed {
                    message: "Interrupted before completion; ask a follow-up to continue".into(),
                };
            }
        }
        self.sessions = state
            .threads
            .values()
            .filter(|thread| !thread.deleted)
            .map(|thread| (thread.child_session_id.clone(), thread.thread_id))
            .collect();
        self.state = state;
        self.pending_forks.clear();
        self.in_flight.clear();
        self.loaded_sessions.clear();
        self.loading_sessions.clear();
        self.last_event_seq.clear();
        self.persist_queue.clear();
        self.persist_in_flight = false;
        self.warnings = warnings;
        self.last_error = None;
        self.restoring = false;
    }

    pub(crate) fn enqueue_persist(
        &mut self,
        event: AnnotationEvent,
        continuation: AnnotationPersistContinuation,
    ) {
        self.persist_queue.push_back(AnnotationPersistRequest {
            event,
            continuation,
        });
    }

    pub(crate) fn start_next_persist(&mut self) -> Option<AnnotationEvent> {
        if self.persist_in_flight {
            return None;
        }
        let event = self.persist_queue.front()?.event.clone();
        self.persist_in_flight = true;
        Some(event)
    }

    pub(crate) fn finish_persist(
        &mut self,
        event_id: uuid::Uuid,
    ) -> Option<AnnotationPersistContinuation> {
        let front = self.persist_queue.front()?;
        if front.event.event_id != event_id {
            return None;
        }
        let request = self.persist_queue.pop_front()?;
        self.persist_in_flight = false;
        Some(request.continuation)
    }

    pub(crate) fn fail_persist(
        &mut self,
        event_id: uuid::Uuid,
        message: String,
    ) -> Option<AnnotationPersistRequest> {
        if !self
            .persist_queue
            .front()
            .is_some_and(|request| request.event.event_id == event_id)
        {
            return None;
        }
        let failed = self.persist_queue.pop_front()?;
        self.persist_queue.clear();
        self.persist_in_flight = false;
        self.last_error = Some(message);
        Some(failed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::annotations::{ANNOTATION_SCHEMA_VERSION, AnnotationEventKind};

    #[test]
    fn persistence_queue_is_strict_fifo() {
        let thread_id = uuid::Uuid::from_u128(1);
        let first = AnnotationEvent::new(thread_id, AnnotationEventKind::ThreadDeleted);
        let second = AnnotationEvent::new(thread_id, AnnotationEventKind::ThreadDeleted);
        let mut runtime = AnnotationRuntime::default();
        runtime.enqueue_persist(first.clone(), AnnotationPersistContinuation::None);
        runtime.enqueue_persist(second.clone(), AnnotationPersistContinuation::None);

        assert_eq!(
            runtime.start_next_persist().map(|event| event.event_id),
            Some(first.event_id)
        );
        assert!(runtime.start_next_persist().is_none());
        assert_eq!(
            runtime.finish_persist(first.event_id),
            Some(AnnotationPersistContinuation::None)
        );
        assert_eq!(
            runtime.start_next_persist().map(|event| event.event_id),
            Some(second.event_id)
        );
        assert_eq!(first.schema_version, ANNOTATION_SCHEMA_VERSION);
    }
}

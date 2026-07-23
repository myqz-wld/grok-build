use agent_client_protocol as acp;

use crate::annotations::{
    AnnotationState, AnnotationWarning, ExchangeId, ThreadId, resolve_transcript_key,
};
use crate::app::actions::Effect;
use crate::app::agent::AgentId;
use crate::app::app_view::AppView;

fn active_root_agent_id(app: &AppView) -> Option<AgentId> {
    match app.active_view {
        crate::app::app_view::ActiveView::Agent(agent_id) => Some(agent_id),
        _ => None,
    }
}

fn annotation_warning_detail(warning: &AnnotationWarning) -> String {
    match warning {
        AnnotationWarning::IncompleteTail { line } => {
            format!("incomplete final record at line {line}")
        }
        AnnotationWarning::MalformedLine { line, .. } => {
            format!("malformed record at line {line}")
        }
        AnnotationWarning::UnsupportedSchema {
            line,
            schema_version,
        } => format!("unsupported schema {schema_version} at line {line}"),
        AnnotationWarning::DuplicateEvent { event_id } => {
            format!("duplicate event {event_id}")
        }
        AnnotationWarning::InvalidSequence { event_id, .. } => {
            format!("invalid event order near {event_id}")
        }
    }
}

fn annotation_warning_toast(warnings: &[AnnotationWarning]) -> Option<String> {
    let first = warnings.first()?;
    let count = warnings.len();
    let noun = if count == 1 { "warning" } else { "warnings" };
    Some(format!(
        "Annotations recovered with {count} {noun}: {}",
        annotation_warning_detail(first)
    ))
}

pub(super) fn dispatch_begin_inline_annotation(
    app: &mut AppView,
    anchor: crate::annotations::AnnotationAnchor,
    question: String,
) -> Vec<Effect> {
    if crate::app::minimal_mode_active() {
        return Vec::new();
    }
    let Some(agent_id) = active_root_agent_id(app) else {
        return Vec::new();
    };
    let Some(agent) = app.agents.get_mut(&agent_id) else {
        return Vec::new();
    };
    match agent.begin_annotation(agent_id, anchor, question) {
        Ok(effect) => vec![effect],
        Err(message) => {
            agent.show_toast(&message);
            Vec::new()
        }
    }
}

pub(super) fn dispatch_follow_up_inline_annotation(
    app: &mut AppView,
    thread_id: ThreadId,
    question: String,
    selected_annotation_text: Option<String>,
) -> Vec<Effect> {
    if crate::app::minimal_mode_active() {
        return Vec::new();
    }
    let Some(agent_id) = active_root_agent_id(app) else {
        return Vec::new();
    };
    let Some(agent) = app.agents.get_mut(&agent_id) else {
        return Vec::new();
    };
    match agent.begin_annotation_follow_up(agent_id, thread_id, question, selected_annotation_text)
    {
        Ok(effects) => effects,
        Err(message) => {
            agent.show_toast(&message);
            Vec::new()
        }
    }
}

pub(super) fn dispatch_cancel_inline_annotation(
    app: &mut AppView,
    thread_id: ThreadId,
) -> Vec<Effect> {
    if crate::app::minimal_mode_active() {
        return Vec::new();
    }
    let Some(agent_id) = active_root_agent_id(app) else {
        return Vec::new();
    };
    let Some(agent) = app.agents.get_mut(&agent_id) else {
        return Vec::new();
    };
    match agent.cancel_annotation(agent_id, thread_id) {
        Ok(effects) => effects,
        Err(message) => {
            agent.show_toast(&message);
            Vec::new()
        }
    }
}

pub(super) fn dispatch_delete_inline_annotation(
    app: &mut AppView,
    thread_id: ThreadId,
) -> Vec<Effect> {
    if crate::app::minimal_mode_active() {
        return Vec::new();
    }
    let Some(agent_id) = active_root_agent_id(app) else {
        return Vec::new();
    };
    let Some(agent) = app.agents.get_mut(&agent_id) else {
        return Vec::new();
    };
    match agent.delete_annotation(agent_id, thread_id) {
        Ok(effects) => effects,
        Err(message) => {
            agent.show_toast(&message);
            Vec::new()
        }
    }
}

pub(super) fn handle_annotation_state_loaded(
    app: &mut AppView,
    agent_id: AgentId,
    parent_session_id: String,
    mut state: AnnotationState,
    warnings: Vec<AnnotationWarning>,
    existing_child_sessions: std::collections::HashSet<String>,
) -> Vec<Effect> {
    let Some(agent) = app.agents.get_mut(&agent_id) else {
        return Vec::new();
    };
    if agent
        .session
        .session_id
        .as_ref()
        .map(ToString::to_string)
        .as_deref()
        != Some(parent_session_id.as_str())
    {
        return Vec::new();
    }
    state.validate_threads(
        |key| resolve_transcript_key(&agent.scrollback, key),
        |child_session_id| existing_child_sessions.contains(child_session_id),
    );
    let warning_toast = annotation_warning_toast(&warnings);
    agent.annotation_runtime.restore(state, warnings);
    if let Some(message) = warning_toast {
        agent.show_toast(&message);
    }
    Vec::new()
}

pub(super) fn handle_annotation_state_load_failed(
    app: &mut AppView,
    agent_id: AgentId,
    parent_session_id: &str,
    error: String,
) -> Vec<Effect> {
    let Some(agent) = app.agents.get_mut(&agent_id) else {
        return Vec::new();
    };
    if agent
        .session
        .session_id
        .as_ref()
        .map(ToString::to_string)
        .as_deref()
        != Some(parent_session_id)
    {
        return Vec::new();
    }
    agent.annotation_runtime.restoring = false;
    agent.annotation_runtime.last_error = Some(error.clone());
    agent.show_toast(&format!("Couldn't load annotations: {error}"));
    Vec::new()
}

pub(super) fn handle_annotation_fork_ready(
    app: &mut AppView,
    agent_id: AgentId,
    thread_id: ThreadId,
    exchange_id: ExchangeId,
    child_session_id: acp::SessionId,
) -> Vec<Effect> {
    app.agents
        .get_mut(&agent_id)
        .map(|agent| {
            agent.annotation_fork_ready(agent_id, thread_id, exchange_id, child_session_id)
        })
        .unwrap_or_default()
}

pub(super) fn handle_annotation_fork_failed(
    app: &mut AppView,
    agent_id: AgentId,
    thread_id: ThreadId,
    exchange_id: ExchangeId,
    error: String,
) -> Vec<Effect> {
    if let Some(agent) = app.agents.get_mut(&agent_id) {
        agent.annotation_fork_failed(thread_id, exchange_id, error);
    }
    Vec::new()
}

pub(super) fn handle_annotation_event_persisted(
    app: &mut AppView,
    agent_id: AgentId,
    event_id: uuid::Uuid,
) -> Vec<Effect> {
    app.agents
        .get_mut(&agent_id)
        .map(|agent| agent.annotation_persist_finished(agent_id, event_id))
        .unwrap_or_default()
}

pub(super) fn handle_annotation_event_persist_failed(
    app: &mut AppView,
    agent_id: AgentId,
    event_id: uuid::Uuid,
    error: String,
) -> Vec<Effect> {
    app.agents
        .get_mut(&agent_id)
        .map(|agent| agent.annotation_persist_failed(agent_id, event_id, error))
        .unwrap_or_default()
}

pub(super) fn handle_annotation_session_loaded(
    app: &mut AppView,
    agent_id: AgentId,
    thread_id: ThreadId,
    exchange_id: ExchangeId,
    session_id: acp::SessionId,
) -> Vec<Effect> {
    app.agents
        .get_mut(&agent_id)
        .map(|agent| agent.annotation_session_loaded(agent_id, thread_id, exchange_id, &session_id))
        .unwrap_or_default()
}

pub(super) fn handle_annotation_session_load_failed(
    app: &mut AppView,
    agent_id: AgentId,
    thread_id: ThreadId,
    exchange_id: ExchangeId,
    session_id: acp::SessionId,
    error: String,
) -> Vec<Effect> {
    app.agents
        .get_mut(&agent_id)
        .map(|agent| {
            agent.annotation_session_load_failed(
                agent_id,
                thread_id,
                exchange_id,
                &session_id,
                error,
            )
        })
        .unwrap_or_default()
}

pub(super) fn handle_annotation_prompt_finished(
    app: &mut AppView,
    agent_id: AgentId,
    thread_id: ThreadId,
    exchange_id: ExchangeId,
    prompt_id: String,
    result: Result<acp::PromptResponse, String>,
) -> Vec<Effect> {
    app.agents
        .get_mut(&agent_id)
        .map(|agent| {
            agent.annotation_prompt_finished(agent_id, thread_id, exchange_id, &prompt_id, result)
        })
        .unwrap_or_default()
}

pub(super) fn handle_annotation_cancel_finished(
    app: &mut AppView,
    agent_id: AgentId,
    thread_id: ThreadId,
    exchange_id: ExchangeId,
    result: Result<(), String>,
) -> Vec<Effect> {
    app.agents
        .get_mut(&agent_id)
        .map(|agent| agent.annotation_cancel_finished(agent_id, thread_id, exchange_id, result))
        .unwrap_or_default()
}

//! Inline-annotation dispatch recovery feedback.

use super::super::annotations::{
    handle_annotation_state_load_failed, handle_annotation_state_loaded,
};
use super::*;
use crate::annotations::{AnnotationState, AnnotationWarning};

#[test]
fn annotation_load_warning_is_visible_without_blocking_recovery() {
    let mut app = test_app_with_agent();
    let warning = AnnotationWarning::MalformedLine {
        line: 7,
        message: "bad json".into(),
    };

    let effects = handle_annotation_state_loaded(
        &mut app,
        AgentId(0),
        "test-session".into(),
        AnnotationState::default(),
        vec![warning.clone()],
        Default::default(),
    );

    assert!(effects.is_empty());
    assert_eq!(
        app.agents[&AgentId(0)].annotation_runtime.warnings,
        vec![warning]
    );
    let toast = app.agents[&AgentId(0)].toast.as_ref().unwrap().0.as_str();
    assert!(toast.contains("Annotations recovered with 1 warning"));
    assert!(toast.contains("line 7"));
}

#[test]
fn annotation_load_error_is_visible_and_retained_for_diagnostics() {
    let mut app = test_app_with_agent();
    app.agents
        .get_mut(&AgentId(0))
        .unwrap()
        .annotation_runtime
        .restoring = true;

    let effects = handle_annotation_state_load_failed(
        &mut app,
        AgentId(0),
        "test-session",
        "permission denied".into(),
    );

    assert!(effects.is_empty());
    let agent = &app.agents[&AgentId(0)];
    assert!(!agent.annotation_runtime.restoring);
    assert_eq!(
        agent.annotation_runtime.last_error.as_deref(),
        Some("permission denied")
    );
    assert!(
        agent
            .toast
            .as_ref()
            .unwrap()
            .0
            .contains("Couldn't load annotations: permission denied")
    );
}

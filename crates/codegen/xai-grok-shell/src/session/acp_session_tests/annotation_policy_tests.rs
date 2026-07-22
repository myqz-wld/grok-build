use super::support::create_test_actor_ex;
use super::*;

#[tokio::test(flavor = "current_thread")]
async fn annotation_actor_exposes_no_turn_capabilities() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _gateway_rx) = tokio::sync::mpsc::unbounded_channel();
            let (persistence_tx, _persistence_rx) = tokio::sync::mpsc::unbounded_channel();
            let (mut actor, _event_rx) =
                create_test_actor_ex(0, 128_000, 90, gateway_tx, persistence_tx).await;
            actor.startup_hints.actor_policy = SessionActorPolicy::Annotation;
            actor.supports_backend_search.set(true);
            actor.memory.initial_injection_config.enabled = true;

            let (definitions, wait_ms) = tokio::time::timeout(
                std::time::Duration::from_millis(100),
                actor.prepare_tool_definitions_timed(),
            )
            .await
            .expect("annotation policy must bypass blocking MCP initialization");
            assert!(definitions.is_empty());
            assert_eq!(wait_ms, 0);
            assert!(
                actor
                    .turn_base_tool_specs(&[ToolDefinition::function(
                        "dangerous",
                        None::<&str>,
                        serde_json::json!({ "type": "object" }),
                    )])
                    .is_empty()
            );
            assert!(!actor.backend_search_allowed());
            assert!(!actor.startup_hints.actor_policy.allows_memory());
            assert_eq!(
                actor
                    .startup_hints
                    .actor_policy
                    .filter_json_schema(Some(serde_json::json!({ "type": "object" }))),
                None
            );
            assert_eq!(actor.first_turn_memory_reminder().await, None);
            assert!(
                !actor
                    .memory
                    .context_injected
                    .load(std::sync::atomic::Ordering::Relaxed),
                "annotation policy must return before touching memory injection state"
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn annotation_actor_rejects_unexpected_tool_calls_before_dispatch() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _gateway_rx) = tokio::sync::mpsc::unbounded_channel();
            let (persistence_tx, _persistence_rx) = tokio::sync::mpsc::unbounded_channel();
            let (mut actor, mut event_rx) =
                create_test_actor_ex(0, 128_000, 90, gateway_tx, persistence_tx).await;
            actor.startup_hints.actor_policy = SessionActorPolicy::Annotation;

            let err = actor
                .execute_tool_calls(vec![crate::sampling::types::ToolCallResponse {
                    id: "call-1".into(),
                    kind: "function".into(),
                    function: crate::sampling::types::ToolCallFunction::new(
                        "bash",
                        r#"{"cmd":"touch nope"}"#,
                    ),
                }])
                .await
                .unwrap_err();

            assert_eq!(err.code, acp::Error::invalid_request().code);
            assert!(format!("{err:?}").contains("annotation sessions cannot dispatch"));
            assert!(
                matches!(
                    event_rx.try_recv(),
                    Err(tokio::sync::mpsc::error::TryRecvError::Empty)
                ),
                "no tool lifecycle event should be emitted before policy rejection"
            );
        })
        .await;
}

#[test]
fn actor_policy_only_comes_from_annotation_session_kind() {
    assert_eq!(
        SessionActorPolicy::from_session_kind(Some("annotation")),
        SessionActorPolicy::Annotation
    );
    for kind in [None, Some("fork"), Some("subagent"), Some("worktree")] {
        assert_eq!(
            SessionActorPolicy::from_session_kind(kind),
            SessionActorPolicy::Standard
        );
    }
}

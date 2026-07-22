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

#[tokio::test(flavor = "current_thread")]
async fn annotation_actor_suppresses_start_and_both_end_hook_paths() {
    use crate::extensions::hooks::ClientHookGroup;
    use xai_grok_hooks::event::{HookEventName, HookPayload};

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, mut gateway_rx) = tokio::sync::mpsc::unbounded_channel();
            let (persistence_tx, _persistence_rx) = tokio::sync::mpsc::unbounded_channel();
            let (mut actor, _event_rx) =
                create_test_actor_ex(0, 128_000, 90, gateway_tx, persistence_tx).await;
            actor.startup_hints.actor_policy = SessionActorPolicy::Annotation;

            let group = ClientHookGroup {
                matcher: None,
                callback_ids: vec!["must-not-fire".to_string()],
                timeout: None,
            };
            for event in [
                HookEventName::SessionStart,
                HookEventName::SessionEnd,
                HookEventName::Stop,
            ] {
                actor
                    .client_hooks
                    .borrow_mut()
                    .insert(event, vec![group.clone()]);
            }
            *actor.hook_registry.borrow_mut() = Some(std::sync::Arc::new(
                xai_grok_hooks::discovery::HookRegistry::default(),
            ));

            for event in [
                HookEventName::SessionStart,
                HookEventName::SessionEnd,
                HookEventName::Stop,
            ] {
                assert!(
                    !actor.hook_event_active(event),
                    "annotation policy must dominate registered client and file hooks for {event}"
                );
            }

            actor
                .dispatch_hook(
                    HookEventName::SessionStart,
                    HookPayload::SessionStart {
                        source: "annotation-load".to_string(),
                        model_id: None,
                        agent_type: None,
                    },
                    None,
                    None,
                )
                .await;
            for reason in ["channel_closed", "shutdown"] {
                actor
                    .dispatch_hook(
                        HookEventName::SessionEnd,
                        HookPayload::SessionEnd {
                            reason: reason.to_string(),
                            turn_count: None,
                            tool_call_count: None,
                        },
                        None,
                        None,
                    )
                    .await;
                actor.dispatch_session_end_stop(reason).await;
            }

            assert!(
                matches!(
                    gateway_rx.try_recv(),
                    Err(tokio::sync::mpsc::error::TryRecvError::Empty)
                ),
                "no lifecycle client callback may be notified for an annotation actor"
            );
        })
        .await;
}

fn stop_hook_registry(script: String) -> xai_grok_hooks::discovery::HookRegistry {
    let (mut registry, _) = xai_grok_hooks::discovery::load_hooks(None, None);
    registry.append_specs(vec![xai_grok_hooks::config::HookSpec {
        name: "test/annotation-policy-stop".into(),
        event: xai_grok_hooks::event::HookEventName::Stop,
        handler_type: xai_grok_hooks::config::HandlerType::Command,
        configured_matcher: None,
        matcher: None,
        enabled: true,
        command: Some(std::path::PathBuf::from(&script)),
        command_raw: Some(script),
        url: None,
        url_raw: None,
        timeout_ms: 5_000,
        source_dir: std::path::PathBuf::from("/tmp"),
        extra_env: std::collections::HashMap::new(),
    }]);
    registry
}

#[tokio::test(flavor = "current_thread")]
async fn annotation_actor_suppresses_turn_stop_gate_and_workspace_hooks() {
    use crate::extensions::hooks::ClientHookGroup;
    use xai_grok_hooks::event::HookEventName;
    use xai_tool_protocol::turn_hook::{AfterTurnPayload, BeforeTurnPayload, TurnHookOutcome};

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let temp = tempfile::tempdir().expect("temp hook directory");
            let annotation_marker = temp.path().join("annotation-stop-ran");
            let standard_marker = temp.path().join("standard-stop-ran");

            let (annotation_gateway, mut annotation_gateway_rx) =
                tokio::sync::mpsc::unbounded_channel();
            let (annotation_persistence, _annotation_persistence_rx) =
                tokio::sync::mpsc::unbounded_channel();
            let (mut annotation, _annotation_events) =
                create_test_actor_ex(0, 128_000, 90, annotation_gateway, annotation_persistence)
                    .await;
            annotation.startup_hints.actor_policy = SessionActorPolicy::Annotation;
            annotation.hook_resolved_workspace_root = temp.path().display().to_string();
            *annotation.hook_registry.borrow_mut() = Some(std::sync::Arc::new(stop_hook_registry(
                format!("touch {}", annotation_marker.display()),
            )));
            annotation.client_hooks.borrow_mut().insert(
                HookEventName::Stop,
                vec![ClientHookGroup {
                    matcher: None,
                    callback_ids: vec!["must-not-run".into()],
                    timeout: None,
                }],
            );

            SessionActor::reset_workspace_turn_hook_test_calls();
            assert!(matches!(
                annotation.run_stop_gate("annotation-prompt", 0).await,
                StopGateDecision::AllowStop
            ));
            annotation
                .send_before_turn_event(BeforeTurnPayload {
                    turn_number: 1,
                    model_id: "test-model".into(),
                    ..Default::default()
                })
                .await;
            annotation
                .send_after_turn_event(AfterTurnPayload {
                    turn_number: 1,
                    outcome: TurnHookOutcome::Completed,
                    duration_ms: 1,
                    tool_call_count: 0,
                    model_id: "test-model".into(),
                    written_repo_paths: Vec::new(),
                    cancellation_category: None,
                    cancellation_context: None,
                })
                .await;

            assert!(!annotation_marker.exists());
            assert_eq!(SessionActor::workspace_turn_hook_test_calls(), (0, 0));
            assert!(matches!(
                annotation_gateway_rx.try_recv(),
                Err(tokio::sync::mpsc::error::TryRecvError::Empty)
            ));

            // Positive control: the same direct rails remain live for a
            // Standard actor, including the on-disk Stop command.
            let (standard_gateway, _standard_gateway_rx) = tokio::sync::mpsc::unbounded_channel();
            let (standard_persistence, _standard_persistence_rx) =
                tokio::sync::mpsc::unbounded_channel();
            let (mut standard, _standard_events) =
                create_test_actor_ex(0, 128_000, 90, standard_gateway, standard_persistence).await;
            standard.hook_resolved_workspace_root = temp.path().display().to_string();
            *standard.hook_registry.borrow_mut() = Some(std::sync::Arc::new(stop_hook_registry(
                format!("touch {}", standard_marker.display()),
            )));

            SessionActor::reset_workspace_turn_hook_test_calls();
            assert!(matches!(
                standard.run_stop_gate("standard-prompt", 0).await,
                StopGateDecision::AllowStop
            ));
            standard
                .send_before_turn_event(BeforeTurnPayload {
                    turn_number: 1,
                    model_id: "test-model".into(),
                    ..Default::default()
                })
                .await;
            standard
                .send_after_turn_event(AfterTurnPayload {
                    turn_number: 1,
                    outcome: TurnHookOutcome::Completed,
                    duration_ms: 1,
                    tool_call_count: 0,
                    model_id: "test-model".into(),
                    written_repo_paths: Vec::new(),
                    cancellation_category: None,
                    cancellation_context: None,
                })
                .await;

            assert!(standard_marker.exists());
            assert_eq!(SessionActor::workspace_turn_hook_test_calls(), (1, 1));
        })
        .await;
}

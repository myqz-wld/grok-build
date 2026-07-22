#![cfg_attr(rustfmt, rustfmt::skip)]
    use super::*;

    #[test]
    fn acp_chunk_for_inactive_agent_lands_in_its_scrollback() {
        // Regression: switching away from a streaming agent must not
        // discard chunks bound for that agent. Before this fix, only
        // `TaskResult::PromptResponse` survived, so the user saw a bare
        // "Worked for X.Xs" with no body text.
        let mut app = make_app_with_agent("sess-A");
        insert_agent(&mut app, AgentId(1), Some("sess-B"));
        switch_active_to(&mut app, AgentId(1));

        let affected = handle(make_agent_chunk_message("sess-A", "hello from A"), &mut app);

        let agent_a = app.agents.get(&AgentId(0)).unwrap();
        assert_eq!(
            agent_message_text(agent_a),
            "hello from A",
            "chunk for inactive agent A must land in A's scrollback"
        );
        assert!(
            !affected,
            "chunk routed to a non-active agent must not request a redraw"
        );
        let agent_b = app.agents.get(&AgentId(1)).unwrap();
        assert!(
            agent_b.scrollback.is_empty(),
            "active agent B's scrollback must remain untouched"
        );
    }

    #[test]
    fn acp_chunk_for_active_agent_returns_affected_true() {
        // Baseline: chunk for the visible agent triggers a redraw.
        let mut app = make_app_with_agent("sess-A");
        insert_agent(&mut app, AgentId(1), Some("sess-B"));
        switch_active_to(&mut app, AgentId(1));

        let affected = handle(make_agent_chunk_message("sess-B", "hello from B"), &mut app);

        assert!(affected, "chunk for active agent must request a redraw");
        let agent_b = app.agents.get(&AgentId(1)).unwrap();
        assert_eq!(agent_message_text(agent_b), "hello from B");
    }

    #[test]
    fn acp_chunk_for_subagent_routes_through_parent() {
        // Subagent (child) chunk must land in the parent's
        // `subagent_views[child_sid]` even when a different agent is
        // currently active.
        let mut app = make_app_with_agent("sess-A");
        insert_agent(&mut app, AgentId(1), Some("sess-B"));
        switch_active_to(&mut app, AgentId(1));

        let child_sid = "sess-A-child";
        {
            let parent = app.agents.get_mut(&AgentId(0)).unwrap();
            parent
                .subagent_sessions
                .insert(child_sid.into(), make_subagent_info(child_sid));
            parent
                .subagent_views
                .insert(child_sid.into(), Box::new(make_agent(Some(child_sid))));
        }

        let affected = handle(
            make_agent_chunk_message(child_sid, "hello from subagent"),
            &mut app,
        );

        let parent = app.agents.get(&AgentId(0)).unwrap();
        let child_view = parent
            .subagent_views
            .get(child_sid)
            .expect("child view must still exist");
        assert_eq!(
            agent_message_text(child_view),
            "hello from subagent",
            "subagent chunk must land in subagent_views[child_sid]"
        );
        assert!(
            !affected,
            "subagent chunk for non-active parent must not request a redraw"
        );
    }

    #[test]
    fn acp_chunk_for_annotation_routes_to_thread_without_touching_root_or_child_views() {
        let mut app = make_app_with_agent("parent");
        let thread_id = uuid::Uuid::from_u128(100);
        let exchange_id = uuid::Uuid::from_u128(101);
        let selected = "selected";
        let anchor = crate::annotations::AnnotationAnchor {
            parent_session_id: "parent".into(),
            transcript_key: crate::annotations::TranscriptKey {
                prompt_index: 0,
                role: crate::annotations::AnnotationEntryRole::Assistant,
                ordinal: 0,
            },
            entry_role: crate::annotations::AnnotationEntryRole::Assistant,
            target_prompt_index: 0,
            start_source_line: 1,
            end_source_line: 1,
            selected_text: selected.into(),
            selected_text_hash: blake3::hash(selected.as_bytes()).to_hex().to_string(),
            surrounding_text_hash: blake3::hash(selected.as_bytes()).to_hex().to_string(),
        };
        {
            let parent = app.agents.get_mut(&AgentId(0)).unwrap();
            let created = crate::annotations::AnnotationEvent::new(
                thread_id,
                crate::annotations::AnnotationEventKind::ThreadCreated {
                    anchor,
                    child_session_id: "annotation-child".into(),
                    first_question: "why?".into(),
                },
            );
            assert!(parent.annotation_runtime.state.apply(created).is_none());
            let started = crate::annotations::AnnotationEvent::new(
                thread_id,
                crate::annotations::AnnotationEventKind::ExchangeStarted {
                    exchange_id,
                    question: "why?".into(),
                },
            );
            assert!(parent.annotation_runtime.state.apply(started).is_none());
            parent
                .annotation_runtime
                .sessions
                .insert("annotation-child".into(), thread_id);
            parent.annotation_runtime.in_flight.insert(
                thread_id,
                crate::annotations::AnnotationInFlight::new(
                    exchange_id,
                    "why?".into(),
                    crate::annotations::AnnotationExchangePhase::Prompting,
                ),
            );
        }

        assert!(matches!(
            find_session_match(&app, &acp::SessionId::new("parent")),
            Some(SessionMatch::Root(AgentId(0)))
        ));
        assert!(matches!(
            find_session_match(&app, &acp::SessionId::new("annotation-child")),
            Some(SessionMatch::Annotation { agent_id: AgentId(0), thread_id: id }) if id == thread_id
        ));

        let affected = handle(
            make_agent_chunk_message("annotation-child", "thread answer"),
            &mut app,
        );
        let parent = app.agents.get(&AgentId(0)).unwrap();
        assert!(affected, "visible parent must redraw its inline card");
        assert!(parent.scrollback.is_empty(), "root transcript stays untouched");
        assert!(parent.subagent_views.is_empty(), "annotation is not a subagent view");
        assert_eq!(
            parent.annotation_runtime.state.threads[&thread_id].exchanges[0].answer_markdown,
            "thread answer"
        );
    }

    #[test]
    fn open_annotation_child_routes_by_running_prompt_without_stealing_root_prompts() {
        let mut app = make_app_with_agent("parent");
        let thread_id = uuid::Uuid::from_u128(110);
        let exchange_id = uuid::Uuid::from_u128(111);
        let selected = "selected";
        let anchor = crate::annotations::AnnotationAnchor {
            parent_session_id: "parent".into(),
            transcript_key: crate::annotations::TranscriptKey {
                prompt_index: 0,
                role: crate::annotations::AnnotationEntryRole::Assistant,
                ordinal: 0,
            },
            entry_role: crate::annotations::AnnotationEntryRole::Assistant,
            target_prompt_index: 0,
            start_source_line: 1,
            end_source_line: 1,
            selected_text: selected.into(),
            selected_text_hash: blake3::hash(selected.as_bytes()).to_hex().to_string(),
            surrounding_text_hash: blake3::hash(selected.as_bytes()).to_hex().to_string(),
        };
        let annotation_prompt_id;
        {
            let parent = app.agents.get_mut(&AgentId(0)).unwrap();
            assert!(parent.annotation_runtime.state.apply(
                crate::annotations::AnnotationEvent::new(
                    thread_id,
                    crate::annotations::AnnotationEventKind::ThreadCreated {
                        anchor,
                        child_session_id: "annotation-child".into(),
                        first_question: "why?".into(),
                    },
                ),
            ).is_none());
            assert!(parent.annotation_runtime.state.apply(
                crate::annotations::AnnotationEvent::new(
                    thread_id,
                    crate::annotations::AnnotationEventKind::ExchangeStarted {
                        exchange_id,
                        question: "why?".into(),
                    },
                ),
            ).is_none());
            parent.annotation_runtime.sessions.insert("annotation-child".into(), thread_id);
            let in_flight = crate::annotations::AnnotationInFlight::new(
                exchange_id,
                "why?".into(),
                crate::annotations::AnnotationExchangePhase::Prompting,
            );
            annotation_prompt_id = in_flight.prompt_id.clone();
            parent.annotation_runtime.in_flight.insert(thread_id, in_flight);
        }

        // Simulate the card's Open child action creating an ordinary root view
        // for the very same session while the parent card remains usable.
        insert_agent(&mut app, AgentId(1), Some("annotation-child"));

        assert!(matches!(
            find_session_match(&app, &acp::SessionId::new("annotation-child")),
            Some(SessionMatch::Root(AgentId(1)))
        ));
        assert!(matches!(
            find_session_match_for_prompt(
                &app,
                &acp::SessionId::new("annotation-child"),
                Some(&annotation_prompt_id),
            ),
            Some(SessionMatch::Annotation { agent_id: AgentId(0), thread_id: id }) if id == thread_id
        ));

        let affected = handle(
            make_agent_chunk_message_with_prompt(
                "annotation-child",
                "annotation follow-up",
                &annotation_prompt_id,
                false,
            ),
            &mut app,
        );
        assert!(affected, "returning to the visible parent redraws the card");
        assert_eq!(
            app.agents[&AgentId(0)].annotation_runtime.state.threads[&thread_id].exchanges[0]
                .answer_markdown,
            "annotation follow-up"
        );
        assert!(app.agents[&AgentId(1)].scrollback.is_empty());

        handle(
            make_agent_chunk_message_with_prompt(
                "annotation-child",
                "ordinary root answer",
                "root-prompt",
                false,
            ),
            &mut app,
        );
        assert_eq!(agent_message_text(&app.agents[&AgentId(1)]), "ordinary root answer");
        assert_eq!(
            app.agents[&AgentId(0)].annotation_runtime.state.threads[&thread_id].exchanges[0]
                .answer_markdown,
            "annotation follow-up",
            "the open root's own prompt must not leak into the card"
        );

        {
            let root = app.agents.get_mut(&AgentId(1)).unwrap();
            root.attached_as_viewer = true;
            root.session.state = AgentState::TurnRunning;
            root.session.current_prompt_id = Some("root-prompt".into());
        }
        assert!(!handle_prompt_complete(
            &prompt_complete_ext_with_prompt_id(
                "annotation-child",
                &annotation_prompt_id,
                "end_turn",
            ),
            &mut app,
        ));
        let root = &app.agents[&AgentId(1)];
        assert!(matches!(root.session.state, AgentState::TurnRunning));
        assert_eq!(root.session.current_prompt_id.as_deref(), Some("root-prompt"));

        for is_replay in [false, true] {
            assert!(!handle_ext_notification(
                &xai_turn_completed_notif(
                    "annotation-child",
                    &annotation_prompt_id,
                    "end_turn",
                    is_replay,
                ),
                &mut app,
            ));
            let root = &app.agents[&AgentId(1)];
            assert!(matches!(root.session.state, AgentState::TurnRunning));
            assert_eq!(
                root.session.current_prompt_id.as_deref(),
                Some("root-prompt"),
                "live and replayed durable annotation terminals must not finish the root"
            );
        }

        assert!(!handle(
            make_ext_session_notification_with_prompt(
                "annotation-child",
                XaiSessionUpdate::RetryState(RetryState::Retrying {
                    attempt: 1,
                    max_retries: 3,
                    reason: "annotation retry".into(),
                }),
                &annotation_prompt_id,
                false,
            ),
            &mut app,
        ));
        assert!(
            !matches!(
                app.agents[&AgentId(1)].session.turn_activity(),
                Some(crate::acp::tracker::TurnActivity::Retrying { reason, .. })
                    if reason == "annotation retry"
            ),
            "annotation retry lifecycle must not decorate the coexisting root"
        );

        assert!(!handle(
            make_ext_session_notification_with_prompt(
                "annotation-child",
                XaiSessionUpdate::RetryState(RetryState::Retrying {
                    attempt: 1,
                    max_retries: 3,
                    reason: "root retry".into(),
                }),
                "root-prompt",
                false,
            ),
            &mut app,
        ));
        assert!(matches!(
            app.agents[&AgentId(1)].session.turn_activity(),
            Some(crate::acp::tracker::TurnActivity::Retrying { reason, .. })
                if reason == "root retry"
        ));

        // Session-global notifications deliberately carry no prompt id. Even
        // with an annotation prompt in flight, root-first routing must let the
        // ordinary child view consume its manual-recap terminal.
        {
            let root = app.agents.get_mut(&AgentId(1)).unwrap();
            let spinner = root
                .scrollback
                .push(crate::scrollback::entry::ScrollbackEntry::running(
                    recap_block(""),
                ));
            root.pending_recap_entry = Some(spinner);
        }
        assert!(!handle(
            make_ext_session_notification(
                "annotation-child",
                XaiSessionUpdate::SessionRecapUnavailable,
            ),
            &mut app,
        ));
        assert!(
            app.agents[&AgentId(1)].pending_recap_entry.is_none(),
            "the coexisting root must consume the unscoped recap terminal"
        );
        assert_eq!(
            app.agents[&AgentId(0)].annotation_runtime.in_flight[&thread_id].prompt_id,
            annotation_prompt_id,
            "session-global routing must leave annotation prompt ownership intact"
        );

        // A storage-failure drain retains prompt ownership even though the
        // card is already locally failed and no longer accepts answer chunks.
        app.agents
            .get_mut(&AgentId(0))
            .unwrap()
            .annotation_runtime
            .in_flight
            .get_mut(&thread_id)
            .unwrap()
            .phase = crate::annotations::AnnotationExchangePhase::DrainingAfterStorageFailure;
        assert!(!handle(
            make_agent_chunk_message_with_prompt(
                "annotation-child",
                "must be discarded",
                &annotation_prompt_id,
                false,
            ),
            &mut app,
        ));
        assert_eq!(
            agent_message_text(&app.agents[&AgentId(1)]),
            "ordinary root answer",
            "late drain chunks stay owned by the annotation and never reach the root"
        );
        assert_eq!(
            app.agents[&AgentId(0)].annotation_runtime.state.threads[&thread_id].exchanges[0]
                .answer_markdown,
            "annotation follow-up"
        );

        assert!(!handle_ext_notification(
            &xai_turn_completed_notif(
                "annotation-child",
                "root-prompt",
                "end_turn",
                false,
            ),
            &mut app,
        ));
        let root = &app.agents[&AgentId(1)];
        assert!(matches!(root.session.state, AgentState::Idle));
        assert!(root.session.current_prompt_id.is_none());
    }

    #[test]
    fn acp_chunk_with_unknown_session_id_is_dropped_and_no_redraw() {
        // No agent owns the session_id and the active agent already has a
        // session_id assigned (so the race-window fallback does not fire).
        // The notification must be dropped silently.
        let mut app = make_app_with_agent("sess-A");
        insert_agent(&mut app, AgentId(1), Some("sess-B"));
        // make_app_with_agent already activated AgentId(0); no switch needed.

        let affected = handle(
            make_agent_chunk_message("sess-unknown", "stray text"),
            &mut app,
        );

        assert!(!affected, "unknown session_id must not request a redraw");
        assert!(
            app.agents.get(&AgentId(0)).unwrap().scrollback.is_empty(),
            "agent A must not have absorbed a notification for sess-unknown"
        );
        assert!(
            app.agents.get(&AgentId(1)).unwrap().scrollback.is_empty(),
            "agent B must not have absorbed a notification for sess-unknown"
        );
    }

    #[test]
    fn session_id_none_race_window_routes_to_active_agent() {
        // Pin the existing race-window semantics: notifications that arrive
        // before `TaskResult::SessionCreated` (active agent has no session_id
        // yet) must still land on the active agent.

        // Case 1: active agent A has session_id == None; everyone else has
        // a real id. Stray notification routes to A.
        {
            let mut app = make_app_with_agent("sess-A");
            app.agents.get_mut(&AgentId(0)).unwrap().session.session_id = None;
            insert_agent(&mut app, AgentId(1), Some("sess-B"));
            // make_app_with_agent already activated AgentId(0); no switch needed.

            let _ = handle(
                make_agent_chunk_message("not-yet-assigned", "racing chunk"),
                &mut app,
            );

            assert_eq!(
                agent_message_text(app.agents.get(&AgentId(0)).unwrap()),
                "racing chunk",
                "race-window fallback should land on active agent A"
            );
            assert!(
                app.agents.get(&AgentId(1)).unwrap().scrollback.is_empty(),
                "non-active agent B must not absorb the race chunk"
            );
        }

        // Case 2: both A and B have session_id == None; the active one wins.
        {
            let mut app = make_app_with_agent("sess-A");
            app.agents.get_mut(&AgentId(0)).unwrap().session.session_id = None;
            insert_agent(&mut app, AgentId(1), None);
            switch_active_to(&mut app, AgentId(1));

            let _ = handle(
                make_agent_chunk_message("not-yet-assigned", "racing chunk"),
                &mut app,
            );

            assert!(
                app.agents.get(&AgentId(0)).unwrap().scrollback.is_empty(),
                "non-active agent A must not absorb the race chunk"
            );
            assert_eq!(
                agent_message_text(app.agents.get(&AgentId(1)).unwrap()),
                "racing chunk",
                "race-window fallback must prefer the active agent (B)"
            );
        }
    }

    #[test]
    fn plan_update_for_inactive_agent_lands_in_its_todo() {
        let mut app = make_app_with_agent("sess-A");
        insert_agent(&mut app, AgentId(1), Some("sess-B"));
        switch_active_to(&mut app, AgentId(1));
        // Sanity: A's todo starts empty.
        assert_eq!(
            app.agents.get(&AgentId(0)).unwrap().todo.counts().total(),
            0,
        );

        let _ = handle(make_plan_message("sess-A", &["task1", "task2"]), &mut app);

        let agent_a = app.agents.get(&AgentId(0)).unwrap();
        assert_eq!(
            agent_a.todo.counts().total(),
            2,
            "Plan update must mutate A's todo even when B is active"
        );
        let agent_b = app.agents.get(&AgentId(1)).unwrap();
        assert_eq!(
            agent_b.todo.counts().total(),
            0,
            "active agent B's todo must not absorb A's plan"
        );
    }

    #[test]
    fn commands_update_for_inactive_agent_bumps_its_generation() {
        let mut app = make_app_with_agent("sess-A");
        insert_agent(&mut app, AgentId(1), Some("sess-B"));
        switch_active_to(&mut app, AgentId(1));
        let initial_gen_a = app
            .agents
            .get(&AgentId(0))
            .unwrap()
            .session
            .available_commands_generation;

        let _ = handle(
            make_commands_update_message("sess-A", &["compact", "fork"]),
            &mut app,
        );

        let agent_a = app.agents.get(&AgentId(0)).unwrap();
        assert_eq!(
            agent_a.session.available_commands.len(),
            2,
            "AvailableCommandsUpdate must replace A's commands list"
        );
        assert_eq!(
            agent_a.session.available_commands_generation,
            initial_gen_a + 1,
            "AvailableCommandsUpdate must bump A's generation counter"
        );
    }

    #[test]
    fn bg_task_stdout_for_inactive_agent_lands_in_its_bg_tasks() {
        let mut app = make_app_with_agent("sess-A");
        insert_agent(&mut app, AgentId(1), Some("sess-B"));
        switch_active_to(&mut app, AgentId(1));

        // Pre-register a bg task on A so route_bg_task_stdout has a target.
        let task_id = "task-A-1";
        let tool_call_id = "call-A-1";
        {
            let agent_a = app.agents.get_mut(&AgentId(0)).unwrap();
            agent_a.session.bg_tasks.insert(
                task_id.into(),
                BgTaskState {
                    task_id: task_id.into(),
                    tool_call_id: tool_call_id.into(),
                    command: "sleep 5".into(),
                    description: None,
                    cwd: "/tmp".into(),
                    output_file: "/tmp/out".into(),
                    status: BgTaskStatus::Running,
                    start_time: std::time::SystemTime::now(),
                    end_time: None,
                    exit_code: None,
                    signal: None,
                    stdout: String::new(),
                    stdout_line_count: 0,
                    truncated: false,
                    pending_kill: false,
                    kill_requested_at: None,
                    scrollback_entry_id: None,
                    is_monitor: false,
                    restored_from_replay: false,
                },
            );
            agent_a
                .session
                .bg_tool_call_to_task
                .insert(tool_call_id.into(), task_id.into());
        }

        let _ = handle(
            make_bash_stdout_message("sess-A", tool_call_id, "stdout-from-A"),
            &mut app,
        );

        let agent_a = app.agents.get(&AgentId(0)).unwrap();
        assert_eq!(
            agent_a.session.bg_tasks.get(task_id).unwrap().stdout,
            "stdout-from-A",
            "Bash stdout must land in A's bg_tasks even when B is active"
        );
    }

    #[test]
    fn acp_chunks_for_two_agents_dont_cross_contaminate() {
        // Send chunks to both A and B in sequence; each landing in its own
        // scrollback proves the demux works in both directions regardless
        // of which agent is currently active.
        let mut app = make_app_with_agent("sess-A");
        insert_agent(&mut app, AgentId(1), Some("sess-B"));
        switch_active_to(&mut app, AgentId(1));

        let _ = handle(make_agent_chunk_message("sess-A", "A only"), &mut app);
        let _ = handle(make_agent_chunk_message("sess-B", "B only"), &mut app);

        assert_eq!(
            agent_message_text(app.agents.get(&AgentId(0)).unwrap()),
            "A only",
        );
        assert_eq!(
            agent_message_text(app.agents.get(&AgentId(1)).unwrap()),
            "B only",
        );
    }

use super::*;

/// Result of looking up which view a notification's `session_id` targets.
///
/// The matched view's mutation must happen on the agent identified here, regardless of which view the user is currently looking at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SessionMatch {
    /// The session_id matches the root session of this agent.
    Root(AgentId),
    /// The session_id matches a subagent view child of this agent (i.e. an entry in `agent.subagent_views`).
    /// The child's key is the notification's `session_id.0.as_ref()`; the caller re-derives it to avoid an extra allocation.
    Child(AgentId),
    /// The session belongs to one parent-owned inline annotation thread. It
    /// has no child `AgentView` and must never enter ordinary session routing.
    Annotation {
        agent_id: AgentId,
        thread_id: crate::annotations::ThreadId,
    },
}

impl SessionMatch {
    /// The owning agent's id: the matched agent for `Root`, the parent that owns the `subagent_views` entry for `Child`.
    /// Callers that do not care about root vs child should use this instead of duplicating the `match { Root(id) | Child(id) => id }` pattern.
    pub(super) fn agent_id(self) -> AgentId {
        match self {
            SessionMatch::Root(id) | SessionMatch::Child(id) => id,
            SessionMatch::Annotation { agent_id, .. } => agent_id,
        }
    }
}

/// Resolve the agent that owns a notification's `session_id` and whether the active view is affected.
///
/// Convenience wrapper around `find_session_match`, `is_matched_agent_active`, and `agents.get_mut()`, used by the bg-task notification handlers.
pub(super) fn resolve_notif_agent<'a>(
    app: &'a mut AppView,
    session_id: &acp::SessionId,
) -> Option<(SessionMatch, bool, &'a mut AgentView)> {
    let matched = find_session_match(app, session_id)?;
    if matches!(matched, SessionMatch::Annotation { .. }) {
        return None;
    }
    let parent_id = matched.agent_id();
    let is_active = is_matched_agent_active(app, parent_id);
    let agent = app.agents.get_mut(&parent_id)?;
    Some((matched, is_active, agent))
}

/// A background session's progress updates and completion signal land on *its* agent instead of whichever agent is foregrounded.
/// Otherwise a background agent's "Connecting MCPs (N/M)…" spinner is never cleared and sticks forever.
/// Only resolves to a `Root` agent: `mcp_init_progress` is a per-root-agent indicator with no per-subagent slot.
pub(super) fn mcp_target_agent<'a>(
    app: &'a mut AppView,
    session_id: Option<&str>,
) -> Option<(bool, &'a mut AgentView)> {
    match session_id {
        Some(sid) => {
            let sid = acp::SessionId::new(sid);
            let (matched, is_active, agent) = resolve_notif_agent(app, &sid)?;
            if matches!(
                matched,
                SessionMatch::Child(_) | SessionMatch::Annotation { .. }
            ) {
                return None;
            }
            Some((is_active, agent))
        }
        None => {
            let id = match app.active_view {
                ActiveView::Agent(id) => id,
                ActiveView::Welcome => app.home_session_agent?,
                ActiveView::AgentDashboard => return None,
            };
            let agent = app.agents.get_mut(&id)?;
            Some((matches!(app.active_view, ActiveView::Agent(_)), agent))
        }
    }
}

/// Given a matched session and the owning agent, borrow the correct `(session, scrollback)` pair.
/// That is the child view's pair when the notification targets a subagent, the root agent's otherwise.
pub(super) fn resolve_target_view<'a>(
    agent: &'a mut AgentView,
    matched: SessionMatch,
    child_sid: &str,
) -> Option<(
    &'a mut AgentSession,
    &'a mut crate::scrollback::state::ScrollbackState,
)> {
    if matches!(matched, SessionMatch::Child(_)) {
        // A `TaskBackgrounded` / `TaskCompleted` block for a resumed child always follows the funneled tool_call that spawned the task
        // The child is therefore never still both empty and NeedsReplay here, so it is precedence-exempt from the hydrate funnel
        let child_view = agent.subagent_views.get_mut(child_sid)?;
        Some((&mut child_view.session, &mut child_view.scrollback))
    } else if matches!(matched, SessionMatch::Annotation { .. }) {
        None
    } else {
        Some((&mut agent.session, &mut agent.scrollback))
    }
}

/// The only agent that could own such a pre-assignment notification is the one the user just created (necessarily active, `session_id == None`).
/// Returns `None` when the notification cannot be associated with any agent.
/// All ACP-notification handlers must route through this function rather than gating on `app.active_view` directly.
pub(super) fn find_session_match(
    app: &AppView,
    session_id: &acp::SessionId,
) -> Option<SessionMatch> {
    find_session_match_for_prompt(app, session_id, None)
}

/// Locate the owner of a prompt-attributed notification.
///
/// An annotation child may also be open as an ordinary root view. In that
/// coexistence state the session id alone is ambiguous: notifications for the
/// annotation prompt still belong to the parent-owned card, while prompts
/// started from the open root belong to that root view. A prompt id matching
/// the annotation thread's current in-flight prompt therefore takes precedence
/// over the otherwise-normal exact-root match. Notifications without a prompt
/// id retain the legacy root-first ownership order.
pub(super) fn find_session_match_for_prompt(
    app: &AppView,
    session_id: &acp::SessionId,
    prompt_id: Option<&str>,
) -> Option<SessionMatch> {
    // Single pass over `app.agents`: collect the exact root and fallback child
    // owners while checking whether prompt attribution disambiguates an
    // annotation owner before the usual root-first ordering is applied.
    //
    // Comparing `Option<&SessionId>` to `Some(&session_id)` borrows both
    // sides -- no SessionId clone. The HashMap lookup uses the inner `&str`
    // directly via the `Borrow<str>` impl on `String`, so no allocation
    // either. Without an annotation prompt match this preserves the previous
    // two-pass semantics (root wins when both could match) while halving the
    // iteration cost on the hot notification path.
    let child_key: &str = session_id.0.as_ref();
    let mut root_match: Option<AgentId> = None;
    let mut child_match: Option<AgentId> = None;
    let mut annotation_match: Option<(AgentId, crate::annotations::ThreadId)> = None;
    let mut annotation_prompt_match: Option<(AgentId, crate::annotations::ThreadId)> = None;
    for (id, agent) in &app.agents {
        if root_match.is_none() && agent.session.session_id.as_ref() == Some(session_id) {
            root_match = Some(*id);
        }
        if child_match.is_none() && agent.subagent_views.contains_key(child_key) {
            child_match = Some(*id);
        }
        if let Some(thread_id) = agent.annotation_runtime.sessions.get(child_key) {
            if annotation_match.is_none() {
                annotation_match = Some((*id, *thread_id));
            }
            if annotation_prompt_match.is_none()
                && prompt_id.is_some_and(|prompt_id| {
                    agent
                        .annotation_runtime
                        .in_flight
                        .get(thread_id)
                        .is_some_and(|in_flight| in_flight.prompt_id == prompt_id)
                })
            {
                annotation_prompt_match = Some((*id, *thread_id));
            }
        }
    }
    if let Some((agent_id, thread_id)) = annotation_prompt_match {
        return Some(SessionMatch::Annotation {
            agent_id,
            thread_id,
        });
    }
    if let Some(id) = root_match {
        return Some(SessionMatch::Root(id));
    }
    if let Some(id) = child_match {
        return Some(SessionMatch::Child(id));
    }
    if let Some((agent_id, thread_id)) = annotation_match {
        return Some(SessionMatch::Annotation {
            agent_id,
            thread_id,
        });
    }
    // Pass 3: race-window fallback for notifications that arrive before the
    // root session_id has been assigned. Only the active agent is eligible,
    // and only when its `session_id` is still `None` -- otherwise we would
    // misroute a stranger's notification to whichever agent happens to be
    // foregrounded.
    if let ActiveView::Agent(active_id) = app.active_view
        && let Some(agent) = app.agents.get(&active_id)
        && agent.session.session_id.is_none()
    {
        return Some(SessionMatch::Root(active_id));
    }
    if matches!(app.active_view, ActiveView::Welcome)
        && let Some(id) = app.home_session_agent
        && let Some(agent) = app.agents.get(&id)
        && agent.session.session_id.is_none()
    {
        return Some(SessionMatch::Root(id));
    }
    None
}

/// Whether the matched agent is the one currently displayed.
pub(super) fn is_matched_agent_active(app: &AppView, matched_agent: AgentId) -> bool {
    matches!(app.active_view, ActiveView::Agent(id) if id == matched_agent)
}

/// Routes by the request's session id via [`find_session_match`] (exactly like `session/update` notifications), not gated on `app.active_view`.
/// A modal raised by a **background** session thus lands on its own view even when the user is on the dashboard or a different session.
/// The caller must then leave the reverse-request unanswered (drop, do NOT error) and rely on the leader's replay-on-attach.
pub(super) fn interaction_target_agent(app: &AppView, session_id: &str) -> Option<AgentId> {
    let sid = acp::SessionId::new(session_id.to_owned());
    match find_session_match(app, &sid) {
        Some(SessionMatch::Root(id) | SessionMatch::Child(id)) => Some(id),
        Some(SessionMatch::Annotation { .. }) => None,
        None => None,
    }
}

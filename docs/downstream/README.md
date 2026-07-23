# Downstream Feature Ledger

This directory records the intentionally maintained delta from
[`xai-org/grok-build`](https://github.com/xai-org/grok-build). The local branch
policy is:

- `main` is a pristine mirror of `upstream/main`.
- `downstream/main` is the integration branch for accepted downstream work.
- each feature is developed on an isolated branch/worktree based on
  `downstream/main`, then merged there after validation.

## Maintained Features

| Feature | Status | Upstream base | User documentation | Design / execution plan |
|---|---|---|---|---|
| Persistent threaded inline annotations | Implemented on `feature/inline-annotations` | `ba76b0a` | [Keyboard shortcuts: Inline Annotations](../../crates/codegen/xai-grok-pager/docs/user-guide/03-keyboard-shortcuts.md#inline-annotations-standard-tui) | [Inline annotations plan](plans/inline-annotations.md) |

### Inline annotations

The standard TUI can attach a persistent, threaded hidden fork with local
read-only file tools to a stable logical-line selection in one completed User
or Assistant message. Annotations remain parent-side UI metadata and never
become parent transcript blocks. `--minimal` is intentionally unchanged.

Primary isolated modules:

- `crates/codegen/xai-grok-pager/src/annotations/`
- `crates/codegen/xai-grok-pager/src/app/agent_view/annotation_ui.rs`
- `crates/codegen/xai-grok-pager/src/app/agent_view/annotations.rs`
- `crates/codegen/xai-grok-pager/src/app/dispatch/annotations.rs`
- `crates/codegen/xai-grok-pager/src/scrollback/decorations.rs`
- `crates/codegen/xai-grok-pager/src/views/annotation.rs`

Persisted data is append-only `annotation_threads.jsonl` beneath the parent
session directory. Annotation children are ordinary persisted session
directories marked `session_kind=annotation` and `hidden=true`; deleting a card
does not recursively delete its child directory.

## Upstream Synchronization Hotspots

Review these paths carefully whenever `upstream/main` is merged or rebased into
`downstream/main`:

| Area | Hotspots | Downstream invariant to preserve |
|---|---|---|
| Action and input routing | `xai-grok-pager/src/actions/{mod.rs,defaults.rs}`, `app/agent_view/input.rs`, `app/mouse.rs` | `Alt+A` and right-click share the validated selection path; all entry points are disabled in minimal mode. |
| Rendering and layout | `scrollback/{render.rs,scrollback_pane.rs,state/,types.rs}`, `scrollback/wrappers/entry_renderer.rs`, `app/agent_view/render.rs` | Decorations affect layout but never enter transcript blocks, transcript selection, search, replay, or export. Annotation cards expose a separate decoration-only copy/follow-up selection projection. Source-line insertion survives rewrap; missing lines use message-boundary fallback. |
| Parent/child notification routing | `app/acp_handler/{routing.rs,session_notification.rs}`, `app/dispatch/{router.rs,task_result.rs,session/}` | Exact root routing wins; annotation children route only to their owning thread and never switch the active parent view. |
| Fork and persistence | `xai-grok-shell/src/session/{fork.rs,persistence.rs,storage/}` | Fork history ends at `target_prompt_index`; summaries retain hidden annotation kind; parent annotation JSONL is independent of transcript JSONL. |
| Capability enforcement | `xai-grok-shell/src/session/acp_session_impl/{session_setup.rs,sampler_turn.rs,turn.rs,tool_calls.rs,mcp.rs}`, `session/acp_session/hooks.rs` | Persisted annotation policy is authoritative: only registered local read/search/list file tools are exposed and dispatchable; a per-turn hidden reminder names that exact filtered surface and clarifies that broader inherited assumptions do not apply. MCP, mutation, commands, hosted search, memory injection, structured-output pseudo-tools, hooks, and unexpected tool dispatch remain blocked. |

Paths in the table are relative to `crates/codegen/` where the crate prefix is
omitted. Prefer keeping new behavior in the isolated modules above and central
edits mechanical.

## Sync Checklist

1. Fetch upstream and fast-forward the pristine `main` mirror only.
2. Integrate the new upstream commit into `downstream/main` without adding
   downstream commits to `main`.
3. Resolve hotspot conflicts against the invariants above, not just textual
   similarity.
4. Run `cargo fmt --all -- --check`,
   `cargo check -p xai-grok-pager-bin`, `cargo test -p xai-grok-pager`, and
   `cargo test -p xai-grok-shell`.
5. Smoke-test annotation creation/follow-up/reload in the standard TUI and
   confirm `--minimal` behavior remains unchanged.
6. Update this ledger's base/status and the feature plan's execution record.

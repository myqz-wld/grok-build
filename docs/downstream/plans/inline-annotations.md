# Grok Build TUI Inline Annotations — Implementation Plan

## 1. Plan identity

- Goal: add persistent, follow-up-capable inline annotations to historical conversation text in the standard Grok Build TUI.
- Repository: `/Users/wanglidong/Repository/grok-build`
- Upstream base at planning time: `ba76b0a` on `main`, `origin/main`, and `upstream/main`.
- Planning date: 2026-07-22.
- Delivery branch policy:
  - `main`: pristine mirror of `upstream/main`; no downstream commits.
  - `downstream/main`: downstream integration branch.
  - `feature/inline-annotations`: isolated implementation branch based on `downstream/main`.
  - Upstreamable fixes, if discovered, must be split later onto `contrib/*` based directly on `upstream/main`.
- Current repository state: clean before planning.

## 2. Goal and invariants

The standard TUI will let a user select text inside one completed historical User or Assistant message, invoke an annotation action with right-click or `Alt+A`, enter a question, and create a hidden forked session that inherits the parent conversation only through the selected message's prompt turn. Its streamed answer appears as an inline annotation card anchored to the selected source lines. The same card accepts follow-up questions; each answer is appended to the card and continues in the same hidden child session.

The implementation must preserve these invariants:

1. An annotation is UI metadata, not a normal parent transcript message. It must not enter parent model context, transcript export, search, or ordinary replay updates.
2. Source line labels are semantic, 1-based lines in the raw User/Assistant message, never terminal rows after wrapping.
3. Resize, rewrap, scrolling, and parent-session reload must not silently move an annotation to different source text.
4. A new annotation child inherits context only through the selected message's containing prompt turn; later parent turns are excluded.
5. Follow-ups reuse the same child session and append to the same annotation thread.
6. Annotation children are hidden from ordinary history/dashboard listings and cannot execute any local, MCP, web-search, structured-output, or other tool.
7. `main` remains a clean upstream mirror. All implementation happens in an isolated worktree on `feature/inline-annotations`.
8. The first release changes only the standard TUI. `--minimal` behavior remains unchanged.

## 3. Confirmed product decisions

| ID | Decision | Status | Rationale |
| --- | --- | --- | --- |
| D1 | Fork context ends at the selected text's containing prompt turn. | Confirmed by user (`按推荐`) | Prevents later conversation from biasing a historical side discussion. |
| D2 | Annotation sessions have a runtime-enforced zero-tool policy. | Confirmed by user (`按推荐`) | A historical explanation must not mutate the repository or initiate external actions. |
| D3 | V1 supports the standard TUI only; `--minimal` is unchanged. | Confirmed by user (`按推荐`) | Keeps the first vertical slice bounded while preserving future-compatible data structures. |
| D4 | V1 selection must remain within one completed User or Assistant message. | Confirmed as part of the recommended scope | Cross-message anchors and partial streaming entries introduce ambiguous context and line ownership. |
| D5 | Right-click is supported where terminal mouse events arrive; `Alt+A` is the canonical portable shortcut. | Confirmed as part of the recommended scope | Some terminal emulators reserve or intercept right-click. |
| D6 | Annotation cards persist with the parent and child sessions load lazily for follow-up/open. | Confirmed as part of the recommended design | Fast parent restore without loading every child actor. |
| D7 | Deleting a card removes the parent-side annotation record only in V1; it does not recursively delete the child session directory. | Engineering default, reversible | Avoids destructive storage behavior; orphan cleanup can be designed separately. |

## 4. Repository evidence and targeted spikes

### 4.1 Selection and rendering

- `crates/codegen/xai-grok-pager/src/scrollback/text_selection.rs`
  - `RangeHit` and `PersistentTextSelection` currently store visible-entry and rendered-block coordinates.
  - Those coordinates are sufficient for copy highlighting but are unstable across replay and rewrap.
- `crates/codegen/xai-grok-pager/src/app/agent_view/selection.rs`
  - `reconstruct_drag_copy`, `persist_drag_selection`, and `finish_text_drag` already reconstruct the selected string.
  - The annotation action should convert this transient selection into a semantic anchor immediately.
- `xai-grok-markdown::MarkdownContent::line_source_map()` already maps pre-wrap rendered lines to raw Markdown source lines.
  - The pager wrap cache currently retains text/joiners but drops this source map.
  - User-message rendering iterates logical raw lines directly and can attach a source line at wrap time.

### 4.2 Forking and hidden sessions

- `crates/codegen/xai-grok-shell/src/session/fork.rs`
  - `ForkSessionRequest` already accepts `target_prompt_index` (0-based, inclusive) and `session_kind`.
- `crates/codegen/xai-grok-shell/src/session/storage/jsonl/mod.rs`
  - Session copy already truncates both chat and replay updates at `target_prompt_index`.
- `crates/codegen/xai-grok-shell/src/session/persistence.rs`
  - `Summary::is_hidden()` honors an explicit `hidden` override, and list APIs filter hidden summaries.
  - `session_kind="annotation"` plus `hidden=true` is therefore compatible with existing storage/listing semantics.
- Existing `/fork` client orchestration already covers fork, load, and first prompt, but it routes the child as the active top-level session. Annotation flow needs a hidden-session target and must leave the parent active.

### 4.3 Runtime no-tool boundary

- `crates/codegen/xai-grok-shell/src/session/acp_session_impl/sampler_turn.rs`
  - `prepare_tool_definitions_*` is the common built-in/MCP definition path.
- `crates/codegen/xai-grok-shell/src/session/acp_session_impl/turn.rs`
  - `process_conversation_turn` builds the effective tool list and separately considers backend search and structured-output tools.
- A persisted actor capability must gate all of these paths. An empty prompt-level instruction alone is insufficient.
- The actor mode should be derived from the persisted summary/session kind during load, not only from client-provided metadata, so manually reloading an annotation child cannot re-enable tools.

### 4.4 Reusable UI patterns

- `/btw` supplies loading/done/error Markdown presentation patterns, but it is a one-shot full-context sampler and cannot provide a persistent fork or follow-up.
- Plan line comments demonstrate line-range badges and interleaved comment presentation, but annotation data must remain separate from transcript `RenderBlock`s.
- Existing `subagent_views` and `SessionMatch::{Root,Child}` show how the pager routes notifications to non-root session views. Annotation children need an explicit parallel route rather than masquerading as dashboard subagents.

## 5. Selected architecture

### 5.1 Semantic anchor

Add an anchor captured from the active selection:

```text
AnnotationAnchor
  parent_session_id
  transcript_key
  entry_role                 // user | assistant
  target_prompt_index        // 0-based fork cutoff
  start_source_line          // 1-based, inclusive
  end_source_line            // 1-based, inclusive
  selected_text
  selected_text_hash
  surrounding_text_hash      // bounded context for replay validation
```

`transcript_key` is a replay-stable key derived from persisted prompt/update identity, role, and within-turn ordinal. `EntryId` remains an in-memory rendering key only. On reload, resolve the durable key first, then validate the selected quote and surrounding hash. If validation fails, keep the thread in storage and render it in an explicit orphaned state rather than attaching it to a possibly wrong line.

### 5.2 Source-line propagation

Extend the scrollback line model with `source_line: Option<usize>` and propagate it through:

1. Markdown raw source line -> markdown renderer `line_source_map`.
2. Markdown line -> each word-wrapped `BlockLine` fragment.
3. User raw logical line -> each wrapped fragment.
4. Selection endpoints -> inclusive semantic line range.

Synthetic chrome, tool blocks, thinking blocks, image-only ranges, and entries without semantic source lines are not annotatable in V1. Wrapped fragments retain the same semantic line number, so width changes do not alter the label.

### 5.3 Parent-side persistence

Store parent-owned annotation state at:

```text
<parent-session-dir>/annotation_threads.jsonl
```

Use versioned append-only events with deterministic fold-on-load:

```text
AnnotationEvent
  schema_version
  event_id
  timestamp
  thread_id
  kind:
    ThreadCreated { anchor, child_session_id, first_question }
    ExchangeStarted { exchange_id, question }
    AnswerCheckpoint { exchange_id, markdown }
    ExchangeCompleted { exchange_id }
    ExchangeFailed { exchange_id, message }
    ThreadDeleted
```

Checkpoint streaming text at bounded intervals and on terminal state so a crash loses at most the current unflushed suffix. Compaction into a snapshot can be added later; V1 load folds the bounded per-parent log.

### 5.4 Hidden child session

Generalize fork options minimally:

- Set `session_kind="annotation"`.
- Persist `hidden=true` in the copied `Summary`.
- Persist/derive an `annotation` actor capability that forces:
  - zero built-in/MCP tool definitions;
  - backend web search disabled;
  - no structured-output pseudo-tool;
  - no memory injection or autonomous side effects specific to ordinary build sessions;
  - rejection of any unexpected tool-call response as a protocol error, never dispatch.
- Copy only through `target_prompt_index`.
- Strip inherited reasoning as normal forks already support.
- Do not copy plan, plan-mode, signal, tool-state, or announcement files into the child.

The first child prompt is a normal persisted user message containing a short annotation directive, the quoted selection with its source lines, and the user's question. Follow-up prompts contain the new question plus a stable reference to the same selection; the child already holds prior annotation exchanges.

### 5.5 Pager session routing

Add a dedicated annotation runtime registry owned by the parent `AgentView`:

```text
annotation_threads: BTreeMap<ThreadId, AnnotationThreadView>
annotation_sessions: HashMap<SessionId, ThreadId>
```

Extend notification routing with `SessionMatch::Annotation { thread_id }`. Add annotation-specific effect/task-result states for fork, hidden load, prompt, cancel, and lazy resume. Do not insert annotation children into dashboard agent collections, breadcrumbs, active subagent selection, or the ordinary session switch flow.

Multiple threads may stream concurrently. Routing is keyed by child session ID, while each thread permits only one in-flight exchange. A follow-up submitted during an active exchange is rejected with a clear local status message in V1.

### 5.6 Interaction and rendering

- Entry point:
  - `Alt+A` opens the question composer when there is one valid selection.
  - Right mouse release on/inside the selected region opens a small context menu; `Annotate selection` opens the same composer.
- Composer:
  - shows `User/Assistant · Lx-Ly` and a one-line quote preview;
  - supports multiline input using existing prompt-editing conventions;
  - `Esc` cancels without forking;
  - submit creates the thread and starts the child.
- Inline card:
  - inserted after the final rendered fragment whose semantic line equals the anchor end line;
  - header: `Annotation · Assistant · L12-L15` plus status;
  - body contains ordered question/answer exchanges rendered as Markdown;
  - actions: expand/collapse, follow up, open child session intentionally, cancel active answer, delete card record;
  - if the exact insertion line is unavailable because of a collapsed entry, render at the message boundary with the same line badge.
- The card is a scrollback decoration layer, not a `ScrollbackEntry` transcript block.

### 5.7 Minimal-mode boundary

All new input bindings, overlays, decorations, and session orchestration are registered only for the standard TUI path. Shared persistence and semantic anchor types must not depend on standard-TUI widgets, allowing a later minimal-mode renderer without a migration.

## 6. Model and deterministic boundaries

### Model call boundary

The model receives:

- inherited parent context through the selected turn;
- the selected raw text and source-line range;
- a narrowly scoped instruction to explain or answer about that selection;
- the user's initial question or follow-up.

The model produces assistant text only. It receives no tool schema, backend search ability, structured-output requirement, memory injection, or permission channel.

### Deterministic application boundary

The application is solely responsible for:

- validating selection eligibility;
- converting screen coordinates into semantic line ranges;
- selecting `target_prompt_index`;
- forking and marking the child hidden/annotation/no-tools;
- routing stream notifications;
- persisting/folding annotation events;
- validating anchors on replay;
- layout, cards, status transitions, cancellation, and follow-up association.

No model output may choose attachment coordinates, session identity, fork cutoff, storage paths, tool policy, or UI state.

## 7. Alternatives considered

| Alternative | Result |
| --- | --- |
| Extend `/btw` | Rejected: one-shot, full-current-context, no true fork, no follow-up session. Reuse only its Markdown/status UI patterns. |
| Treat annotation child as an ordinary fork and switch to it | Rejected: disrupts the parent view and leaks into normal history/navigation. |
| Treat annotation child as a subagent | Rejected: incorrect semantics and would couple annotations to dashboard/task lifecycle. |
| Insert answers as parent transcript blocks | Rejected: contaminates replay, search/export, and future model context. |
| Store only terminal row numbers | Rejected: anchors drift on resize, font/width changes, and Markdown reflow. |
| Put answers only in an ephemeral overlay | Rejected: annotations would disappear from their textual context and on restart. |

## 8. Implementation tasks

### T1 — Isolate downstream work

- Create local `downstream/main` at pristine `main` if absent.
- Enter an Agent Deck worktree for `feature/inline-annotations` based on `downstream/main`.
- Use an explicit sibling worktree path because the agreed workflow forbids adding `.agent-deck/` to pristine `main` solely for tooling.
- Add this approved plan to the feature branch as the durable implementation record.

Acceptance: `main` remains byte-for-byte at upstream base; implementation branch/worktree is active and clean.

### T2 — Semantic line metadata and anchor extraction

- Add source-line metadata to wrapped scrollback lines.
- Preserve Markdown `line_source_map` through caching and wrapping.
- Add user-message logical-line mapping.
- Add selection-to-anchor conversion and eligibility errors.
- Add replay-stable transcript keys and prompt-index metadata to annotatable entries.

Acceptance: unit tests cover plain text, Markdown list/code/table, blank lines, CJK, narrow/wide resize, and invalid cross-message selection.

### T3 — Annotation storage domain

- Add versioned event types, JSONL reader/writer, fold logic, atomic append discipline, and orphan validation.
- Keep filesystem paths parent-session scoped.
- Add bounded answer checkpointing.

Acceptance: round-trip, partial-last-line recovery, delete tombstone, duplicate event, missing child, and anchor-mismatch tests pass.

### T4 — Fork/session safety extensions

- Extend fork copy options/request to persist `hidden=true` and annotation session policy.
- Disable unnecessary copied state for annotation children.
- Add persisted actor capability for annotation sessions.
- Gate tool definitions, backend search, structured-output tool, memory injection, and unexpected tool dispatch.

Acceptance: copied history ends at selected prompt index; summary is hidden/kind annotation; request snapshots contain no tools/backend search; forced synthetic tool-call cannot dispatch.

### T5 — Hidden annotation orchestration

- Add annotation fork/load/prompt/cancel effects and results.
- Add session-ID-to-thread routing and streamed Markdown accumulation.
- Restore cards when parent loads; lazy-load child on follow-up/open.
- Ensure root and subagent routing regressions are unchanged.

Acceptance: two concurrent annotation children route independently; follow-up remains in the original child; parent stays active; failures update only their owning card.

### T6 — Standard TUI interaction

- Add `Alt+A` binding.
- Add right-click selection context menu.
- Add composer and validation/status messaging.
- Add inline decoration cards and actions.
- Guard all new surfaces from minimal mode.

Acceptance: keyboard-only and mouse flows work; cancel creates no child; card stays on semantic lines after resize; collapsed entry fallback is deterministic.

### T7 — Integration, docs, and upstream-maintenance record

- Add concise user-facing keybinding/help documentation.
- Add a downstream feature ledger documenting the long-lived delta and upstream conflict hotspots.
- Format, lint/check, and run targeted plus crate-level tests.
- Review diff for accidental changes to generated/vendor files and for `main` contamination.

Acceptance: validations below pass or any environmental failure is recorded with the exact command/output and no known product regression remains.

### T8 — Post-delivery review remediation

Remediate all eight findings confirmed by the independent review of
`ba76b0a..4928862` without reopening the approved product decisions:

1. Preserve raw Markdown source identity at selectable span/column granularity so
   text on either side of a collapsed soft break anchors to its own semantic line.
2. Gate direct `SessionStart` and both `SessionEnd` lifecycle-hook paths with the
   persisted annotation actor policy, covering both client and file hooks.
3. Keep Open child available while routing annotation notifications by the
   running annotation prompt, so a coexisting root view cannot steal a follow-up.
4. Make append failures reconcile every queued persistence request and leave no
   unrelated thread stranded in an optimistic or in-flight state.
5. Cancel Persisting/LoadingChild exchanges locally, persist a terminal state,
   and require exchange/phase matches before every later continuation.
6. Restore annotations for every persisted non-minimal parent, including
   chat-kind sessions, and reject creation clearly only when no local persistence
   target exists.
7. Cache expanded-card Markdown/layout by content revision, width, theme, and
   expansion state so idle, hover, and duplicate streaming-revision redraws do
   not reparse or rehash unchanged answer text.
8. Surface concise, non-blocking annotation-load recovery warnings and errors.

Add focused regression coverage for collapsed soft breaks at multiple widths
(including CJK/formatting), all lifecycle-hook paths, root/annotation routing
coexistence, multi-thread and terminal append failures, cancellation in every
pre-prompt phase, chat-kind reload, render-cache hits/invalidation, and load
diagnostics. Run the complete validation plan plus standard/minimal smoke checks
and record baseline-only failures precisely.

Final review uses one retained `gpt-5.6-sol` reviewer at `max` thinking. Reuse
that same child session for every fix/re-review round until it explicitly passes;
do not replace it or add parallel reviewers.

Acceptance: all eight regressions are fixed with focused tests; the full pager and
shell validation is no worse than the recorded `main@ba76b0a` baseline; the fix is
committed and pushed on `fix/inline-annotations-review`, then fast-forwarded and
pushed to `feature/inline-annotations`; pristine `main` remains unchanged.

## 9. Validation plan

Run the narrowest tests during development, then:

```text
cargo fmt --all -- --check
cargo check -p xai-grok-pager-bin
cargo test -p xai-grok-pager
cargo test -p xai-grok-shell
```

Manual smoke test in a standard TUI:

1. Open a saved multi-turn session.
2. Select wrapped Markdown text in an earlier Assistant message.
3. Use `Alt+A`, cancel, and confirm no session/storage record appears.
4. Repeat, submit a question, and confirm the parent remains visible while the card streams.
5. Resize narrower and wider; verify the badge and attachment remain on the same raw lines.
6. Add a follow-up; verify it appends and uses the same child ID.
7. Create a second annotation and verify independent streaming/routing.
8. Restart and reload the parent; verify cards restore and follow-up lazily resumes the child.
9. Inspect child summary/history listing and sampler request capture: hidden, cutoff correct, zero tools/search.
10. Confirm `--minimal` launches and behaves as before.

## 10. Failure and recovery behavior

- Fork failure: keep a retryable failed draft card; no child ID is assumed.
- Load/prompt failure: persist exchange failure and allow retry/follow-up after a successful lazy reload.
- Parent closes during stream: checkpoint received text; child may finish independently; reconcile on next parent load.
- Corrupt final JSONL record: ignore only the incomplete tail, preserve earlier events, and surface a non-fatal warning.
- Anchor mismatch after replay/schema evolution: mark orphaned; never auto-reattach using line numbers alone.
- Child missing: retain readable prior exchanges; disable follow-up until user explicitly starts a replacement thread (future enhancement if not included in V1).
- Unexpected tool call: do not dispatch; terminate that exchange with a policy error.

## 11. Known risks and mitigations

- Pretty Markdown can collapse multiple raw source lines into one rendered row. Mitigation: completed-message rendering now preserves raw source identity in selectable display-column spans, and wrapping clips/rebases those spans before anchor construction; the scalar row map remains only as a fallback for non-linear/specialized rows.
- Persisted transcript identity may not currently expose one universal key. Mitigation: introduce a narrow replay identity from prompt index + role + ordinal and validate quote/context hashes.
- Event routing changes touch central dispatch. Mitigation: explicit `Annotation` route variant and regression tests for Root/Child paths.
- Terminal right-click portability varies. Mitigation: `Alt+A` is canonical; mouse is additive.
- Upstream churn in large pager/session files can cause conflicts. Mitigation: isolate new domain modules, keep edits to central enums/dispatch mechanical, and record hotspots in the downstream ledger.
- Full crate tests may be expensive. Mitigation: targeted unit tests first; crate tests before completion; record only genuine environment blockers.

## 12. Explicit exclusions for V1

- No cross-message or multi-entry selection.
- No annotation on tool output, thinking, diff, image-only, or actively streaming content.
- No `--minimal` interaction/rendering.
- No collaborative/shared annotation sync across machines.
- No automatic child-session directory deletion.
- No annotation export/search integration.
- No arbitrary dynamic plugin API; this is a maintained downstream feature implemented behind cohesive internal interfaces.

## 13. Execution state and cold-start continuation

- Planning: approved by the user on 2026-07-22.
- Isolation (T1): complete.
- Implementation: T2–T8 complete; retained remediation review passed.
- Main repository: `/Users/wanglidong/Repository/grok-build`, still checked out on pristine `main` at `ba76b0a`.
- Delivered implementation worktree: removed after commit `4928862` was pushed.
- Active remediation worktree: `/Users/wanglidong/Repository/grok-build-worktrees/inline-annotations-fixes`.
- Branches:
  - base: `downstream/main` at `ba76b0a`;
  - delivered feature: `feature/inline-annotations` at `4928862`;
  - remediation: `fix/inline-annotations-review`, based on `feature/inline-annotations`.
- Agent Deck tasks:
  - T1 `770e449c-8332-4967-9a2e-204c60cea06a` — isolate worktree;
  - T2 `eadf1639-fa6b-4190-a561-fcfe0ec69751` — semantic lines and anchors;
  - T3 `d679fb1a-e4a6-4bbe-b942-e1bc9113307f` — annotation storage;
  - T4 `b05ed15c-e0ef-4b3b-8749-b1fdf997dfbc` — hidden no-tool forks;
  - T5 `2affc384-0421-4084-8f26-8a3c0cb18f5e` — session routing;
  - T6 `07bc7bd0-d4f5-4c3d-a753-c23684a1fd24` — standard TUI;
  - T7 `2c986083-0c0d-42d8-bc09-a09b3fc809ee` — validation/docs;
  - T8 `497d1845-0061-47a3-a41f-4c74609cc696` — review remediation.
- T2 completion (2026-07-22):
  - added 1-based raw source-line metadata to completed User/Assistant rows;
  - preserved Markdown source maps through wrapping and completion-time cache rebuilds;
  - added replay-stable prompt/role/ordinal transcript keys and selection-to-anchor conversion;
  - covered plain/user text, Markdown lists/code/tables, blank lines, CJK, narrow/wide wrapping, streaming completion, unsupported/running content, and cross-message rejection;
  - targeted annotation/source-line tests pass and `cargo fmt --all -- --check` is clean.
- T3 completion (2026-07-22):
  - added the versioned parent-session `annotation_threads.jsonl` event model and locked, durable append path;
  - added deterministic folding, duplicate-event handling, deletion tombstones, torn-tail recovery/healing, and schema/corruption warnings;
  - added bounded streaming answer checkpoint gating and explicit missing-child/transcript/anchor-mismatch orphan states;
  - all focused annotation domain/storage tests pass (11 tests at completion).
- T4 completion (2026-07-22):
  - annotation forks now require a prompt cutoff, persist `session_kind=annotation` plus `hidden=true`, and skip plan/mode/signals/tool/announcement/compaction-archive state;
  - actor capability policy is derived from the persisted summary on load and cannot be supplied through ACP startup metadata;
  - annotation requests expose no built-in/MCP tools, hosted search, memory context, or native/pseudo-tool structured output; MCP reminders and hook dispatch are also suppressed;
  - any unexpected model tool call is rejected with an ACP protocol error before tool lifecycle events or dispatch;
  - focused fork/cutoff/hidden-state and actor-policy tests pass (6 tests at completion), and `cargo check -p xai-grok-shell` passes.
- T5 completion (2026-07-22):
  - added parent-owned annotation runtime state, strict FIFO event persistence, lazy hidden-child loading, and one-in-flight exchange enforcement per thread;
  - routed annotation child ACP chunks and terminal states by child session id without changing the active parent or ordinary subagent views;
  - added bounded streamed-answer checkpoints, event-sequence deduplication, isolated cancellation/failure handling, and interrupted-stream recovery;
  - covered fork/persist/load/prompt ordering, follow-up reuse, concurrent child isolation, cancellation isolation, and root/child/annotation routing (18 focused annotation tests plus 4 routing tests at completion);
  - `cargo check -p xai-grok-pager` passes.
- T6 completion (2026-07-22):
  - added standard-TUI `Alt+A` and exact-selection right-click entry points backed by one multiline annotation composer; cancelling the composer creates no child;
  - added a transcript-independent scrollback decoration layer that inserts cards after the last wrapped fragment of the anchored semantic source line and falls back deterministically to the message boundary;
  - added persistent collapsed/expanded cards, streamed Markdown answers, line/role/status badges, follow-up, open-child, cancel, and delete actions with narrow-width wrapping and hit targets;
  - restored cards default collapsed, live cards stay expanded, and orphan cards remain visible at a deterministic fallback boundary;
  - guarded composer, mouse handling, decorations, and dispatch from `--minimal`; focused keyboard, mouse, card-action, resize, fallback, and render-placement tests pass (26 annotation tests and 6 decoration tests at completion);
  - `cargo check -p xai-grok-pager` passes without warnings.
- T7 completion (2026-07-22):
  - added standard-TUI usage/keybinding documentation and a downstream feature ledger with the branch policy, durable invariants, and upstream conflict hotspots;
  - final review added rollback from a failed initial `ThreadCreated` append to a retryable draft, preventing an in-memory thread that could not survive reload;
  - `cargo fmt --all -- --check`, `cargo check -p xai-grok-pager-bin`, and `git diff --check` pass;
  - focused tests pass: 33 annotation pager tests, 6 decoration/layout tests, and 6 annotation fork/runtime-policy shell tests;
  - `cargo test -p xai-grok-pager` completes with 7416 passed, 11 failed, and 10 ignored. The 11 failures are pre-existing host theme/platform expectations (six skill-token color tests, three theme cursor/background tests, and two macOS `Opt+Enter` label tests). A representative skill-color failure reproduces on pristine `main@ba76b0a`, all five non-user-message failing source files are unchanged from that base, and single-thread reruns preserve the same failure set;
  - `cargo test -p xai-grok-shell` completes with 5733 passed, 1 failed, and 13 ignored. The sole `claude_import::tests::gate_load_claude_env_returns_empty_when_marker_set` environment failure reproduces on pristine `main@ba76b0a`; all six new annotation safety tests pass;
  - PTY smoke checks launch and quit the standard TUI and `--minimal` successfully. Full live create/stream/follow-up/reload E2E was not sent because this environment stops at browser authentication; no model request or external mutation was made;
  - the primary repository remains clean on `main` at `ba76b0a683fa52e4e60685017b85905451be17bc`.
- T8 start (2026-07-22): an independent complete read-only review reported four HIGH, three MEDIUM, and one LOW finding; the user explicitly authorized fixing all eight. The isolated remediation worktree and branch above were created from `4928862`, with no change to pristine `main`.
- T8 implementation and local validation (2026-07-22):
  - added display-column source spans through completed Markdown rendering and wrapping, with wide/narrow formatted ASCII and CJK soft-break selection coverage;
  - gated client and file hook dispatch through the annotation actor policy for `SessionStart`, both `SessionEnd` reasons, Stop, notification, and subagent-start paths;
  - made root/annotation coexistence prompt-aware for ACP/xAI chunks, queue broadcasts, and prompt completion while retaining ordinary root ownership for ordinary prompts;
  - made append failure drain and reconcile every queued/active thread, close unhealthy storage to new work, and cover multi-thread plus terminal-event failure;
  - made Persisting/LoadingChild cancellation local and terminal, added exchange/phase guards to every continuation, and kept ACP cancel exclusive to Prompting;
  - restored annotation logs for non-minimal chat-kind parents, cached card bodies by content/width/theme/expansion with bounded invalidation coverage, and surfaced concise load warning/error toasts;
  - focused results: all 44 annotation pager regressions passed, all 472 `xai-grok-markdown` tests passed, and all 7 annotation shell policy tests passed;
  - `cargo check -p xai-grok-pager-bin`, `cargo build -p xai-grok-pager-bin --bin xai-grok-pager`, `cargo fmt --all -- --check`, and `git diff --check` pass;
  - `cargo test -p xai-grok-pager` completed with 7427 passed, 11 failed, and 10 ignored. The same six skill-token color, three cursor/theme, and two macOS `Opt+Enter` failures are the recorded pristine-main baseline; there are no new failures;
  - `cargo test -p xai-grok-shell` completed with 5734 passed, 1 failed, and 13 ignored. The sole `claude_import::tests::gate_load_claude_env_returns_empty_when_marker_set` failure is the recorded pristine-main baseline;
  - explicit PTY smoke tests passed for standard welcome-screen launch/quit and minimal-mode launch/double-Ctrl+C quit. No authenticated live model request or other external mutation was sent.
- T8 retained-review round 1 (2026-07-22): the same read-only `gpt-5.6-sol`/`max` reviewer confirmed CODEX-001/005/006/007/008 fixed, but found four remaining issues: direct turn Stop/workspace-hook rails, durable terminal/retry coexistence routing, live prompt draining after append failure, and quadratic source-span wrapping.
- T8 retained-review round 1 remediation (2026-07-22):
  - `run_stop_gate` now fails open before consulting file/client hooks when actor policy disallows hooks, and both workspace before/after-turn helpers no-op at the same policy boundary; direct file/client Stop and workspace-spy tests include Standard-actor positive controls;
  - durable `TurnCompleted` routing derives prompt identity from its payload before owner selection, live prompt-scoped xAI updates inherit `promptId`, and terminal emission explicitly preserves that id after the live prompt pin is cleared; coexistence coverage now includes legacy and durable terminals (live/replayed), annotation/root retry updates, and ordinary root completion;
  - append failure now cancels every genuinely Prompting exchange, avoids duplicate cancellation for Cancelling exchanges, retains a prompt-ownership drain tombstone until the exact prompt terminal, ignores late chunks, and never lets that terminal overwrite the storage failure; multi-stream and coexisting-root tests cover the drain lifecycle;
  - source-span wrapping now advances monotonic byte/display-column and semantic-span cursors, making ordinary long-line wrapping linear; a 4,096-soft-break scaling regression preserves every source line at width 1;
  - post-remediation focused results pass: 46 pager annotation tests, 8 shell annotation/fork/policy tests, 9 durable turn-completion emission tests, the 4,096-span performance regression, formatting, and diff checks.
- T8 retained-review round 1 full validation (2026-07-22):
  - `cargo fmt --all -- --check`, `cargo check -p xai-grok-pager-bin`, and `git diff --check` pass;
  - `cargo test -p xai-grok-pager` completes with 7430 passed, 11 failed, and 10 ignored. The 11 failures are the same recorded pristine-main baseline: six skill-token color expectations, three cursor/theme expectations, and two macOS `Opt+Enter` labels;
  - the default concurrent shell run exposed an unchanged `test_pool_fill_creates_worktrees` cleanup race that left `git status` in a deleted temporary cwd; that exact test passed alone. A complete single-thread rerun then finished with 5736 passed, 1 failed, and 13 ignored, with only the already-recorded `claude_import::tests::gate_load_claude_env_returns_empty_when_marker_set` baseline failure;
  - ignored PTY tests passed explicitly for the standard welcome-screen launch/quit and minimal-mode first/second Ctrl+C confirmation/quit paths;
  - the primary repository remains clean on `main` at `ba76b0a683fa52e4e60685017b85905451be17bc`.
- T8 retained-review round 2 (2026-07-22): the same reviewer confirmed the
  original CODEX-001/002/004/005/006/007/008 findings and the source-span
  performance follow-up fixed. CODEX-003 had one remaining MEDIUM regression:
  the shared xAI sender attributed session-global broadcasts such as manual
  recap and hook/plugin registry changes to an unrelated live annotation
  prompt, so root-first coexistence routing could drop them.
- T8 retained-review round 2 remediation (2026-07-22): xAI prompt attribution
  is now an explicit allowlist for retry, automatic compact/continue/recovery,
  and prompt image-processing updates; session-global broadcasts remain
  unscoped, while payload-identified terminals retain their authoritative id.
  Shell coverage distinguishes a prompt-scoped retry from an unscoped recap
  terminal, and pager coexistence coverage verifies the ordinary child root
  consumes the recap terminal without disturbing annotation prompt ownership.
- T8 retained-review round 2 validation (2026-07-22): the focused shell
  attribution test, the root/annotation recap coexistence regression, all 46
  pager annotation tests, all 8 annotation fork/policy shell tests, and all 9
  durable turn-completion tests pass. `cargo fmt --all -- --check`,
  `cargo check -p xai-grok-pager-bin`, and `git diff --check` pass. Full pager
  remains 7430 passed / 11 recorded baseline failures / 10 ignored; full
  single-thread shell remains 5736 passed / the one recorded `claude_import`
  baseline failure / 13 ignored. Explicit standard welcome-screen and minimal
  double-Ctrl+C PTY smoke tests pass, and pristine `main` remains unchanged.
- T8 retained-review round 3 (2026-07-22): the same retained read-only
  `gpt-5.6-sol`/`max` reviewer completed another full-file and call-site audit,
  reported no findings, confirmed every original CODEX-001 through CODEX-008
  disposition plus both prior follow-up rounds fixed, and returned explicit
  `REVIEW PASS`.
- T8 delivery (2026-07-22): the reviewed remediation is committed and pushed
  on `fix/inline-annotations-review`, `feature/inline-annotations` is
  fast-forwarded and pushed to the same commit without force, both branches are
  retained, the isolated remediation worktree is removed, and the primary
  repository remains pristine on `main@ba76b0a`.
- Current state: T8 is complete; no implementation, validation, review, or
  delivery work remains.

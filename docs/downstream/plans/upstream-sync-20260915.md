# Upstream sync and local deployment — 2026-09-15

## Goal and scope

Merge `upstream/main@482711333c7195dc16a272777f86086d615e2afb` (Grok 1.0.32)
into `downstream/main@e0a6bb5ab8aa29b07b1c3ca59815451945a6635e`, validate,
publish the integration and pristine mirror, and deploy the release binary
through the existing local `grok` installation with a recoverable previous version.

## Invariants

- `main` is a pristine upstream mirror; downstream changes stay on `downstream/main`.
- Preserve persistent threaded inline annotations, including source-line anchors,
  parent/child routing, hidden persistence, and enforced read-only capabilities.
- Preserve Responses Lite streamed text and function-call fallback while keeping
  terminal response content authoritative.
- Preserve upstream functionality and the standard/minimal UI boundary.
- Use the repository's Rust 1.94.0 toolchain; leave global runtimes unchanged.
- Do not restart or replace Agent Deck processes or its application bundle.
- Install only after validation; preserve the previous binary and existing sessions.

## Decisions

- Work in the isolated `sync/upstream-20260915` branch from the downstream tip.
- The user authorized upstream integration and redeployment; the existing deployment
  is a local release binary reached through `~/.local/bin/grok`.
- The upstream update contains 10 sync commits and conflicts in 36 files.
- Resolve conflicts against the maintained feature ledger, with targeted tests
  before broader validation. Record upstream/environment baseline limitations.

## Progress

- [x] Fetch origin/upstream, inspect clean starting state and deployment target.
- [x] Create isolated worktree and integration branch.
- [x] Resolve Markdown, pager, shell, workspace, sampler, and lockfile conflicts.
- [x] Format, check pager binary, and run focused downstream regression tests.
- [ ] Run pager/shell suites and standard/minimal startup checks.
- [x] Update feature ledger and execution record; commit integration.
- [ ] Build release from the committed tree; verify reported version and commit.
- [ ] Fast-forward and push downstream/main and pristine main without force.
- [ ] Back up/install locally, verify deployment, and restore the primary workspace.

## Validation and risks

Required: `cargo fmt --all -- --check`, `cargo check -p xai-grok-pager-bin`,
pager and shell test suites, focused annotation and Responses stream tests,
release build, and standard/minimal startup smoke tests. Compare failures against
the exact upstream code when necessary; do not hide skipped or blocked checks.

The previous sync recorded macOS-dependent test failures, missing public-tree
internal documentation fixtures, and an upstream CryptoProvider test conflict.
Confirm the current status before classifying any failure as baseline.

## Next action

Build the release artifact from the integration commit, then run real-terminal
annotation, standard/minimal startup, and shell integration checks before delivery.

Added an ignored PTY regression in `xai-grok-pager-pty-harness` for annotation
creation, restart/reload, follow-up in the same hidden child, and parent transcript
isolation. It uses the harness's isolated home and local mock inference.

Resolved all 36 conflicted files. Preserved upstream session identity/load guards,
MCP generation ownership and startup bounds, typed cancellation/error metadata,
child-tool projection, render groups, and source-span propagation. Reapplied the
annotation gate to the new deferred session-start/session-end hook paths.

## Unresolved questions

None at start; deployment follows the repository's previous local delivery.

### Validation results

- `cargo check --locked -p xai-grok-pager-bin`: passed (Rust 1.94.0).
- `cargo fmt --all -- --check` and downstream-vs-upstream whitespace check: passed.
- Focused pager annotation tests: 57 passed, 0 failed.
- Initial full pager lib run reached the end of the test list but three unchanged
  upstream `doctor_cmd` tests hung beyond 60 seconds in the known macOS probe
  family; the exact test process was terminated after 2 minutes. The next run
  skips those three cases.
- One downstream cache test raced global theme mutation in the full concurrent
  suite (the focused suite passed). Added upstream's shared `pin_theme()` guard;
  a fresh full run will validate the correction.
- Focused shell annotation tests: 13 passed, 0 failed (includes 12 downstream
  capability/fork regressions and one upstream hook-annotation case).

- Fresh complete pager lib run, excluding the three confirmed probe hangs:
  **10018 passed, 2 failed, 4 ignored, 3 filtered**. Both failures are the
  unchanged upstream dashboard expectations of Shift/Alt+Enter where macOS
  correctly renders Opt+Enter; their source and test files match `upstream/main`.
  The guarded annotation cache regression passes in this full run.

- Complete shell lib run: **6859 passed, 2 failed, 5 ignored**. Both failing
  modules are unchanged from `upstream/main`: empty facet-filter normalization
  in `session/unified_list/mod.rs`, and the macOS `/private/var` versus `/var`
  path expectation in `session/workflow/registry.rs`.
- Responses stream regressions: **29 passed**, including minimal terminal text
  and function-call fallbacks and terminal-content precedence.
- Complete Markdown lib suite: **484 passed**.

- Read-only shell parser: **4 passed**.
- Pager documentation integration target cannot compile because upstream omits
  `docs/internal/25-enterprise.md` and `docs/internal/22-environment-variables.md`.
  The missing files are also absent from `upstream/main` (verified with Git).

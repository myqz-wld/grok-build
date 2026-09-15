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
- [x] Run pager/shell suites and standard/minimal startup checks.
- [x] Update feature ledger and execution record; commit integration.
- [x] Build release from the committed tree; verify reported version and commit.
- [ ] Fast-forward and push downstream/main and pristine main without force.
- [x] Back up/install locally and verify deployment.
- [ ] Restore the primary workspace and remove the leased worktree.

## Validation and risks

Required: `cargo fmt --all -- --check`, `cargo check -p xai-grok-pager-bin`,
pager and shell test suites, focused annotation and Responses stream tests,
release build, and standard/minimal startup smoke tests. Compare failures against
the exact upstream code when necessary; do not hide skipped or blocked checks.

The previous sync recorded macOS-dependent test failures, missing public-tree
internal documentation fixtures, and an upstream CryptoProvider test conflict.
Confirm the current status before classifying any failure as baseline.

## Next action

Fast-forward and push the original integration branch, publish the pristine
mirror, then restore the primary workspace and remove the leased worktree.

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

### Integration artifact

- Functional merge commit: `be4bb23da4c2cf15a42c220a9d5a8243175f87a8`.
- Parents: downstream `e0a6bb5a` and upstream `48271133`.
- Release build uses `GROK_VERSION=1.0.32`, the pinned Rust toolchain, and
  `cargo build --locked -p xai-grok-pager-bin --release`.
- Pre-deployment installed binary: Grok 1.0.6 (`e0a6bb5`), SHA-256
  `084f4b8790cf17fc24b3b8d2a531c960309322cfc384879034e72cc546fd0bc7`.

- PTY smoke, minimal-mode, and downstream annotation test binaries compiled
  successfully. Waiting for the release binary before executing them.

- Release build passed in 14m 15s. Artifact reports `grok 1.0.32 (be4bb23da4c2)`;
  SHA-256: `2c9d9e1ad47619109551bb7b2a590e10e7bb213fb02a932988642473633ecc0f`.
- Local pristine `main` was fast-forwarded to the frozen upstream commit.

- Release PTY checks all passed: standard welcome/quit, minimal Ctrl+C
  confirmation/quit, and annotation create → restart/reload → follow-up.
  The annotation test confirms one persisted child and two completed exchanges,
  with neither answer in the parent transcript.
- Default shell integration compilation hit an upstream harness configuration
  issue: `reset_startup_settings_for_tests` is gated by `test-support`, but a
  default integration target uses it. Running the integration/binary tests with
  the repository's `test-support` feature and the actual release artifact.

### Completed runtime validation and local deployment

- Shell integration/binary tests with `--features test-support`: **35 targets,
  131 passed, 0 failed, 3 ignored**. Tests that spawn Grok use this release via
  `GROK_BINARY`, `GROK_BINARY_LEADER`, and `GROK_BINARY_CLIENT`.
- Shell doctest command succeeds; both doctest examples are marked ignored
  upstream (0 run, 2 ignored).
- Installed: `grok 1.0.32 (be4bb23da4c2)` through the existing
  `~/.local/bin/grok -> ~/.grok/bin/grok` link. Both Grok and legacy agent
  aliases now select `grok-1.0.32-macos-aarch64-downstream-be4bb23da4c2`.
- Verified the installed version and SHA-256 against the tested artifact.
- Preserved the previous download and an independent executable backup at
  `~/.grok/backups/grok-1.0.6-e0a6bb5-20260915`.
- Rollback can point both `~/.grok/bin/grok` and `~/.grok/bin/agent` at that
  backup; retain the outer `~/.local/bin/grok` link.
- No Agent Deck process or app bundle was restarted or replaced.

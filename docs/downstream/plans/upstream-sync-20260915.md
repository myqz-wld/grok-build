# Upstream sync and local deployment — 2026-09-15

## Outcome

Completed upstream integration and local deployment of **Grok 1.0.32**.

- Upstream: `482711333c7195dc16a272777f86086d615e2afb` (10 sync commits).
- Previous downstream: `e0a6bb5ab8aa29b07b1c3ca59815451945a6635e`.
- Functional merge: `be4bb23da4c2cf15a42c220a9d5a8243175f87a8`.
- Installed version: `grok 1.0.32 (be4bb23da4c2)`.
- `main` mirrors upstream; `downstream/main` contains the merge and delivery
  records. Both branches were pushed to origin without force.
- The primary workspace is restored on `downstream/main`; the leased worktree
  was removed after its intended files were committed.

## Goal, scope, and invariants

Merge current upstream into the maintained downstream branch, validate the
combined behavior, publish the branches, and redeploy the local release binary.

Preserved:

- Persistent threaded inline annotations: semantic source-line anchors,
  hidden child persistence, prompt-aware routing, and enforced local read-only
  capabilities. Annotation answers remain outside the parent transcript.
- Responses Lite streamed text and function-call fallback, with terminal
  response content authoritative when present.
- Upstream session loading, identity, cancellation, MCP lifecycle, rendering,
  and the standard/minimal UI boundary.
- The pristine upstream-only `main` policy and repository Rust 1.94.0 toolchain.
- The previous executable and existing user sessions. Agent Deck processes and
  the application bundle were not restarted or replaced.

## Integration decisions

The integration used an isolated `sync/upstream-20260915` branch and resolved
36 conflicted files. Conflicts were resolved against the feature ledger,
including source-span propagation, render-group offsets, cold-load identity
checks, MCP generation ownership and bounded startup, typed terminal metadata,
and child-tool projection. Annotation gates were reapplied to the new deferred
session-start and session-end hook paths.

Test fixtures were migrated to the new retry/load/synthetic-message types.
The annotation cache test now uses upstream's shared theme guard so concurrent
theme-changing tests cannot invalidate its idle-redraw assertions.

A new ignored PTY regression in `xai-grok-pager-pty-harness/tests/downstream_annotations.rs`
uses an isolated home and local mock inference to check creation, restart/reload,
and follow-up in the same hidden child. It confirms two completed exchanges and
that neither answer appears in the parent transcript.

## Validation

| Check | Result |
| --- | --- |
| `cargo fmt --all -- --check` | Passed |
| `cargo check --locked -p xai-grok-pager-bin` | Passed |
| Downstream diff whitespace check | Passed |
| Pager annotation regressions | 57 passed |
| Shell annotation/fork/capability regressions | 13 passed, including one upstream hook-annotation case |
| Responses stream regressions | 29 passed |
| Complete Markdown lib suite | 484 passed |
| Read-only shell parser | 4 passed |
| Pager lib suite | 10018 passed, 2 failed, 4 ignored, 3 filtered |
| Shell lib suite | 6859 passed, 2 failed, 5 ignored |
| Shell integration/binary tests with `test-support` | 35 targets: 131 passed, 0 failed, 3 ignored |
| Shell doctest command | Succeeded; both examples are ignored upstream (0 executed, 2 ignored) |
| Release build | Passed in 14m 15s |
| Release standard TUI welcome/quit | Passed |
| Release minimal Ctrl+C confirmation/quit | Passed |
| Release annotation creation/reload/follow-up | Passed |

The release was built with `GROK_VERSION=1.0.32` and
`cargo build --locked -p xai-grok-pager-bin --release`. PTY tests used
`PAGER_BINARY` pointing to that artifact. Shell process tests used the same
artifact through `GROK_BINARY`, `GROK_BINARY_LEADER`, and `GROK_BINARY_CLIENT`.

### Confirmed upstream/environment limitations

- The two pager failures are unchanged dashboard tests expecting Shift/Alt+Enter
  where macOS displays Opt+Enter.
- Three unchanged `doctor_cmd` tests hung in the known macOS probe family.
  The exact initial test process was terminated after two minutes; the completed
  pager run filtered those cases.
- The two shell failures are in unchanged upstream modules: empty facet-filter
  normalization in `session/unified_list/mod.rs`, and the `/private/var` versus
  `/var` path expectation in `session/workflow/registry.rs`.
- The pager documentation integration target cannot compile because upstream
  omits `docs/internal/25-enterprise.md` and `docs/internal/22-environment-variables.md`.
  Git confirms both files are absent from the exact upstream commit.
- Default shell integration compilation exposes an upstream test-configuration
  mismatch: the harness calls `reset_startup_settings_for_tests`, which is gated
  by `test-support`. The integration/binary suite passes with that repository
  feature enabled.

All failing production/test paths above were compared with the exact upstream
commit. These limitations were not hidden or changed into production behavior.
Logs and deployment metadata are retained under `target/upstream-sync-20260915-*`.

## Deployment and rollback

- Entry point: `~/.local/bin/grok -> ~/.grok/bin/grok`.
- Both `~/.grok/bin/grok` and `~/.grok/bin/agent` now point to
  `../downloads/grok-1.0.32-macos-aarch64-downstream-be4bb23da4c2`.
- Installed artifact SHA-256:
  `2c9d9e1ad47619109551bb7b2a590e10e7bb213fb02a932988642473633ecc0f`.
- Previous version: `grok 1.0.6 (e0a6bb5)`.
- Independent executable backup:
  `~/.grok/backups/grok-1.0.6-e0a6bb5-20260915`.
- Backup SHA-256:
  `084f4b8790cf17fc24b3b8d2a531c960309322cfc384879034e72cc546fd0bc7`.

Installation verified both hashes and the reported version before atomically
replacing each alias. The previous download remains available as well. Rollback
can point both inner aliases at the backup while retaining the outer
`~/.local/bin/grok` link.

## Progress

- [x] Fetch and inspect upstream, downstream, and deployment state.
- [x] Isolate integration and resolve conflicts.
- [x] Validate source, downstream regressions, and full suites with recorded limits.
- [x] Commit integration and build the release from the committed source.
- [x] Validate the actual release in standard, minimal, and annotation PTY flows.
- [x] Back up, install, and verify the local executable.
- [x] Fast-forward and publish the pristine mirror and downstream branch.
- [x] Restore the primary workspace and remove the leased worktree.

## Next action and unresolved questions

None. Delivery is complete.

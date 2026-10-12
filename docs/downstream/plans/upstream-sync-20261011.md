# Upstream sync and local deployment — 2026-10-11

## Goal and scope

Merge `upstream/main@2bdd1d6a` (Grok 1.0.45, six upstream sync commits)
into `downstream/main`, validate the maintained downstream behavior, and
reinstall the local release through the existing executable aliases.
The starting downstream commit is `89d3ecbb`; the previous upstream base
is `48271133`. The checkout was clean before this task.

## Invariants and decisions

- Preserve persistent threaded inline annotations, hidden child persistence,
  exact parent/child routing, read-only capabilities, and the minimal-mode boundary.
- Preserve Responses Lite streamed text/tool-call fallback, with terminal
  response data authoritative when supplied.
- Keep `main` a pristine upstream-only mirror; use the repository Rust 1.94.0
  toolchain and existing package/build conventions.
- Integrate in the current clean `downstream/main` checkout. The original
  downstream commit remains reachable through merge history.
- Preserve existing configuration, credentials, sessions, and the previous
  executable. Do not restart Agent Deck or other live sessions.
- Build with source-path remapping and inspect the installed artifact for
  personal paths. Store temporary raw diagnostics privately outside the repository.
- Scope is local merge and installation; remote publication is not requested.

## Progress

- [x] Inspect branch, upstream, toolchain, installed aliases, and prior sync record.
- [x] Fetch upstream and preview the merge (11 conflicted files).
- [x] Resolve conflicts and inspect downstream integration hotspots.
- [x] Validate formatting, compilation, downstream regressions, and relevant suites.
- [ ] Commit integration and build the release from committed source.
- [ ] Validate release standard/minimal TUI and annotation persistence/follow-up.
- [ ] Preserve the old executable, install atomically, and verify aliases/version/hash.
- [ ] Update feature ledger and delivery record; leave a clean downstream checkout.

## Validation and risks

Required checks follow `docs/downstream/README.md`: formatting, pager-bin
compilation, pager and shell tests, and release PTY coverage. Also exercise
Responses stream fallback and the annotation read-only parser. Prior syncs
recorded macOS-specific test failures, hanging doctor probes, missing upstream
internal documentation, and a shell integration feature-gate mismatch; verify
their current status and distinguish pre-existing limitations from regressions.

| Check | Status |
| --- | --- |
| `cargo fmt --all -- --check` | Passed; subsequent Rust edits checked with rustfmt |
| `cargo check --locked -p xai-grok-pager-bin` | Passed after API/layout adaptations |
| Downstream diff whitespace check | Passed |
| Pager lib suite | 10289 passed, 0 failed, 4 ignored, 3 filtered |
| Sampler lib suite | 253 passed, 0 failed |
| Markdown lib suite | 498 passed, 0 failed |
| Pager annotation regressions | 57 passed |
| Shell annotation/fork/capability regressions | 13 passed |
| Read-only shell parser | 4 passed |
| Shell lib suite | 7052 passed, 11 failed, 5 ignored |
| Workspace lib suite | 2087 passed, 7 failed |
| Release build and PTY validation | Pending |

The public upstream tree still omits `docs/internal/25-enterprise.md` and
`docs/internal/22-environment-variables.md`, which are compile-time inputs to
the pager documentation integration target. Those paths are absent at the
exact upstream commit; no placeholder documentation is introduced.

The initial pager run reproduced the prior macOS hang in three `doctor_cmd`
tests: `fake_standalone_facts_compose_through_shared_view`,
`standalone_runtime_and_tmux_are_unavailable_without_false_wezterm_finding`,
and `standalone_wayland_missing_is_issue_but_no_seats_or_errors_are_not`.
After two minutes, only the verified pager libtest child was terminated.
The same compiled test binary then passed with those three exact tests skipped.
The doctor module is identical to the fetched upstream commit. The combined
Cargo test run uses `--no-fail-fast`, so the remaining suites continue normally.

Shell failures are in upstream MCP/project-trust configuration tests (five),
workflow registry tests (three), empty facet-filter normalization (one), consent
persistence (one), and symlink-path persistence (one). Serial reruns with
`TMPDIR=/private/tmp` pass the two canonical-path cases and consent persistence;
the other eight reproduce. The failing modules and their project-discovery,
configuration, and trust dependencies match upstream, except for the portable
builtin listing label described below, which does not affect these cases.

Workspace failures are in image-storage path spelling (one), Git conversation
branch creation (one), redirect-event mock/telemetry expectations (four), and a
Grove fallback-reason expectation (one). The image case passes with a canonical
temporary directory; the other six reproduce serially. The failing modules
match upstream. The downstream read-only parser tests all pass.

These full-suite limits remain visible; they are not represented as passing
checks. Actual release standard/minimal/annotation PTY checks are required
before replacing the installed executable.

## Integration details

The 11 conflicts are resolved. Annotation routing retains prompt attribution
and rejects annotation terminals before ordinary root/child finalization;
upstream's new ordinary-child completion path remains intact. Rendering keeps
annotation decorations while following upstream's removal of inline editing.
The read-only shell gate uses upstream's extracted permission-rules APIs.
Tool dispatch retains the annotation policy check and the new requested-model
argument. Durable turn completion combines upstream's cancellation metadata
helper with explicit prompt identity. UUID features include both downstream
serialization and upstream v7 generation. Four downstream tool-policy test
calls were adapted to the new dispatch signature.

Compilation also identified two mechanical integration adjustments: traverse
tree-sitter nodes through its iterator API after the upstream 0.26 upgrade,
and retain each scrollback entry's ID when measuring annotation decorations
under upstream's index-based layout loop.

The compiled-in workflow's listing label used `env!("CARGO_MANIFEST_DIR")`,
which embeds the build machine's directory even with compiler path remapping.
It now uses its repository-relative source path. Workflow execution still uses
the existing embedded script; the label is only listing metadata. The initial
release prebuild was stopped so the final build includes this privacy fix.

The staged upstream additions passed a scan for this machine's home/username,
private-key markers, and common access-token formats. The existing repository
author/committer identity matches prior downstream commits and uses a personal
public address, with no new attribution trailers.
The updated historical annotation plan now uses repository-relative or portable
paths in place of personal absolute paths. This cleans current files only;
published and local historical commits are not rewritten.

No unresolved user decisions. Next action: complete compilation and regression
validation, then build and install the committed release.

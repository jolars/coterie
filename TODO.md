# Coterie roadmap

This roadmap is ordered by dependency and recommended implementation sequence.
[`DESIGN.md`](DESIGN.md) is the architectural authority; this file divides that
design into acceptance-gated milestones. Check off a deliverable only when its
tests and the milestone gate pass.

The first public milestone is a deliberately narrow, single-project MVP. The
later milestones complete the broader initial product target in `DESIGN.md`.

## M0: Development foundation

The repository now contains the Rust package and project-specific checks that
later milestones build on.

- [x] Replace the generic `devenv.nix` template with a Rust environment modeled
  on `../basin`: enable the pinned toolchain from `rust-toolchain.toml`, and
  install Git, Go Task, SQLite, `cargo-nextest`, `cargo-llvm-cov`,
  `cargo-audit`, `cargo-deny`, `nixfmt`, `taplo`, and `actionlint`.
- [x] Scaffold one edition-2024 binary crate named `coterie`. Start at version
  `0.1.0`, keep the runtime in internal modules, and commit `Cargo.lock`.
- [x] Pin Rust 1.98.0 with rustfmt and Clippy. Do not declare an independent
  MSRV before release hardening establishes one from evidence.
- [x] Add a `Taskfile.yml` with `fmt`, `lint`, `test`, `docs`, `audit`, `check`,
  and `coverage` tasks. `check` must reproduce every required local and CI
  gate except coverage.
- [x] Enable pre-commit rustfmt, Clippy, TOML formatting, and Nix formatting
  through devenv. Hooks must call the same underlying commands as
  `task check`.
- [x] Add CI for formatting, Clippy with warnings denied, nextest, rustdoc with
  warnings denied, dependency policy, and Nix evaluation. Cache build
  artifacts without making the cache part of correctness.
- [x] Add the README, dual MIT/Apache-2.0 license files, `CHANGELOG.md`, package
  metadata, `deny.toml`, formatting configuration, and dependency-update
  policy.
- [x] Configure Versionary's Rust release-PR workflow with Conventional Commits,
  pre-major feature bumps, stable-major releases disabled, commit authors,
  and best-effort issue references. Release `v0.1.0` manually, then enable
  Versionary and trusted crates.io publishing for later releases.
- [x] Add an operational `AGENTS.md` that records project invariants, the
  test-first workflow, verification commands, and documentation
  synchronization rules.

### M0 gate

- [x] A fresh `devenv shell` can run `task check` successfully.
- [x] The pre-commit hooks pass on the scaffold without rewriting tracked files.
- [x] CI passes from a clean checkout, and Versionary configuration verifies
  without creating a tag or publishing a package.

## M1: Core contracts and durable state

- [x] Create the internal `cli`, `config`, `project`, `supervisor`, `protocol`,
  `providers`, `tasks`, `workspace`, `state`, and `transcript` modules. Keep
  one deployable binary; extract a crate only after a concrete boundary
  requires it.
- [x] Define stable, type-specific IDs using a short Coterie prefix and a ULID,
  including run, project, agent, session, task, assignment, message,
  operation, and event identities.
- [x] Define versioned JSON success envelopes, machine-readable error bodies,
  documented exit-code categories, and operation IDs for every mutating
  command. Standard output must never mix JSON and diagnostics.
- [x] Model the versioned `builtin:standard@1` archetype and its roles,
  capabilities, permission profiles, workspace policies, and limits as data.
  Defer external configuration layers without hardcoding role semantics.
- [x] Add append-only SQLite migrations and transactional repositories for runs,
  configuration snapshots, projects, agents, sessions, tasks, dependencies,
  task groups, comments, claims, assignments, messages, workspaces,
  operations, and events. Store provider transcripts outside the database.
- [x] Enforce foreign keys, bounded busy timeouts, single-writer access, WAL
  where supported, compare-and-set claims, and idempotent mutations.
- [x] Implement task readiness and the `open`, `in_progress`, `submitted`,
  `closed`, and `canceled` lifecycle. Dependencies are satisfied only by
  closed tasks; blocking remains derived state.

### M1 gate

- [x] Tests cover every task transition, dependency release, atomic claim,
  retry, authorization decision, migration, invalid ID, and corrupt-state
  error.
- [x] Golden tests lock the versioned JSON and exit-code contracts before other
  processes depend on them.

## M2: Supervised runtime with deterministic fakes

- [x] Discover the canonical Git project or non-Git directory, derive its
  identity, and place private runtime and durable data beneath the
  appropriate XDG directories.
- [x] Implement a nonblocking exclusive project lease, active-run index, Unix
  socket handshake, automatic singleton supervisor startup, and typed local
  RPC.
- [x] Authenticate agents with random, generation-scoped tokens whose stored
  representation is a verifier; keep the operator path distinct.
- [x] Implement a deterministic fake provider and use it to drive agent and
  session lifecycles without model access.
- [x] Implement the minimum delegation commands: foreground launch, `status`,
  `whoami`, `prime`, `task create`, `task ready`, `task close`, `spawn`,
  `finish`, `send`, `inbox`, `logs`, `events`, and `stop`.
- [x] Persist messages before delivery, use monotonic inbox cursors and explicit
  acknowledgements, and normalize every state transition into the event
  stream.
- [x] Record durable intent before launching a process or changing a workspace;
  reconciliation must distinguish desired, observed, lost, and unknown
  state.

### M2 gate

- [x] An integration test launches a fake lead and worker, delegates and closes
  a task, disconnects the foreground, and reconnects to the same run.
- [x] Restart tests preserve the task graph, transcript references, operations,
  and workspace metadata. An unverifiable live process is classified as
  `lost` or `unknown`, never silently adopted or declared successful.

## M3: Codex and Git vertical slice---v0.1.0 MVP

- [x] Probe the installed Codex version and required capabilities before launch;
  reject incompatible versions with an actionable diagnostic.
- [x] Launch the foreground Codex TUI with inherited terminal streams, working
  directory, resize behavior, and signal forwarding. Inject only Coterie's
  orchestration bootstrap through Codex's documented
  `developer_instructions` setting, leaving repository `AGENTS.md` discovery
  intact.
- [x] Launch workers through `codex exec --json`, parse its JSONL event stream,
  classify exits and malformed frames, store append-only transcripts, and
  pass only the identity-scoped environment.
- [x] Map the built-in permission profiles to enforceable Codex flags. Fail
  closed when a requested filesystem, network, approval, bootstrap, or
  working directory capability cannot be enforced.
- [x] Implement the workspace trait with `git2`: record a base commit, create a
  task-owned worktree and reference below Coterie's state directory, and
  record the resulting commit without invoking the Git CLI.
- [x] Implement explicit guarded integration. Refuse dirty targets, unexpected
  tips, ambiguous histories, and conflicts; never remove dirty,
  unintegrated, running, or ambiguously owned work. Integration defaults to
  rebase, preserving contribution commits in linear history, with explicit
  `--strategy merge` support in the CLI and agent MCP tool. Migration 16
  preserves historical merge plans. Real-repository tests cover authors,
  messages, empty commits, intermediate conflicts, and preserved
  submissions; crash matrices cover both strategies and repeated recovery.
  The multi-worker workflow verifies linear integration through accepted
  task closure. `task check` passes with 478 tests and 17 opt-in or helper
  tests skipped.
- [x] Complete the operator loop for task creation, worker spawn, logs and
  messages, assignment submission, integration, validation, task closure,
  and safe stop.
- [x] Document installation, Codex prerequisites, the Sidekick custom-command
  entry, the trust model, recovery behavior, and every MVP command and exit
  code.
- [x] Enable crates.io publication with trusted publishing and attach
  checksummed, provenance-attested `x86_64` and `aarch64` binaries for glibc
  and musl Linux to the Versionary-created GitHub release.

### v0.1.0 MVP gate

- [x] From a clean Git repository, `coterie` starts or reconnects to its
  supervisor and opens the foreground Codex lead without unsolicited wrapper
  output corrupting the TUI.
- [x] The lead can create one task, spawn one Codex worker in an isolated
  worktree, receive its durable result, inspect its transcript, integrate it
  explicitly, validate it, and close the task.
- [x] Closing the foreground leaves the run and active worker intact. A later
  invocation reconstructs orchestration context even when transparent Codex
  session reattachment is unavailable.
- [x] Interrupting the foreground reaches Codex but does not stop the run;
  `coterie stop` performs bounded shutdown and preserves recoverable work.
- [x] Unit, fake-provider, temporary-repository, and opt-in real-Codex contract
  tests pass. CI never requires networked or account-authenticated Codex
  runs.
- [ ] Merge the first Versionary release PR only after every MVP criterion is
  satisfied; publish that release as `v0.2.0`.

The MVP intentionally excludes attached projects, cross-project dependencies,
external configuration layers, provenance and lock files, live steering,
transparent provider reattachment, and exhaustive crash-boundary coverage.

## M4: Reliability and safety hardening

- [x] Reconcile every durable operation and owned resource idempotently after
  crashes between intent, external side effect, and observed-result
  recording.
- [x] Fence sessions, assignments, workspaces, and late provider output by run
  and generation; adopt a live process only when it proves both.
- [x] Add bounded restart windows, crash-loop quarantine, timeout handling, and
  the full phased shutdown protocol.
- [x] Complete `doctor`, conservative stale-state repair, resumable event
  following, transcript-tail handling, credential redaction, and private
  runtime permission checks.
- [x] Inject failures at every database/process/filesystem boundary and verify
  convergence to a recoverable state without duplicated side effects. See
  the [crash matrix](docs/crash-matrix.md) for boundaries and recovery
  evidence.

### M4 gate

- [x] The crash matrix, restart tests, cleanup safety tests, and fake-provider
  conformance suite pass repeatedly under concurrency.
- [x] No destructive path runs without positive proof of ownership, inactivity,
  and recoverability. See the [safety audit](docs/destructive-operations.md)
  for the operation inventory, guards, and regression evidence.

## M5: Full declarative configuration

- [x] Implement the internal loader and resolver for versioned compiled
  defaults, trusted global configuration and local includes, a selected
  built-in or global archetype, safe project restrictions, and bounded
  operator overrides in the specified precedence order.
- [x] Track provenance for every effective value; reject unknown fields,
  unsupported schemas, recursive includes, cycles, shadowed `builtin:`
  names, and references to missing trusted definitions.
- [x] Implement monotone policy intersection so project data can disable roles
  or reduce authority and capacity but cannot introduce commands,
  instructions, hooks, paths, environment variables, capabilities, or
  permission profiles.
- [x] Implement `config check`, effective configuration with provenance, schema
  generation, explicit lock creation, and lock verification without secrets
  or host-specific values.
- [x] Wire resolved configuration into launches and recovery, snapshot the
  effective run configuration, and reject incompatible changes rather than
  hot-applying them. The [runtime tests](tests/supervisor_runtime.rs) cover
  configured launches, authority, limits, overrides, and recovery. Migration
  11 pins historical policy for existing runs; upgrade and crash tests
  verify durable snapshots. `task check` passes.

### M5 gate

- [x] Lattice and property tests prove that an untrusted project override can
  never increase authority or a resource ceiling. See the [policy
  tests](src/config/policy_tests.rs) for intersection laws, combined
  restrictions, trusted selection, operator bounds, and injection rejection.
- [x] Golden tests cover schemas, provenance, configuration fingerprints, lock
  portability, includes, and actionable mismatch diagnostics. See the [lock
  tests](src/config/lock/tests.rs) and [configuration CLI
  tests](tests/config_cli.rs), alongside the loader and provenance golden
  tests. `task check` passes.

## M6: Cross-project orchestration

- [x] Attach canonical project roots under unique aliases, enforce global root
  policy and per-project leases, and discover the same active run from every
  attached project. The [runtime tests](tests/supervisor_runtime.rs) cover
  authorization, aliases, symlinks, linked worktrees, discovery, and lease
  races. The [crash tests](src/supervisor/crash_tests.rs) cover attachment
  and retirement; migration 12 pins historical root policy. `task check`
  passes.
- [ ] Apply and snapshot each attached project's restrictions and lock without
  allowing its archetype selector to replace the run archetype.
- [ ] Give every task exactly one writable target and explicit read-only input
  projects. Resolve every assignment workspace from the task's project
  identity.
- [ ] Support cross-project dependencies and materialize an accepted upstream
  Git tree at its recorded integration commit as a non-writable input
  snapshot.
- [ ] Make attachment and workspace operations durable, idempotent, and
  deadlock-free; interrupted or conflicting attachment must remain visible.

### M6 gate

- [ ] In two temporary repositories, a library worker runs in the library
  worktree, a bindings worker runs in the bindings worktree, and the
  bindings task remains blocked until the library task is integrated,
  verified, and closed.
- [ ] Attachment races, alias collisions, symlinks, linked Git worktrees,
  incompatible restrictions, dirty targets, and cross-run lease conflicts
  have deterministic tests and diagnostics.

## M7: Complete the initial product target

- [ ] Audit every item under `DESIGN.md`'s initial product target and every
  stated safety invariant; add any missing command, state transition, or
  test.
- [ ] Run shared provider conformance tests against the fake and Codex adapters,
  and shared workspace tests against temporary real repositories.
- [ ] Finish CLI help, JSON schema and exit-code documentation, configuration
  references, recovery guidance, threat-model documentation, shell
  completions, and a Sidekick smoke test.
- [ ] Validate installation from crates.io and the Linux release artifact in a
  clean environment, including database and configuration upgrades from
  every published `0.x` release.
- [ ] Define an evidence-based MSRV and supported Linux baseline. Keep releases
  in `0.x` until a separate compatibility review establishes the `1.0`
  contract.

### Research workflow follow-ups

These items come from the September 15, 2026
[normreg-multi field report](docs/field-report-normreg-multi.md). Address the
high-priority items first. The observations concern one run; pin the Coterie
build, provider version, and effective policy in reproductions before attributing
a cause. Specify new interfaces or authority in `DESIGN.md` before implementation.
Scientific relevance and task scope remain agent responsibilities.

- [x] **High: Make the linked-worktree commit path usable under the selected
  policy.** Reproduce edit, validate, stage, commit, and submit with actual
  provider-launched workers in a real linked worktree, including a recovery
  continuation. Diagnose an unsupported commit path before work begins, and
  provide scoped commit support or an explicit coordinator-commit handoff.
  Test that the primary checkout, sibling worktrees, shared references, and
  repository configuration remain protected; broad write access to the Git
  common directory is not an acceptable fix. Keep real-provider tests opt-in.
  The explicit coordinator handoff passed with Codex 0.153.4 on NixOS, including
  normal submission and recovery through validated closure. See the
  [reproduction and acceptance evidence](docs/linked-worktree-commits.md).
- [x] **High: Diagnose validation environment access separately from Git
  permissions.** Reproduce Nix daemon access and `.devenv` write failures in
  assigned workspaces, recording the command and selected policy. Report which
  validation steps succeed or are blocked and document supported environment
  entry without automatically widening permissions. Add opt-in NixOS regression
  coverage for reproduced failures. The assigned-worktree regression passed on
  NixOS with Codex 0.153.4, with independent daemon and external `.devenv` state
  denials and successful validation using a prepared interpreter. See the
  [environment guide and recorded evidence](docs/validation-environments.md).
- [x] **High: Bound repeated context inspection.** Provide compact task and
  assignment summaries in `prime`, with stable references for fetching full
  descriptions and reports. Keep current task context, recovery provenance,
  and actionable blockers visible after transitions and reconnects. Measure
  serialized response sizes on a fixture with long reports and several
  completed tasks, and enforce documented size bounds. Include repeated
  bootstrap and serialized context in transcript-inspection measurements;
  provide a concise route to current activity while retaining full transcripts.
  Test human and JSON views and full-detail retrieval. The bounded `progress`
  feed remains a lifecycle feed, not a substitute for current task context.
  Compact context, revision-checked full details, and transcript tails passed
  the fixture and `task check`. See the [bounds and measurements](docs/context-inspection.md).
- [x] **Medium: Make recovery handoffs self-contained.** Expose the preserved
  path and base commit, dirty, staged, and untracked paths, prior validation
  evidence, and unfinished steps with references to their sources. Distinguish
  agent-reported checks and next steps from recorded mechanical state. Test a
  worker exit with unsubmitted staged artifacts, recovery into a fresh
  worktree, and continuation through integration, validation, and task closure.
  Verify that the source files and index survive unchanged and that the
  continuation receives no writable ownership of the preserved workspace.
  Recovery snapshots and sourced reports passed staged-artifact continuation,
  schema upgrades, crash tests, and `task check`. See the
  [handoff guide](docs/recovery-handoffs.md).
- [x] **High: Deliver foreground wake-up notifications through Codex queue.**
  Capability-probed delivery binds provider metadata to the authenticated
  foreground generation and verifies the host bridge's process provenance.
  Durable attempts coalesce updates without promoting worker content, changing
  user authority, acknowledging inbox messages, or accepting tasks. Tests cover
  busy and ended turns, stale bindings, shutdown, reconnects, command failure,
  duplicates, and crash recovery. `task check` passed all 509 tests and gates;
  the opt-in real foreground test passed on Codex 0.153.4. See the
  [delivery contract and evidence](docs/codex-queue.md).
- [ ] **Medium: Reduce client-side protocol bookkeeping.** Add client support
  for separate progress and inbox cursors, page draining, acknowledgement of
  handled messages, and mutation retries. Progress must never acknowledge
  messages; uncertain retries must retain the original operation ID and
  identical arguments. Test empty pages with `has_more`, reconnects, partial
  message handling, and uncertain mutation outcomes before shortening
  agent-facing instructions. Preserve explicit task acceptance and generation
  checks.
- [ ] **Medium: Document and test MCP bridge rediscovery after reconnect.**
  Exercise a stale tool identifier followed by discovery of the current bridge
  and `prime`, verifying the same run and agent identity with current session
  authentication. Document the recovery steps and test that stale credentials
  remain rejected. Distinguish restored tool access from automatic foreground
  wake-up, which requires separate provider capability evidence.

### Commit-handoff validation follow-ups

These observations come from the September 15, 2026
[linked-worktree reproduction](docs/linked-worktree-commits.md#recorded-reproduction).

- [ ] **High: Verify Git commit publication after write failures.** Isolate
  libgit2 1.9.7 returning a commit ID after an object-write failure while a fresh
  repository handle observes the original HEAD. Add a regression that checks
  object readability and reference advancement, audit Coterie's Git write paths
  before recording observed success, and verify recovery and idempotent retries
  after failed writes. Track the upstream error-propagation fix and evaluate a
  dependency update without treating a returned ID as proof of publication.
- [ ] **Medium: Diagnose missing Codex code-tool transcript entries.** Reproduce
  shell commands invoked through Codex's code tool being absent from
  `exec --json` command-execution events on 0.153.4. Compare emitted events with
  independent command-result artifacts and direct shell invocations, pinning
  the provider version and policy. Preserve available events and expose any
  observability limitation explicitly; do not infer that an omitted command
  never ran or rely on undocumented provider session-file formats. Test human
  and JSON log views, credential redaction, and any supported adapter change.
  Keep real-provider coverage opt-in.

### Diplodocus workflow follow-ups

These observations come from the September 15, 2026 Diplodocus Milestone 3 run
`cr-01M2JD7PN7SYP6ZCRJ5EQA6ZNY`: three implementation assignments and an
independent review reached validated closure. Pin the Coterie build, provider
version, and effective policy when reproducing these observations; they do not
establish behavior across other builds or policies.

- [ ] **Medium: Make review-only task acceptance explicit.** A clean review
  submitted its unchanged base commit, but `task close` required a no-op
  `workspace integrate` first. Specify and test the supported path from an
  unchanged submission through validation and accepted closure. Document the
  no-op integration requirement, or define an explicit no-change result
  contract in `DESIGN.md` before changing closure rules. Preserve explicit
  acceptance, generation checks, and dependency release only after closure.
  Cover a target that advances during review and verify that accepting the
  review cannot move its HEAD or overwrite work.
- [ ] **Medium: Diagnose build-artifact disk pressure across worktrees.**
  Separate Rust build caches exhausted available disk space during parallel
  work. Measure per-assignment artifact usage, expose actionable low-space
  diagnostics, and define scoped cleanup of explicitly selected disposable
  artifacts from completed, inactive assignments. Specify cleanup ownership
  and authority in `DESIGN.md` before implementation. Preserve source changes,
  indexes, submissions, and artifacts still used by running processes. Test
  low-space failures, symlink boundaries, concurrent use, and interrupted or
  repeated cleanup without deleting unrelated files or caches.
- [ ] **Medium: Extend validation coverage to declared environment inputs.**
  The lead could discover the configured Python/R kernels, while the reviewer
  lacked `JUPYTER_PATH`. Extend the existing
  [validation-environment regression](docs/validation-environments.md) with a
  check that needs an additional declared environment input. Distinguish
  intentional provider filtering, missing prerequisites, and sandbox denials;
  verify the documented workspace environment-entry or coordinator-validation
  handoff with exact commit and command evidence. Do not infer full environment
  inheritance from a preserved `PATH`, pass the entire ambient environment, or
  widen permissions automatically. Keep real-provider coverage opt-in and
  retain the completed Nix daemon and `.devenv` access coverage.

### M7 gate

- [ ] The full design criterion succeeds end to end, including the two-project
  workflow, crash recovery, conservative cleanup, and configuration
  inspection.
- [ ] Every non-goal remains absent or explicitly proposed as a later design
  change rather than entering the implementation accidentally.

## Future work to scope

- [ ] Scope an optional transfer helper for the
  [research recovery handoff](docs/field-report-normreg-multi.md#3-recovery-preserved-work-but-required-a-manual-handoff).
  Require an explicit selection of changes, identify conflicts before applying
  them to the continuation's fresh workspace, and preserve the source files,
  index, and ownership. Define the behavior in `DESIGN.md` before implementation,
  with tests for conflicting changes, partial selection, and interrupted or
  repeated transfers.
- [ ] Design exclusive resource reservations for performance measurements: allow
  parallel implementation while serializing benchmark windows across Coterie
  runs on the same machine. Agents request and release reservations; Rust
  enforces admission after competing workers acknowledge safe stopping
  points and their builds, tests, and other competing subprocesses have
  finished. Prevent competing work from starting until release, and make
  reservation ownership, timeouts, and crash recovery durable and
  inspectable. Define enforcement and subprocess tracking in `DESIGN.md`
  before implementation, with tests for concurrent requests, interrupted
  acquisition, and recovery. Scope the guarantee to Coterie-managed
  workloads; unrelated host processes require separate handling.

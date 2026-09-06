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
- [ ] Model the versioned `builtin:standard@1` archetype and its roles,
  capabilities, permission profiles, workspace policies, and limits as data.
  Defer external configuration layers without hardcoding role semantics.
- [ ] Add append-only SQLite migrations and transactional repositories for runs,
  configuration snapshots, projects, agents, sessions, tasks, dependencies,
  task groups, comments, claims, assignments, messages, workspaces,
  operations, and events. Store provider transcripts outside the database.
- [ ] Enforce foreign keys, bounded busy timeouts, single-writer access, WAL
  where supported, compare-and-set claims, and idempotent mutations.
- [ ] Implement task readiness and the `open`, `in_progress`, `submitted`,
  `closed`, and `canceled` lifecycle. Dependencies are satisfied only by
  closed tasks; blocking remains derived state.

### M1 gate

- [ ] Tests cover every task transition, dependency release, atomic claim,
  retry, authorization decision, migration, invalid ID, and corrupt-state
  error.
- [ ] Golden tests lock the versioned JSON and exit-code contracts before other
  processes depend on them.

## M2: Supervised runtime with deterministic fakes

- [ ] Discover the canonical Git project or non-Git directory, derive its
  identity, and place private runtime and durable data beneath the
  appropriate XDG directories.
- [ ] Implement a nonblocking exclusive project lease, active-run index, Unix
  socket handshake, automatic singleton supervisor startup, and typed local
  RPC.
- [ ] Authenticate agents with random, generation-scoped tokens whose stored
  representation is a verifier; keep the operator path distinct.
- [ ] Implement a deterministic fake provider and use it to drive agent and
  session lifecycles without model access.
- [ ] Implement the minimum delegation commands: foreground launch, `status`,
  `whoami`, `prime`, `task create`, `task ready`, `task close`, `spawn`,
  `finish`, `send`, `inbox`, `logs`, `events`, and `stop`.
- [ ] Persist messages before delivery, use monotonic inbox cursors and explicit
  acknowledgements, and normalize every state transition into the event
  stream.
- [ ] Record durable intent before launching a process or changing a workspace;
  reconciliation must distinguish desired, observed, lost, and unknown
  state.

### M2 gate

- [ ] An integration test launches a fake lead and worker, delegates and closes
  a task, disconnects the foreground, and reconnects to the same run.
- [ ] Restart tests preserve the task graph, transcript references, operations,
  and workspace metadata. An unverifiable live process is classified as
  `lost` or `unknown`, never silently adopted or declared successful.

## M3: Codex and Git vertical slice---v0.1.0 MVP

- [ ] Probe the installed Codex version and required capabilities before launch;
  reject incompatible versions with an actionable diagnostic.
- [ ] Launch the foreground Codex TUI with inherited terminal streams, working
  directory, resize behavior, and signal forwarding. Inject only Coterie's
  orchestration bootstrap through Codex's documented
  `developer_instructions` setting, leaving repository `AGENTS.md` discovery
  intact.
- [ ] Launch workers through `codex exec --json`, parse its JSONL event stream,
  classify exits and malformed frames, store append-only transcripts, and
  pass only the identity-scoped environment.
- [ ] Map the built-in permission profiles to enforceable Codex flags. Fail
  closed when a requested filesystem, network, approval, bootstrap, or
  working directory capability cannot be enforced.
- [ ] Implement the workspace trait with `git2`: record a base commit, create a
  task-owned worktree and reference below Coterie's state directory, and
  record the resulting commit without invoking the Git CLI.
- [ ] Implement explicit guarded integration. Refuse dirty targets, unexpected
  tips, ambiguous histories, and conflicts; never remove dirty,
  unintegrated, running, or ambiguously owned work.
- [ ] Complete the operator loop for task creation, worker spawn, logs and
  messages, assignment submission, integration, validation, task closure,
  and safe stop.
- [ ] Document installation, Codex prerequisites, the Sidekick custom-command
  entry, the trust model, recovery behavior, and every MVP command and exit
  code.
- [ ] Enable crates.io publication with trusted publishing and attach a
  checksummed `x86_64-unknown-linux-gnu` binary to the Versionary-created
  GitHub release.

### v0.1.0 MVP gate

- [ ] From a clean Git repository, `coterie` starts or reconnects to its
  supervisor and opens the foreground Codex lead without unsolicited wrapper
  output corrupting the TUI.
- [ ] The lead can create one task, spawn one Codex worker in an isolated
  worktree, receive its durable result, inspect its transcript, integrate it
  explicitly, validate it, and close the task.
- [ ] Closing the foreground leaves the run and active worker intact. A later
  invocation reconstructs orchestration context even when transparent Codex
  session reattachment is unavailable.
- [ ] Interrupting the foreground reaches Codex but does not stop the run;
  `coterie stop` performs bounded shutdown and preserves recoverable work.
- [ ] Unit, fake-provider, temporary-repository, and opt-in real-Codex contract
  tests pass. CI never requires networked or account-authenticated Codex
  runs.
- [ ] Merge the first Versionary release PR only after every MVP criterion is
  satisfied; publish that release as `v0.1.0`.

The MVP intentionally excludes attached projects, cross-project dependencies,
external configuration layers, provenance and lock files, live steering,
transparent provider reattachment, and exhaustive crash-boundary coverage.

## M4: Reliability and safety hardening

- [ ] Reconcile every durable operation and owned resource idempotently after
  crashes between intent, external side effect, and observed-result
  recording.
- [ ] Fence sessions, assignments, workspaces, and late provider output by run
  and generation; adopt a live process only when it proves both.
- [ ] Add bounded restart windows, crash-loop quarantine, timeout handling, and
  the full phased shutdown protocol.
- [ ] Complete `doctor`, conservative stale-state repair, resumable event
  following, transcript-tail handling, credential redaction, and private
  runtime permission checks.
- [ ] Inject failures at every database/process/filesystem boundary and verify
  convergence to a recoverable state without duplicated side effects.

### M4 gate

- [ ] The crash matrix, restart tests, cleanup safety tests, and fake-provider
  conformance suite pass repeatedly under concurrency.
- [ ] No destructive path runs without positive proof of ownership, inactivity,
  and recoverability.

## M5: Full declarative configuration

- [ ] Load versioned compiled defaults, trusted global configuration and local
  includes, a selected built-in or global archetype, safe project
  restrictions, and bounded operator overrides in the specified precedence
  order.
- [ ] Track provenance for every effective value; reject unknown fields,
  unsupported schemas, recursive includes, cycles, shadowed `builtin:`
  names, and references to missing trusted definitions.
- [ ] Implement monotone policy intersection so project data can disable roles
  or reduce authority and capacity but cannot introduce commands,
  instructions, hooks, paths, environment variables, capabilities, or
  permission profiles.
- [ ] Implement `config check`, effective configuration with provenance, schema
  generation, explicit lock creation, and lock verification without secrets
  or host-specific values.
- [ ] Snapshot the effective run configuration and reject incompatible changes
  rather than hot-applying them.

### M5 gate

- [ ] Lattice and property tests prove that an untrusted project override can
  never increase authority or a resource ceiling.
- [ ] Golden tests cover schemas, provenance, configuration fingerprints, lock
  portability, includes, and actionable mismatch diagnostics.

## M6: Cross-project orchestration

- [ ] Attach canonical project roots under unique aliases, enforce global root
  policy and per-project leases, and discover the same active run from every
  attached project.
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

### M7 gate

- [ ] The full design criterion succeeds end to end, including the two-project
  workflow, crash recovery, conservative cleanup, and configuration
  inspection.
- [ ] Every non-goal remains absent or explicitly proposed as a later design
  change rather than entering the implementation accidentally.

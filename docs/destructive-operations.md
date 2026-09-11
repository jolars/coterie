# Destructive-operation safety audit

The M4 safety gate covers every implemented operation that can remove a
filesystem name, replace existing contents, advance a Git reference, or stop a
process. The inventory below identifies its authority and preservation checks.
The [crash matrix](crash-matrix.md) separately verifies recovery across the
intent, effect, and observation boundaries.

## Coordination files

The guards live in [project.rs](../src/project.rs),
[private_fs.rs](../src/private_fs.rs), and [supervisor.rs](../src/supervisor.rs).

| Operation | Ownership and inactivity proof | Recoverability and refusal evidence |
| --- | --- | --- |
| `ProjectLease::try_acquire`: truncate the lease owner hint | Open a private, single-link regular file without following symlinks; acquire its exclusive kernel lock; verify the pathname still names that inode. | The hint is disposable. The lock, active-run index, and run database establish authority. A competing holder prevents truncation. `project_leases_are_nonblocking_and_identity_scoped` and the private-file tests cover contention and unsafe file types. |
| `ActiveRunIndex::publish`: replace a project index entry | Require the lease for that project and run, verify that its open inode still occupies the private lease path, and require any existing entry to match the complete published identity. Create a new private temporary file, sync it, and recheck the source and destination inodes before rename. | Replacement contains the same run identity, whose durable database has already been initialized. Conflicting, malformed, linked, or public entries remain intact. Failed publication preserves temporary files. `active_run_index_is_identity_checked_and_retired_only_by_its_owner`, `index_mutation_requires_the_original_held_lease`, and `index_mutation_preserves_unverifiable_entries` cover refusal and successful retirement. |
| `ActiveRunIndex::retire`: unlink a project index entry | Require the matching, still-held lease and a private, identity-checked entry for that run. Verify the inspected open inode immediately before unlinking. The supervisor invokes retirement after durable stopped state and socket retirement. | An active or unknown session keeps shutdown incomplete. The database, events, transcripts, tasks, workspaces, and refs remain. A foreign run entry is preserved. `timed_out_shutdown_keeps_launches_blocked_and_replays_completion` and `completed_shutdown_retires_stale_coordination_before_a_new_run` cover incomplete and completed shutdown. |
| `remove_stale_socket`: unlink a stale socket | Startup holds the project lease and verifies the run database. Pin a current-user, mode-0600, single-link socket inode, require a private parent and a refused connection, and recheck the inode after the probe. | Responsive, inaccessible, timed-out, replaced, or insecure paths are preserved. A missing socket needs no removal. `stale_socket_repair_preserves_responsive_or_insecure_paths` and the runtime crash cases verify these outcomes. |
| `remove_owned_socket`: unlink this supervisor's socket | Retain an open handle to the filesystem inode recorded at binding. After the listener closes, require that same inode, its private mode, one link, and a refused connection. | A replacement file, symlink, hard link, socket, or insecure parent is preserved. Failed removal also preserves the index for recovery. `owned_socket_retirement_preserves_unverified_paths` and `owned_socket_retirement_requires_listener_inactivity` exercise those cases. |

Attached projects use these same guards. Secondary indexes retire before the
primary index so interrupted shutdown retains its recovery entrypoint. Recovery
of a stopped run preserves projects already leased and indexed by a newer run.
The two-project runtime crash matrix covers publication and retirement.
`stop_from_an_attached_project_waits_for_primary_retirement` verifies that
stopping from a secondary project waits for the primary index to retire.

Lease files are never unlinked. Removing a locked file would allow another
supervisor to acquire a different inode at the same pathname. Coterie releases
the lock by dropping its own file handle.

## Git operations and recoverable work

[`GitWorkspace`](../src/workspace.rs) is the only production Git mutation
boundary. Worktree creation
uses the recorded base, run, assignment, and project. It refuses conflicting
refs, existing unowned paths, changed administrative identity, and symlinked
workspace components. Owned refs are created without force. Reconciliation
preserves uncertain resources and never recreates a vanished workspace that was
previously observed.

Guarded integration is an explicit, capability-authorized write to an active
project. It requires current run and generation ownership, a submitted result,
the recorded worker tip and base, clean and inspectable worktrees, a matching
Git working directory, unambiguous history, and a conflict-free merge. Safe
checkout disables overwriting ignored files. Assume-unchanged and skip-worktree
flags block integration because status cannot establish cleanliness. Reference
advancement compares the original tip and retains both original histories.
Interrupted checkout is completed only when the index and worktree match the
planned result exactly; uncertain partial changes remain for inspection.

The `guarded_integration_*` real-repository tests cover dirty targets, changed
tips, conflicts, ambiguous histories, ignored files, hidden edits, and redirected
working directories. `workspace_observation_refuses_a_relocated_state_parent`
checks a moved workspace parent replaced by a symlink.
`workspace_side_effects_and_observations_require_current_ownership` and
`integration_plans_cannot_cross_runs_or_generations` cover stale ownership.
The integration crash matrices check preservation of worker commits, target
history, repository instructions, and partial checkout state.

There is no production worktree deletion, pruning, owned-ref deletion, Git
reset, or recursive directory removal. Shutdown only observes workspaces.
`shutdown_reconciliation_never_materializes_missing_work` verifies that it
does not create missing work. `operator_commands_drive_the_minimum_delegation_flow`
in [the runtime suite](../tests/supervisor_runtime.rs) verifies that stop
preserves a dirty, unintegrated worktree and its owned ref. `doctor` uses
read-only inspection and preserves ambiguous workspaces and transcripts.

Any future workspace-removal operation must implement all six cleanup proofs
in `DESIGN.md`: containment, durable run and generation ownership, Git
administrative identity, provider inactivity, clean and reachable or preserved
commits, and integration or explicit operator approval. An observed workspace
record alone is insufficient.

An operator closure override observes and records acceptance of work integrated
outside Coterie. It performs no Git writes, does not populate workspace
integration metadata, and grants no cleanup authority. The closure override
runtime tests verify that later dirty files and all owned worktree references
survive acceptance and shutdown.

## Processes and durable storage

The process guards live in [providers.rs](../src/providers.rs) and
[supervisor/session.rs](../src/supervisor/session.rs). Storage checks live in
[state.rs](../src/state.rs) and [transcript.rs](../src/transcript.rs).

Process interruption and termination intentionally act on active processes.
The required authority is an owned child handle bound to the current session
scope, with durable control intent recorded before delivery. Job control checks
the child has not been reaped before signaling it. The foreground wrapper owns
and waits for its child; the supervisor never substitutes a stored PID for that
handle. Capability probes, failed launch cleanup, and malformed-output
quarantine also control only the child they created. Control phases and
synchronous job or probe reaping have deadlines. The foreground wrapper retains
its child until an exit is observed; the supervisor's shutdown observation
deadline still bounds the stop request.

Unknown recovered processes receive no signals. Shutdown waits for terminal
observations before retiring run discovery. Timeouts preserve the active run
with launches blocked. Tasks, assignments, workspaces, refs, and transcript
prefixes survive interruption. The provider conformance tests,
`recovery_requires_the_exact_provider_run_and_generation`,
`session_timeouts_are_bounded_and_unknown_processes_are_never_signaled`, and
`shutdown_controls_escalate_once_and_survive_recovery` verify these guards.

Database writes require a private, single-link file and validated adjacent
SQLite files. The leased supervisor is the only writer. Forward migrations and
transactions preserve durable history; Coterie has no runtime database deletion
or history-purge operation. SQLite manages its own journals under that ownership.
Transcripts append at the end and have no truncation path. Permission changes
secure owned runtime resources without deleting content. The private-file,
migration-upgrade, transcript-tail, and crash tests cover these boundaries.

Validation uses Linux `O_PATH` handles for existing database and journal files.
Closing an ordinary file descriptor can release SQLite's process-wide POSIX
locks, even when its connection remains open; see SQLite's
[locking guidance](https://www.sqlite.org/howtocorrupt.html#posix_advisory_locks_canceled_by_a_separate_thread_doing_close_).
Database creation uses an exclusive create and closes that descriptor before
opening SQLite. Startup, recovery, and doctor inspection preserve SQLite's
locks while retaining ownership, permission, type, and link checks. The
`database_creation_guard_preserves_live_sqlite_locks` and
`database_validation_preserves_live_sqlite_locks` tests query actual kernel
locks on SQLite connections, including WAL shared memory and rollback mode.

## Verification and scope

Run `task check`, followed by repeated concurrent execution of the complete
suite when changing these guards:

```console
cargo nextest run --workspace --all-features --test-threads 4 --stress-count 3 --retries 0
```

This audit applies to the implemented Linux runtime and its documented
same-user trust model. Inode checks detect replacement and refuse ambiguous
ownership; they do not create a hostile same-UID isolation boundary. The ordinary
provider sandbox remains required. Provider-managed files and internal Git or
SQLite syscalls are governed by their respective adapters and libraries.

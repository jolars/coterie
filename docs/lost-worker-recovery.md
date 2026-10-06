# Lost-worker recovery incident

The October 6, 2026 Metewand run
`cr-01M48CRWHZ7XCA5PBF292AWRE7` left assignment
`ca-01M48FVDTZ3N5PGHWR5ZVDPX8B` active after sandbox commands failed with
`No space left on device`. Its source worktree retained staged, unstaged, and
untracked files. The provider transcript ended with an incomplete frame.

The retained database identifies session `cs-01M48FVDTZPKX9EHYFM3NF5S8Q`
as a supervisor-owned Codex job with provider identity `process:233126`.
Events 155 and 156 recorded launch and running state. Events 172–174 recorded
the session and agent becoming `lost` during reconciliation. No provider
process-exit event exists for that generation. The session credential was
revoked. At inspection, `/proc/233126` was absent. These records show that
the process was absent by reconciliation, but they do not recover its exit
status or establish why the supervisor lost the observation. The earlier disk
failure may have contributed; the available records cannot prove that link.

The explicit operator path in [the design](../DESIGN.md#embedded-task-and-state-store)
addresses this evidence gap. `task recover --acknowledge-lost` requires a
fresh adapter proof of process absence, preserves the source worktree and
index, and records that the exit remains unknown. A live or uncertain process
still blocks recovery. The continuation works in a new worktree and must pass
ordinary validation, integration, and accepted closure. The regression test
[`lost_worker_after_supervisor_restart_continues_through_accepted_closure`](../tests/supervisor_runtime/recovery.rs)
exercises this route with an incomplete transcript and dirty source.

The running 0.2.0 supervisor for the incident run does not have this operator
path. Applying the fix to that retained run requires starting a supervisor
with the new implementation after the existing one has stopped; the fix does
not mutate the incident run during development.

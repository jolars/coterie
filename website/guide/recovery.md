# Recovery

Coterie preserves durable tasks, messages, transcripts, and workspaces when the foreground exits, a worker fails, or a run stops. Recovery starts from the recorded state; it does not assume that an uncertain process has exited or that uncommitted work can be discarded.

## Return to an active run

Run `coterie` again from the primary project. Coterie reconnects to an active supervisor or conservatively restarts an indexed run after a crash. The fresh foreground Codex session gets its current context from durable state. It does not reattach the old TUI session.

Inspect the run when its state is unclear:

```console
coterie status
coterie doctor
coterie events
```

## Continue a stopped run

Stopping a run retains its work. Select the original run before launching a new foreground session:

```console
coterie run list
coterie run recover <run-id> --reason "Continue retained work."
coterie
```

Recovery verifies saved configuration, project identity and leases, process exits, and workspace ownership. If a replacement run exists, stop it first. Changed policy must be restored. Retrying an uncertain recovery uses the same operation ID and identical arguments; see [JSON and retry rules](/reference/protocol).

## Submit a retained commit

If the worker exited after a commit handoff, inspect the original assignment and independently review its exact commit. An operator or coordinator with `task:submit-retained` can submit it without copying changes or launching another worker:

```console
coterie task submit-retained --assignment <assignment-id> --result <full-commit-id> --summary "Validation commands, outcomes, and blocked checks." --reason "Worker exited before finish." --review "Independent review of the exact commit." --review-source "Review message or artifact."
```

Coterie requires a verified process exit, current assignment ownership, and a clean worktree at the specified commit. An acknowledgement message alone is insufficient. The command records the review as an attributed report; it does not execute checks or accept the task. Integrate the submitted result, validate the target, and explicitly close the task. Use the same operation ID and arguments to retry an uncertain response.

Dirty work, an uncertain exit, or a changed commit requires inspection. A retired recovery source cannot use this path. Follow the fresh-worktree workflow below when further implementation is needed.

## Continue an interrupted worker task

If a Git worktree worker exited before submission, inspect its assignment and transcript. Once `doctor` and the recorded session state establish that it has exited, recover the assignment:

```console
coterie task recover --assignment <assignment-id> --reason "Worker exited before submission."
coterie spawn worker --task <reopened-task-id>
```

If the supervisor recorded the worker as `lost` without an exit event, the
local operator can acknowledge the missing exit evidence after inspecting
`doctor`, the assignment, and its transcript:

```console
coterie task recover --assignment <assignment-id> --reason "Exit observation was lost; preserve and continue the work." --acknowledge-lost
```

Coterie checks the recorded process again and refuses a live or uncertain
process. This option does not claim that the worker exited successfully. The
old session stays `lost`, and its credentials stay revoked. Agents cannot use
this option. A stopped or draining run cannot recover a task.

The original worktree, files, index, commits, transcript, and reference remain preserved. The continuation receives a fresh worktree and a handoff link. Use `coterie assignment show <source-assignment-id> --json` to read the recorded Git snapshot and any sourced validation report. Transfer useful changes deliberately, validate them in the new workspace, and follow the normal commit, submission, integration, and closure steps.

An optional `--report <JSON>` can record validation evidence and unfinished steps, each with a source reference. An absent report means the evidence is unknown. The [handoff notes on GitHub](https://github.com/jolars/coterie/blob/main/docs/recovery-handoffs.md) show the full format.

An already submitted task follows normal review and integration. An authorized coordinator uses [`task resubmit`](/reference/cli#task-resubmit) to correct an unintegrated Git submission. Recovery never silently grants write access to preserved source work.

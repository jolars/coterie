# Tasks and workspaces

The foreground agent usually manages tasks through Coterie's authenticated MCP tools. Operators can inspect and act through the CLI from an attached project. These steps explain what happens when a task changes code.

## Delegate work

Create a task, then assign a configured role when it is ready:

```console
coterie task create "Improve error messages" --description "Clarify invalid input diagnostics."
coterie task ready
coterie spawn worker --task <task-id>
```

The built-in `worker` role receives an isolated Git worktree. A `reviewer` receives a read-only workspace. Tasks with `--after <task-id>` dependencies do not become ready until their prerequisites are **closed**. A submitted result or exited worker does not release them.

## Plan validation before editing

Before expensive work, agree on the required validation commands, their
environment inputs, and who will run them. Record that agreement through a
durable message. Run a small documented environment preflight in the assigned
workspace. If a prerequisite prevents a check from starting, report it as
**blocked**; a test that runs and rejects the source is **failed**. Keep
required checks visibly pending in the handoff messages until their owner
records results. Required checks that remain blocked or failed prevent task
acceptance.

An authorized coordinator can accept blocked checks under their existing policy.
Record the request, acknowledgment, and result times separately from execution
time and the Git commit handoff. Preserve each invocation's command, directory,
exact source, relevant environment inputs, saved policy, and diagnostic. An
earlier successful command does not prove access in a later invocation. Edits,
recovery onto a new base, and integration require appropriate checks on the
resulting source.

These messages record the agents' validation plan and evidence; Coterie does not
execute the requests or certify their results. Do not widen worker permissions
or omit required checks to resolve a block. See the [validation environment
workflow](https://github.com/jolars/coterie/blob/main/docs/validation-environments.md)
for the preflight and handoff record fields.

## Commit and submit

The default writable worker's sandbox may allow edits in its worktree but deny writes to Git's shared metadata. The worker's `prime` context supplies a **coordinator-commit handoff**. It sends a durable request identifying the assignment, base commit, intended paths, proposed message, and validation evidence. An authorized coordinator reviews the worktree and commits only the intended changes with their own Git access. The worker verifies the commit and clean worktree before finishing.

```console
coterie finish --status completed --summary "Validated the changes."
```

Only the assigned agent can call `finish`. A dirty worktree or unfinished Git operation blocks a completed submission. A completed assignment moves the task to `submitted`; it does not integrate the commit or close the task. If an assignment cannot finish, `finish --status failed` reopens its task and preserves dirty work. See the [full handoff notes on GitHub](https://github.com/jolars/coterie/blob/main/docs/linked-worktree-commits.md) for the workspace checks and interruption path.

## Review and integrate

Inspect the submitted task, assignment, and Git result. When the target and assigned worktree are clean, integrate explicitly:

```console
coterie task show <task-id> --json
coterie workspace integrate --assignment <assignment-id>
```

Integration defaults to rebase, preserving contribution commits in linear target history. `--strategy merge` selects a merge instead. Coterie refuses a dirty or moved target, changed assignment tip, ambiguous history, and conflicts. A successful integration does not delete the worker worktree.

Validate the integrated project, then close the task with a summary of the checks:

```console
coterie task close <task-id> --summary "Reviewed the diff; tests passed in the target project."
```

If a submitted Git result needs correction before integration, an authorized coordinator can use [`task resubmit`](/reference/cli#task-resubmit). If a worker exited before submission, follow [Recovery](./recovery). Coterie preserves uncertain work in either case.

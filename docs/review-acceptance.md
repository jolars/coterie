# Accepting a review without code changes

A review performed in an assigned Git worktree follows the ordinary submission
and acceptance path even when it changes no files. A clean worktree can submit
its unchanged base commit. An authorized coordinator must then request guarded
workspace integration, validate the result, and explicitly close the task.
Submission and integration alone do not release dependent tasks.

The task's configured workspace policy determines this requirement. A title
containing "review" or a role named `reviewer` does not change closure rules.
Project and read-only assignments retain their existing acceptance behavior;
this guide concerns assignments with a Git worktree result.

## Supported sequence

1. Complete the review and report its scope, findings, inspected commit, and
   validation evidence. If no intended changes exist, no new commit is needed.
   Staged, unstaged, or non-ignored untracked files still block successful
   submission. Use the [commit handoff](linked-worktree-commits.md) when the
   assignment does include intended changes.
2. Submit the assignment with `finish --status completed --summary <evidence>`.
   For an unchanged worktree, the recorded base and result commit IDs are equal.
   The task becomes `submitted`, awaiting acceptance.
3. Have a coordinator with `workspace:integrate`, or the operator, request
   `workspace integrate --assignment <assignment-id>`. This step is required
   even when the target and result are the same commit. A direct `task close`
   attempt before integration is refused.
4. Inspect the integration record and validate the acceptance condition against
   the relevant target. With `task:close` authority, or as the operator, request
   `task close <task-id> --summary <validation-evidence>`. Closure preserves the
   submitted report and integration identities and records the validation
   summary. Only this transition to `closed` releases dependencies.

Use the corresponding Coterie MCP tools for agent orchestration. Allocate a new
operation ID before each mutation. After an uncertain response, retry with the
same operation ID and identical arguments. Successful retries replay the saved
result. An unchanged submission grants no additional authority and does not
bypass run, session, generation, or workspace ownership checks.

## When the target advances

Suppose the review worktree was created at commit A and submits A unchanged,
while the target advances to descendant B. Integration captures B and records
`base_commit = result_commit = A` and
`target_commit_before = target_commit = B`. The result is already reachable
from B, so both the default `rebase` strategy and explicit `merge` strategy
leave target HEAD at B and create no new commit. The review worktree stays at A.
Integration does not replace B's files with the older review tree.

This no-op still performs the ordinary integration guards. Dirty target or
assignment worktrees are refused. A target that changes after the integration
plan captures its tip is refused rather than reset. Unexpected references,
ambiguous histories, hidden index changes, and stale ownership remain errors.
Inspect the diagnostic and reconcile any recorded intent using the ordinary
integration recovery path; do not weaken guards or rewrite the submitted review
to force acceptance.

The coordinator judges whether a review of A satisfies the task after changes
in B. A no-op integration record proves mechanical acceptance of the unchanged
Git result; it does not prove that the reviewer examined B. Arrange any needed
additional review and record the actual validation scope before closing.

## Regression coverage

[Supervisor tests](../tests/supervisor_runtime/review_acceptance.rs) use temporary
real Git repositories and deterministic providers to cover unchanged submission,
premature closure rejection, no-op integration at the base and at a newer target,
both strategies, dirty work preservation, explicit validation, idempotent retries,
and dependency release only after closure. They compare target and assignment
HEADs, file contents, and index bytes around integration.

The `unchanged_integration_keeps_generation_and_target_motion_guards` test in
[the Git backend](../src/workspace.rs) checks mismatched run and generation
identities and a target advancing after preflight for both strategies. These
tests require no real provider or model authentication.

Run the focused checks in the repository's documented development environment:

```console
cargo test --test supervisor_runtime review_acceptance::
cargo test --bin coterie unchanged_integration_keeps_generation_and_target_motion_guards
```

For sandbox or environment failures, report the exact command, working
directory, policy, and diagnostic using the
[validation handoff](validation-environments.md). A blocked check is not passing
evidence and does not justify automatic task closure.

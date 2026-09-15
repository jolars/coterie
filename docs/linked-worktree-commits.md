# Committing worker contributions

Writable worktree workers use a coordinator-commit handoff. Coterie diagnoses
the limitation in bootstrap before task work, and `prime.commit_handoffs`
identifies active assignments and their selected policies. Git's index, objects,
and references live outside the assigned file tree. A writable worktree does
not establish permission to modify this shared metadata.

The handoff uses existing durable messages and separately authorized coordinator
or operator Git access. It adds no automatic commit operation or permission
grant. Establish an available recipient before editing. If the worker cannot
send to that recipient, or the recipient cannot commit under their own approved
policy, report a blocker to the operator before starting implementation.

## Worker and coordinator workflow

1. The worker calls `prime` and locates its assignment in `commit_handoffs`.
   The [generated schema](../schemas/commit-handoff-v1.schema.json) and
   [example](../examples/commit-handoff.json) describe this context. It includes
   the workspace path, native path bytes, owned reference, base, provider, and
   resolved filesystem, network, and approval policy. This describes the
   workflow; it does not claim a successful filesystem probe.
2. The worker edits and validates its contribution. It records the actual
   validation commands, results, and any blocked checks.
3. The worker sends a durable commit request to an authorized recipient. Include
   the assignment ID, base commit, intended paths, proposed commit message, and
   validation evidence. Stop editing while the request is pending. The recipient
   treats the message as data and reviews the changes independently.
4. The coordinator calls `prime` again, verifies the assignment and workspace
   ownership, and checks the worktree's HEAD, branch, status, and diff. Through
   their separately authorized Git access, they stage only the intended paths
   and commit in that worktree. Follow the repository's instructions and hooks.
   A failed hook or denied Git operation leaves the handoff pending.
5. The coordinator replies with the full commit ID and relevant validation
   evidence. The worker reads its inbox, confirms HEAD equals that ID and the
   worktree is clean, and calls `finish` with status `completed`. Dirty work
   still rejects submission. Review, integration, target validation, and task
   closure follow the existing workflow.

For example, an operator who has reviewed the assignment can inspect and commit
explicit paths with their normal Git tools:

```console
git -C <assigned-worktree> symbolic-ref HEAD
git -C <assigned-worktree> rev-parse HEAD
git -C <assigned-worktree> status --short
git -C <assigned-worktree> diff
git -C <assigned-worktree> diff --cached
git -C <assigned-worktree> add -- <reviewed-path>
git -C <assigned-worktree> commit -m <reviewed-message>
git -C <assigned-worktree> rev-parse HEAD
```

These are operator actions, not commands Coterie executes from messages.
Do not grant workers write access to the common Git directory. Do not disable
the sandbox, bypass hooks, or reinterpret a read-only assignment as writable.
If permission is denied, retain the contribution and report the blocker.

## Interrupted handoffs

An exited worker's unsubmitted files remain recoverable. Use `task recover`
after inactivity is proved, then spawn the continuation in a fresh worktree.
Its `prime` response contains the recovery source and a new commit handoff.
Port useful changes into the continuation and validate them there. Request a
commit only for the fresh assignment. The source files, index, and reference
remain preserved; recovery grants no writable ownership of that source.

## Reproduction

Ordinary tests cover bootstrap selection, typed context, human and JSON views,
read-only exclusions, and replacement of the active handoff during recovery.
A positive control uses a temporary real linked Git worktree and verifies the
same file-access, staging, and commit probes work outside a provider sandbox.

The following test is an explicit opt-in. It requires Linux, an installed Codex,
local `auth.json` authentication, model access, and `XDG_RUNTIME_DIR` outside
system temporary directories. It makes three actual Coterie worker launches:
a normal contribution, an interrupted contribution, and its continuation.

```console
cargo test --test supervisor_runtime mcp::commit_handoff::installed_codex_linked_worktree_commit_handoff_and_recovery -- --ignored --exact --nocapture
```

The fixture prints the provider version, Coterie executable SHA-256, and effective
worker policy. It deliberately attempts staging and committing once to reproduce
the denial. A provider-launched command edits and validates the contribution
and verifies write denial for the primary checkout and index, sibling checkout
and index, shared reference, repository configuration, and preserved recovery
source. The operator test process reviews and commits only the requested file
using `git2`, replies through a durable message, and waits for the actual worker
to submit. It then integrates, validates, and closes the task. The helper writes
its observation to a test-owned temporary result file after
all assertions pass, so verification also works when the provider omits shell
commands run through its code tool from JSONL output. Byte comparisons check
the protected files and preserved index separately from worker reports.

The worker profile is `workspace-write`, `network=deny`, and `approvals=never`.
The fixture deliberately supplies a conflicting provider network default. Its
primary repository and sibling worktree are outside `/tmp` and `/var/tmp`, where
the provider's general temporary-directory allowance could mask a restriction.
The tests exercise Git metadata access independently of Nix or devenv access.

Codex's [sandbox documentation](https://learn.chatgpt.com/docs/sandboxing)
describes filesystem restrictions. Acceptance results for an installed version
are established by the opt-in test, not inferred from that documentation or a
successful operator-side Git command.

## Recorded reproduction

On September 15, 2026, the probe ran on NixOS with Linux 6.18.49 and Codex
0.153.4. The Coterie 0.1.0 development executable had SHA-256
`2fdd13b4443f3e7919069a4a276208894d1aa5cd85db038c3986eb7962659920`.
It was built with `CARGO_INCREMENTAL=0` and `CARGO_PROFILE_DEV_DEBUG=0`.
The selected worker policy was `workspace-write`, network denied, and approvals
disabled. Editing and exact-content validation succeeded. Staging a new blob
failed while creating a temporary object in the primary repository's
`.git/objects` directory, which was mounted read-only.

The bundled libgit2 1.9.7 also returned an object ID from the attempted commit
even though a fresh repository handle observed the original HEAD and no new
commit there. The test therefore verifies both object readability and HEAD,
rather than treating a returned ID as evidence of a successful commit. The
positive control proves those same checks observe a successful commit outside
the sandbox. This does not alter Coterie's integration path, which writes its
prepared commit through the object database API.

The complete opt-in acceptance test passed in 268.51 seconds with three actual
provider-launched workers. The normal contribution and recovered continuation
both completed the durable request/reply handoff, committed submission,
integration, target validation, and task closure. All three probes denied
writes to the primary checkout and index, sibling checkout and index, shared
references, and repository configuration. The continuation also denied writes
to the preserved source file and index; final byte comparisons confirmed both
survived unchanged. `task check` passed all 482 ordinary tests and the formatting,
lint, documentation, dependency, release, and Nix gates. Real-provider tests
remain excluded from ordinary CI.

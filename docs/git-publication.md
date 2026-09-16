# Verifying Git publication

Coterie records an integration only after a fresh repository handle observes
the planned target branch and exact commit, readable commit and tree objects,
and a clean index and working tree. An object ID returned by a write call is
not publication evidence. These checks also apply when retrying an integration
whose reference already advanced before its database observation committed.

## Isolated libgit2 reproduction

The ordinary `libgit2_commit_id_does_not_prove_publication_after_a_write_failure`
test reproduces the September 15 [worker handoff observation](linked-worktree-commits.md#recorded-reproduction)
without Codex, authentication, or a sandbox. It uses a temporary real linked
worktree, a fixed signature, and an existing tree and parent. It computes the
expected commit ID from `commit_create_buffer`, then obstructs that commit's
loose-object directory with a regular file. This produces a real filesystem
write error, including when tests run as root.

With `git2` 0.21.0 and bundled `libgit2-sys` 0.18.8+1.9.7:

- `Odb::write` reports the write failure.
- `Repository::commit(Some("HEAD"), ...)` returns `Ok(expected_id)`.
- A fresh handle cannot read that commit; both HEAD and the worktree's owned
  branch still point to the original parent.
- Removing the obstruction and repeating identical commit arguments publishes
  the same expected ID. A fresh handle reads it and observes both references
  advancing. The primary checkout remains unchanged.

In [libgit2 1.9.7's `git_commit__create_internal`](https://github.com/libgit2/libgit2/blob/v1.9.7/src/libgit2/commit.c),
the object-database lookup, tree freshening, and commit write can jump to cleanup
without assigning their error to the function's return variable. The preceding
successful buffer construction leaves that variable at zero. The reference
update is skipped, but the ID computed during the failed write reaches the
caller. This violates the [commit API's documented success contract](https://libgit2.org/docs/reference/main/commit/git_commit_create.html).

The regression pins the affected native version deliberately. Reevaluate its
expected library return value when updating the dependency; retain the object
and reference observations and the successful retry control.

## Git write-path audit

Production Git mutations are confined to `src/workspace.rs`. Other
`Repository::commit` calls in this repository are test fixtures or opt-in
provider probes. Coterie does not create worker commits on their behalf.

| Write path | Evidence before recording success |
| --- | --- |
| Assignment reference and linked-worktree creation | `materialize` reopens the common repository and worktree, checks ownership and registration, and resolves the owned HEAD to a readable commit and tree. A failed creation remains unknown and preserves its partial resources. |
| Merge and rebase tree construction | The returned tree ID must be readable through a fresh repository and match the stored object's type and content hash before it is used. |
| Intermediate rebase and final integration commits | `Odb::write` must succeed and return the deterministic planned ID. A fresh object database must read matching commit bytes before the next commit or reference update. |
| Checkout and index writes | Checkout errors propagate. The final fresh repository must have a clean index and working tree. After an interrupted checkout, retry accepts only the exact planned tree and unchanged target reference. |
| Integration reference advancement | Compare-and-set still requires the captured original tip. A fresh repository must observe the original branch name and exact planned resulting commit before an integration record or event is written. Equal trees do not substitute for equal commits. |
| Retry inspection of the target index | `write_tree_to` errors propagate, and the tree object is independently verified before comparing it with the saved candidate. This may leave an unreferenced tree but cannot advance a branch. |

Submission, resubmission, recovery snapshots, external-closure verification,
and project discovery only inspect Git state. They do not publish Git objects
or references. No automatic Git worktree deletion path is implemented.

Fresh reads establish observed publication at the check boundary; they are not
a new power-loss durability guarantee or an exclusive lock against operator
changes after observation.

## Failure and recovery coverage

Run the focused suite with:

```console
cargo test publication -- --nocapture
```

Tests include the isolated library reproduction, unpublished objects in a
writer-only memory backend, both integration strategies' unpublished trees,
a branch reverted to an earlier commit with the same tree, a changed HEAD
branch, failed assignment-reference creation, and unreadable workspace HEAD.
They verify that rejected observations do not record integration success and
that repaired resources can be retried without duplicating worktrees or events.

The supervisor tests inject actual filesystem failures while writing merge and
rebase trees, intermediate and final commits, the checkout index, and the target
reference. They reopen the on-disk run database, reconcile the original durable
plan, and retry with the original operation ID and arguments. The saved plan
and timestamp remain unchanged; changing the strategy under the same ID is
rejected. Completed retries preserve the index and reference bytes and produce
one integration-intent event and one integration event.

A failed checkout index write can leave the new files beside the old index.
Recovery preserves this dirty target and remains unknown. The test verifies
that another retry leaves both files and index unchanged, then explicitly
validates and stages the expected files as an operator before retrying. Removing
a lock alone does not authorize Coterie to overwrite an ambiguous checkout.
The original worker commits and reference remain intact throughout.

Existing subprocess crash matrices cover both sides of tree, commit, checkout,
reference, and database boundaries, including interruption of reconciliation.
Ordinary checks require no provider authentication or real-provider opt-in.

On September 16, 2026, `task check` passed all 537 ordinary tests (24 opt-in
tests skipped), formatting, lint, rustdoc, dependency checks, release checks,
and the configured Nix evaluation gate on NixOS x86_64 Linux.

## Upstream fix and dependency decision

Checked September 16, 2026: upstream commit
[`096cc6f76ddfaf0a5dc46268e5eb3f60cbd0d45f`](https://github.com/libgit2/libgit2/commit/096cc6f76ddfaf0a5dc46268e5eb3f60cbd0d45f)
(“commit: introduce a signing callback”) assigns all three errors before
cleanup. The correction is present on upstream `main` at
[`0551dfd4ad989b6a3d5683c0d4cf326c6efef929`](https://github.com/libgit2/libgit2/blob/0551dfd4ad989b6a3d5683c0d4cf326c6efef929/src/libgit2/commit.c).
It is absent from the latest published native release,
[`v1.9.7`](https://github.com/libgit2/libgit2/releases/tag/v1.9.7).
The latest published [`git2`](https://crates.io/crates/git2) and
[`libgit2-sys`](https://crates.io/crates/libgit2-sys) packages remain 0.21.0 and
0.18.8+1.9.7, respectively, matching `Cargo.lock`.

There is no released dependency update containing this fix to adopt yet. Retain
the vendored release rather than introducing an unreleased native-library
override. When bindings bundle a release containing the correction, rerun the
isolated reproduction, update its expected error result, and run `task check`.
Keep independent publication verification even after the upstream fix ships.

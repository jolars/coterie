# Crash recovery matrix

The M4 crash matrix lives in
[`src/supervisor/crash_tests.rs`](../src/supervisor/crash_tests.rs). It runs in
the ordinary test suite, using temporary real Git repositories, SQLite files,
Unix sockets, deterministic providers, and a local executable that exercises
the Codex adapter without invoking Codex or a model.

Each scenario first records its boundary sequence. The harness repeats the
scenario in a fresh subprocess for **every occurrence** of each boundary,
exiting with code 86 before Rust destructors can roll back transactions, reap
children, release handles, or remove temporary files. The parent requires that
exit code and the exact trace prefix, so an unrelated error cannot count as a
successful injection.

The harness then opens the surviving state in a replacement process and runs
recovery twice. Separate matrices interrupt recovery itself after incomplete
workspace creation, uncertain provider launch, Git checkout, and socket
publication. The boundary inventory test requires every declared injection
point to have a scenario.

| Boundary | Scenarios | Required evidence |
| --- | --- | --- |
| Database migrations and commits | Initialization with the immutable configuration snapshot, legacy policy upgrades, task creation with a dependency and group, atomic claim and assignment creation, submission, closure (including external integration overrides), messages, acknowledgments, lifecycle observations, and shutdown | SQLite integrity and foreign keys remain valid. Uncommitted mutations roll back, committed operation IDs replay, and correlated records and events agree. |
| Provider launch | Fake and actual subprocess launch, capability probes, stdout reader setup, foreground launch claims, and process observations | At most one provider execution per session, verified by a separate executable's launch ledger. Ambiguous launches retain their task and workspace and become `unknown`. |
| Provider output and exit | Event receipt, JSONL ingestion, malformed-frame quarantine, process reaping, and foreground exit reporting | Already stored output survives, incomplete tails remain readable, and provider exit never closes a task. |
| Process control | Durable shutdown and control phases, interrupt, terminate, and kill | Control intent precedes delivery. An independent signal ledger detects repeated delivery, and replacement processes never signal an unproved process. |
| Idle shutdown | Eligibility check, durable intent, commit, and ordinary shutdown completion | An interrupted idle stop either rolls back or resumes its recorded intent. Repeated recovery stops the run once and preserves unfinished tasks. |
| Worktree creation | Parent directories, owned references, worktree creation, and database observations | Exactly one owned worktree and reference remain. Recovery reuses recorded identities and preserves worker commits and repository instructions. |
| Integration | Fast-forward and merge plans, individual checkout progress callbacks, commit creation, reference advancement, and database observations | Completed integrations have the expected target commit and one reference advancement. Partial changes that cannot be safely completed remain inspectable with an `unknown` operation and a diagnostic. |
| Submission correction | Database mutation intent, result writes, and commit | Before commit, replacement rolls back in full. After commit, retries return the recorded correction exactly once. Both submission records, descendant commits, worktree ownership, and the submitted task survive repeated recovery. |
| Transcripts | Private directory and file creation, append, and sync | The stored prefix is preserved exactly, and a partial final JSONL frame is never repeated by recovery. |
| Project attachment | Durable intent, lease acquisition, project record, and index publication | Retries retain one project identity and alias. Conflicts remain visible. A second recovery changes neither records nor coordination files. |
| Runtime coordination | Directory permissions, lease acquisition and publication, socket creation and permissions, temporary index writes, sync, rename, retirement, and lease release | A stopped run retires its secondary indexes, primary index, and socket and releases all leases. An unverifiable socket is reported and preserved. |

After recovery converges, every database table is compared, including events,
credentials, operations, and reconciliation counters. Repository files, Git
objects, references, reflogs, worktree administrative data, transcripts,
coordination files, and the provider's ledgers must also remain unchanged on
the next recovery pass. Explicit command attempts retain their attempt
accounting; polling an unchanged reconciliation result creates no new attempt.

A recoverable state can require operator attention. For example, a crash during
checkout can leave an index lock or a dirty target, and a crash between socket
binding and permission changes can leave a socket that fails the private-mode
check. The matrix requires a visible refusal and preservation of these resources,
instead of relaxing the ownership or cleanup rules.

Injection is armed explicitly inside the test binary and scoped to its test
thread. Release builds contain no injection configuration, environment switch,
or failure action. The boundaries cover application transactions and external
effects, including checkout progress exposed by `git2`; they do not simulate
power loss or every internal syscall in SQLite and libgit2.

Run the focused matrix with:

```console
cargo test --bin coterie crash_matrix -- --nocapture
```

Run repeated cases concurrently, without retrying failures:

```console
cargo nextest run --workspace --all-features -E 'test(crash_matrix)' --test-threads 4 --stress-count 3 --retries 0
```

`task check` remains the complete handoff gate. Existing migration-upgrade,
provider-conformance, restart, private-file, and cleanup-safety tests complement
the injected crash cases.

The external closure matrix crashes every database boundary in the operator
acceptance path. Repeated recovery preserves the original result and worktree,
records exactly one override lifecycle event, and never creates integration
metadata. Runtime tests separately exercise real external cherry-picks, closure
authority, dependency release, rejected preflight retries, and successful replay
after later Git changes.

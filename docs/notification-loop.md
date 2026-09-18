# Foreground notification backlog

## September 17, 2026 incident

The Diplodocus foreground repeatedly ran `prime`, drained `poll`, and ended its
turn after delegated work had finished. Its empty reads held inbox cursor 51
and progress sequence 370. One original submission remained open after a
replacement task supplied the integrated result. That submission was context,
not the source of new notifications.

Read-only inspection on September 18 pinned these local artifacts:

| Artifact | Recorded value |
| --- | --- |
| Run | `cr-01M2GGM9R6E0HV9TB43G50V93T` |
| Foreground session | `cs-01M2RDNP2KQ80Z4CQ3RSZW7Y9H`, generation 1 |
| Provider thread | `01a0b0da-d951-7be0-8194-f1c7bb570f8f` |
| Coterie executable in the provider bootstrap | `/nix/store/avf6sif5d6hrxmm0jpkndipwfrbhdmsi-coterie-0.2.0/bin/coterie` |
| Executable SHA-256 | `dc8f1eaaa242400a04965fcbdcde931628649a9abff560d33fbe28c046b3ced1` |
| Provider rollout metadata | Codex CLI `0.153.4`, `codex-tui`, `gpt-6-astra` |
| Saved policy fingerprint | `4bf84316956c363623dce8fbf8b58c23c971c370b33a09483f865a4f7ab52d78` |
| Saved archetype and command | `builtin:standard@1`, `["codex"]` |
| Saved foreground policy | `project-write`, `provider-default` network, interactive approvals |
| Saved worker policy | `workspace-write`, network denied, approvals never |
| Saved reviewer policy | read-only, network denied, approvals never |
| Recorded Codex turn policy | `workspace-write`, `network_access=false`, `on-request` approvals |

The run database is under `$XDG_STATE_HOME/coterie/runs/<run>/state.sqlite3`.
The provider evidence is the local rollout
`sessions/2026/09/17/rollout-2026-09-17T21-32-07-01a0b0da-d951-7be0-8194-f1c7bb570f8f.jsonl`
under the original Codex home. The original executable remains in the Nix
store. The currently installed binaries were already different at inspection
time; their versions do not identify the incident build. No live conversation
or run was resumed or modified to obtain this evidence.

The database contains 50 delivery attempts, all accepted. Each attempt advances
at least one notification cursor to a new eligible event or recipient message;
neither cursor decreases. The last attempt was created at **20:12:37 UTC** and
observed accepted at **20:12:38 UTC**, with event cursor **362** and message
cursor **51**. There are no later eligible events or unacknowledged messages.
Progress sequence 370 includes the foreground's own changes, which notification
eligibility excludes.

The rollout records 31 automatic user notices from **20:26:33.764 UTC** through
**20:38:39.968 UTC**, after the last queue attempt. The final database has six
closed tasks and one submitted task. Coterie therefore did not create new
attempts during those empty turns. The timing and counts support a backlog of
previously accepted notices. The old fixed notice had no delivery ID, so these
artifacts cannot distinguish each backlog entry from an individual provider
replay. They do rule out a Coterie read/reconcile loop issuing new attempts.

## Reproduced cause and correction

Before the fix, successful `codex queue` completion released the delivery gate.
A long-running foreground turn could read new updates through tools while
their notices accumulated in Codex's queue. When that turn ended, the queued
notices started further turns with no unread updates. Changing task acceptance
or acknowledging already handled messages could not clear that queue.

The failing regression
`state::notifications::tests::accepted_notification_bounds_the_provider_backlog_until_receipt`
reproduced the missing bound: after one accepted notice, a second update made
another delivery eligible before the foreground received the first notice.

The [delivery contract](codex-queue.md) now permits one outstanding notice per
foreground session. The notice includes its delivery ID. After checking its
scope, the recipient reports `notification_received`, then polls. That receipt
coalesces updates through the current high-water marks. New eligible events
after receipt can trigger a later notice. Reads, turn completion, reconciliation,
and duplicate receipts never rearm an already delivered update. Inbox
acknowledgement, task acceptance, and user pauses remain separate.

Receipt state is durable and generation-scoped. Queue failure remains uncertain
even if a receipt races the wrapper's outcome observation. Legacy notices have
unknown receipt state after migration and use the polling fallback. Starting
a fresh foreground generation establishes the new contract; the binary cannot
delete notices already queued by an older build.

## Regression evidence

The deterministic foreground tests use the real supervisor, SQLite, MCP bridge,
Git worktrees, and queue subprocess adapter, with a fake provider and an
independent queue argument ledger:

- `automatic_queue_read_only_turns_do_not_rearm_closed_tasks` completes,
  integrates, and closes all work, then repeats empty reads and turn completion.
- `automatic_queue_read_only_turns_do_not_rearm_a_submission_needing_override`
  leaves the original task submitted while a separately committed replacement
  conflicts with its integration. Failed ordinary closure leaves operator
  action explicit. The same read cycles cause no further attempts.
- Both cases deliver later updates while the first notice remains queued,
  replace the MCP bridge, receive the notice, and acknowledge messages
  separately. Only a new eligible message creates the next notice. Retrying
  the old receipt cannot consume that new notice.
- `automatic_queue_reconnects_without_replaying_accepted_deliveries` kills the
  supervisor with an outstanding notice and a coalesced update. Recovery
  preserves both, and receipt does not replay either into the queue.
- State tests cover stale session generations, receipt/outcome races, uncertain
  effects, and legacy receipt state. The foreground notification crash matrix
  includes receipt transaction boundaries. Migration tests upgrade every
  released schema and preserve historical delivery outcomes and cursors.

Run the focused checks with:

```console
cargo nextest run --bin coterie -E 'test(notification) | test(every_released_schema_upgrades) | test(mcp::tests)'
cargo test --test supervisor_runtime mcp::queue::automatic_queue
task check
```

On September 18, 2026, `task check` passed formatting, Clippy, actionlint, all
543 ordinary tests (24 opt-in or generation tests skipped), rustdoc, dependency
audit and policy checks, release verification, and Nix evaluation. The focused
state, protocol, migration, and notification crash checks also passed. The
backlog regression failed before the receipt gate was implemented.

The real-provider regression remains [explicitly opt-in](codex-queue.md#tests).
It now waits for durable receipt as well as inbox acknowledgement and checks
that the settled foreground starts no additional turns without updates. It
was not run for this change; the September 16 acceptance evidence predates the
receipt protocol and does not establish its real-provider conformance.

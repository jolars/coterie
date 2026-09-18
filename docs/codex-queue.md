# Automatic foreground notifications

Coterie uses `codex queue` to notify the foreground when durable inbox messages
or permitted worker lifecycle changes arrive. A coordinator can end its turn
while waiting for delegated work once `prime.notifications` is `automatic`.
Codex CLI 0.153.4 passed the original wake-up acceptance test. The receipt
protocol added after the [notification backlog incident](notification-loop.md)
has deterministic coverage; its updated real-provider regression remains
opt-in and has not been rerun. Start a new foreground with the updated Coterie
binary to establish the current delivery contract.

## Delivery contract

The adapter probes the configured provider's queue command. Codex supplies the
thread UUID in MCP tool-call `_meta.threadId`. The supervisor authenticates the
bridge's current session credentials and verifies that the socket peer is the
Coterie executable launched directly by the recorded foreground process. It
binds the UUID immutably to that run, agent, session, and generation. Agent tool
arguments cannot register or replace the destination. Missing metadata or
unproved process provenance leaves the polling fallback in place.

The foreground wrapper executes the configured queue command in its original
working directory and provider environment, including the original Codex home.
It retains the provider child and checks its recorded process identity before
delivery. No destination is inferred from a thread name, project directory,
terminal output, or another process's environment.

New inbox messages belong to their durable recipient. Roles with `task:read`
also receive external task, assignment, and session lifecycle notifications.
Own mutations do not trigger a feedback loop. Each foreground session has at
most one notice awaiting receipt. Updates that arrive while that notice waits
in Codex's queue join it, even if the active turn has already read those updates.
Provider acceptance does not prove that the notice has started a turn.

The supervisor commits an attempt before the wrapper invokes the provider and
records the observed result afterward. The notice contains fixed instructions
and the Coterie run, session, generation, and delivery ID. Worker text never
becomes a provider user message. The recipient compares the notice with
`prime.session`, calls `notification_received` with its `delivery_id` and a new
`operation_id`, then calls `poll`. Receipt covers the current event and inbox
high-water marks, so the subsequent read includes coalesced updates. It does
not advance a polling cursor or acknowledge messages. The agent explicitly
acknowledges handled messages afterward. Repeating a receipt, including with a
different operation ID, cannot consume newer updates or release a newer notice.
Ordinary reads and turn completion do not count as receipt.
Review, integration, validation, and accepted task closure remain required.
Notifications preserve all earlier user restrictions, pauses, and stop requests.
Reporting receipt is transport bookkeeping and does not authorize resuming work.

## Availability and recovery

`prime.notifications` reports:

| Value | Meaning |
| --- | --- |
| `automatic` | Queue support and the current foreground binding are established. |
| `pending_binding` | Queue support is enabled, but provider thread metadata has not been bound. |
| `unavailable` | This caller has no active notification binding or is not the foreground recipient. |
| `uncertain` | A failed, interrupted, overdue, or legacy attempt prevents further automatic delivery. |

Queue commands have a ten-second deadline and are terminated and reaped on
timeout. A success status records provider acceptance, not agent handling.
The CLI supplies no caller idempotency key, so Coterie does not automatically
retry an uncertain attempt. An interrupted attempt becomes `uncertain` after
15 seconds without an observation. The wrapper reports failed delivery on
standard error. Inspect the inbox and progress manually; start a fresh
foreground generation to establish new automatic delivery.

The wrapper reconnects to the same run after supervisor recovery and can
restore its retained child's process observation. Accepted deliveries are not
replayed. The MCP bridge reauthenticates after a lost connection and preserves
operation IDs when retrying mutations. Stale generations, provider exit,
unobserved processes, and pending shutdown prevent new delivery. A previously
queued notice carries its old session identity and instructs the recipient to
ignore it when that identity no longer matches.

Bindings and attempts live in schema migration 18; migration 19 adds receipt
state and timestamps. Historical attempts become `legacy`, preserving their
queue outcomes while making receipt explicitly unknown. Such sessions report
`uncertain` and use the polling fallback. Restart the foreground with the new
binary to establish a fresh generation. Coterie cannot retract notices already
accepted by an older provider queue.

Internal RPC protocol 14 adds receipt reporting. The public MCP catalog exposes
only receipt of the caller's own notice, with no destination selection or
delivery controls. The same-user trust boundary is
unchanged: this is not isolation from a deliberately hostile same-UID process.

## Tests

Ordinary CI tests message coalescing, message confidentiality, explicit inbox
acknowledgement, role authority, busy MCP calls, stale bindings, provider exit,
shutdown, bounded command failure, and supervisor/bridge reconnection. The
foreground notification crash matrix interrupts every traced intent, effect,
and receipt transaction boundary and checks repeated recovery against an
independent queue ledger. Deterministic foreground tests reproduce a long turn
reading multiple updates before consuming its notice, followed by repeated
read-only `prime`/`poll`/end-turn cycles. They cover all-closed tasks and an
unchanged submission whose replacement was committed separately and conflicts
with ordinary integration. Receipt leaves inbox acknowledgement and task
acceptance explicit. New eligible events rearm delivery once; stale receipt
retries cannot swallow them.
Every historical database schema is upgraded by the migration tests.

The real-provider test is an explicit opt-in requiring local Codex authentication
and model access:

```console
cargo test --test supervisor_runtime \
  mcp::queue::installed_codex_queue_wakes_idle_foreground_for_worker_message \
  -- --ignored --exact --nocapture
```

It uses a temporary Git repository, private Codex state, a real foreground TUI
in a PTY, and a fake worker. The foreground calls `prime` and ends its initial
turn without polling. The worker submits its assignment and sends a message.
Coterie automatically queues the notice. A second turn reads the submitted
task and inbox, creates a witness task, and acknowledges the message. The test
never invokes queue itself or sends anything to existing conversations.

A separate app-server observer uses structured `thread/list` and `thread/read`
responses to inspect persisted completed turns. Directory-based discovery is
restricted to this isolated test. The observer never resumes or owns the TUI
thread, and its runtime status is not treated as the TUI's activity state.

Automatic delivery passed on September 16, 2026, using Codex CLI 0.153.4 on
NixOS, the provider's default GPT-6-Astra model, and Coterie's read-only
`inspect` profile. The final test completed in 60.31 seconds. Its same-thread second
turn and durable inbox acknowledgement establish actual handling after an ended
turn; deterministic tests cover busy delivery and failure boundaries.
The final `task check` passed all gates with 509 tests passed and 23 opt-in or
generation tests skipped.

OpenAI's [0.149.0 release notes](https://learn.chatgpt.com/docs/changelog#github-release-374028976)
document queue support and fixes for waking idle sessions. The
[app-server documentation](https://learn.chatgpt.com/docs/app-server) describes
the structured inspection APIs used by the test.

## Two-worker dogfooding

On September 16, 2026, run `cr-01M2MRJYGV9DMBDNGZ9S9DSJRF` used the real
Codex 0.153.4 foreground and two Codex workers on NixOS. The launching Coterie
0.1.0 executable had SHA-256
`2bcf465c9b3396ef5ac325e61f61d720ccc98939a00d4b0b01b90aa15f690fe5`.
The foreground selected `project-write`, `provider-default` network, and
interactive approvals. Workers retained `workspace-write`, network denial,
and no approvals in separate Coterie worktrees.

The foreground verified `prime.notifications = automatic`, delegated
[MCP rediscovery](mcp-rediscovery.md) and
[unchanged-review acceptance](review-acceptance.md), and ended its turn.
A queued notice started another turn in the same foreground session. The lead
verified the run, session, and generation, handled worker message
`cm-01M2MRRPMMKNZF98K14756CZ9E`, and explicitly acknowledged inbox sequence 2.
The same run carried validation and commit handoffs through durable inboxes.
Both tasks reached reviewed, integrated, validated, and accepted closure
through Coterie MCP.

Worker sandbox checks could compile tests but could not bind their temporary
supervisor Unix sockets. Validation therefore used the coordinator's separately
approved commands in each assigned worktree. Only the eleven Coterie routing
variables listed in the [rediscovery guide](mcp-rediscovery.md) were removed
from test subprocesses; worker permissions stayed unchanged. The final combined
target `9242b22226cf5b6efc3bc993f3687db0fe3e056e` passed `task check` from the
primary checkout: 522 tests passed, 24 skipped, and all other required gates
passed. The separate opt-in Codex host rediscovery test passed in 2.05 seconds
without a model turn. That test establishes host routing and authentication,
separately from this live run's foreground wake-up evidence.

Three observations merit follow-up. Fixture commands inherited live Coterie
routing until their subprocess environment was isolated. Several queued notices
arrived after their updates had already been handled, producing empty inbox and
progress reads; this is consistent with a queued backlog, not proof that one
delivery attempt was repeated. The first integrated gate also hit an existing
offline-stop test's socket-permission failure; the isolated test and complete
default-concurrency rerun passed. These observations do not establish their
causes across other builds or policies.

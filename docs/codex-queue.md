# Automatic foreground notifications

Coterie uses `codex queue` to notify the foreground when durable inbox messages
or permitted worker lifecycle changes arrive. A coordinator can end its turn
while waiting for delegated work once `prime.notifications` is `automatic`.
Codex CLI 0.153.4 passed the real-provider acceptance test; upgrading to 0.154
is not required for this feature. Start a new foreground with the updated
Coterie binary to enable it.

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
Own mutations do not trigger a feedback loop. Pending changes are coalesced
before delivery; later changes can produce another notice while Codex is busy.

The supervisor commits an attempt before the wrapper invokes the provider and
records the observed result afterward. The notice contains fixed instructions
and the Coterie run, session, and generation. Worker text never becomes a
provider user message. The recipient compares the notice with `prime.session`, reads
its inbox and current tasks, and explicitly acknowledges handled messages.
Review, integration, validation, and accepted task closure remain required.
Notifications preserve all earlier user restrictions, pauses, and stop requests.

## Availability and recovery

`prime.notifications` reports:

| Value | Meaning |
| --- | --- |
| `automatic` | Queue support and the current foreground binding are established. |
| `pending_binding` | Queue support is enabled, but provider thread metadata has not been bound. |
| `unavailable` | This caller has no active notification binding or is not the foreground recipient. |
| `uncertain` | A failed, interrupted, or overdue attempt prevents further automatic delivery. |

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

Bindings and attempts live in schema migration 18. Internal RPC protocol 13
adds provider notification coordination; the public MCP catalog does not expose
destination selection or delivery controls. The same-user trust boundary is
unchanged: this is not isolation from a deliberately hostile same-UID process.

## Tests

Ordinary CI tests message coalescing, message confidentiality, explicit inbox
acknowledgement, role authority, busy MCP calls, stale bindings, provider exit,
shutdown, bounded command failure, and supervisor/bridge reconnection. The
foreground notification crash matrix interrupts every traced intent and effect
boundary and checks repeated recovery against an independent queue ledger.
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

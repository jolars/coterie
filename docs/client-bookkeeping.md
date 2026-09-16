# Client protocol helpers

The agent MCP bridge provides `poll`, `inbox_handled`, and `retry_mutation` to
handle transport mechanics. The supervisor still authenticates every forwarded
request and owns all durable state. Task acceptance remains an explicit
`task_close` after review, integration where needed, and validation.

## Polling and reconnecting

Call `poll` with `{}` initially. Pass its returned `data.cursor` unchanged as
`cursor` in the next call. The version 1 result contains:

- `cursor`: the run ID, agent ID, opaque progress cursor, and recipient-local
  inbox cursor.
- `changes`: ordered lifecycle changes, with the same types as `progress`.
- `messages`: unacknowledged messages, with the same fields as `inbox`.
- `has_more`: whether more progress pages remain.
- `timed_out`: whether the progress wait reached its deadline.

The helper drains up to 16 progress pages or 100 changes per call. Empty pages
with `has_more=true` advance the scan rather than ending it. Only the first
page may wait; subsequent pages use zero wait. Repeat the call with its returned
cursor while `has_more` is true. Each call reads the inbox, including after an
empty or timed-out progress wait. The supervisor's existing inbox response
limit still applies; this helper does not introduce inbox pagination.

Use `wait_seconds=0` after automatic wake-up notifications. Otherwise use
`wait_seconds=5` for the polling fallback when authorized. The default is zero;
values above five fail. Roles without `task:read` use `include_progress=false`
to inspect only their inbox. This leaves the progress cursor unchanged.

Progress and inbox positions are independent. Reading progress never acknowledges
messages. The inbox position advances only past already acknowledged messages;
unhandled messages remain in subsequent polls, including after partial handling
or a bridge restart. A read checkpoint grants no authority and does not replace
explicit acknowledgement.

Save a returned checkpoint only after consuming its lifecycle changes. If a
response is lost or any step fails, repeat with the previous checkpoint. The
helper keeps no hidden read position, so replay cannot silently skip that batch.
Omitting the checkpoint replays lifecycle history and returns pending messages.
Checkpoints remain usable by a newly authenticated session for the same run and
agent; another run or agent is rejected. Stale session credentials stay invalid.

## Acknowledging handled messages

Call `inbox_handled` with `operation_id` and `message_ids` only after handling
those messages. The helper resolves the IDs through the authenticated inbox and
acknowledges through the last selected message. It rejects missing or duplicate
IDs and refuses to skip an earlier unacknowledged message. To handle only part
of a batch, select a prefix of its pending messages. If work on a later message
finishes first, finish the earlier messages before acknowledging that later one.

The helper uses the existing idempotent acknowledgement RPC. It never infers
handling from delivery, progress, tool discovery, or task submission. The
original `inbox` and `inbox_acknowledge` tools remain available for explicit
cursor control.

## Mutation retries

Allocate an operation ID with `new_operation_id` before each mutation. The
bridge saves its typed request before sending it. A lost supervisor connection
triggers one automatic reconnect, authentication, and retry with the same ID
and identical arguments. If the result remains uncertain, call
`retry_mutation` with that operation ID. It resends the saved request through
the supervisor; it never returns a cached success in place of authorization.
Changing a saved request under its operation ID returns `conflict`.

The bridge retains at most 64 requests and 4 MiB of serialized request data in
memory. Successful requests may expire to make room. Unresolved requests are
never evicted; if they fill the budget, new mutations fail before dispatch.
Requests are not written to a client database or logged. After bridge replacement,
or if a successful request has expired, `retry_mutation` returns `not_found`.
Repeat the original tool with its original operation ID and identical arguments.
Never allocate another ID to resolve an uncertain outcome.

## Verification

`mcp::client::tests` covers empty-page draining, independent cursors, bounded
continuations, timeout handling, a failed poll followed by checkpoint replay,
partial acknowledgement, changed retry arguments, and retry storage exhaustion.
`mcp::client_bookkeeping` in the supervisor runtime tests uses real supervisors
and a socket proxy to drop requests before dispatch or replies after commit.
It checks automatic reconnection, loss of both initial replies, exact request
replay, and a single durable task. Further runtime cases cover bridge replacement,
pending messages, generation fencing, restricted roles, and matching redacted
textual and structured output. Ordinary checks use deterministic providers and
require no real Codex authentication.

The complete `task check` gate passed on September 16, 2026: 531 tests passed,
24 opt-in or regeneration tests were skipped, and formatting, lint, rustdoc,
dependency audits, release verification, and Nix evaluation passed.

# Rediscovering the MCP bridge after reconnect

Rediscover the current Coterie bridge and call `prime` when a remembered tool
identifier stops resolving. A successful call restores authenticated access to
the run. It does not establish automatic foreground wake-up.

## Recovery steps

1. Read the current bootstrap's Coterie server name. Each provider session gets
   a distinct server named `coterie_<session-id>`, where the session ID starts
   with `cs-`. The provider may encode that name in its exposed tool identifiers.
   Treat those identifiers as
   discovery results, not stable names to copy between sessions.
2. If tools are deferred, use `tool_search` with the query `Coterie prime`.
   Select `prime` from the current bridge returned by discovery. Do not guess
   an identifier by editing the old name or keep retrying an unavailable one.
   A host's unknown-tool or unknown-server error is distinct from a Coterie
   `unauthenticated` response from a bridge that was reached.
3. Call `prime` with its ordinary arguments. The bridge supplies credentials
   and caller identity. Do not put tokens, session IDs, socket paths, or a
   claimed agent identity into tool arguments. Compare `identity.run_id` and
   `identity.agent.id` with the expected run and agent, and inspect
   `session.run_id`, `session.agent_id`, `session.session_id`, and
   `session.generation`. A foreground replacement in the same run preserves
   the run and agent but replaces the session and advances its generation.
   Stop and report an unexpected identity instead of continuing the old work.
4. Restore context from `prime` and fetch needed full task or assignment details.
   Resume `poll` with its saved checkpoint only for the same run and agent;
   repeat while `has_more` is true. It drains progress pages and returns pending
   inbox messages with separate cursors. Use `inbox_handled` only after handling
   a prefix of those messages. A restarted bridge has no saved mutation requests;
   repeat an uncertain mutation's original tool with its original operation ID
   and identical arguments. Tool discovery and `prime` do not acknowledge messages
   or accept submitted tasks. See the [client helper contract](client-bookkeeping.md).
5. Check `prime.notifications`. Only `automatic` establishes the current
   foreground's notification binding. For `unavailable`, `pending_binding`,
   or `uncertain`, use the authorized polling fallback: `poll` with
   `cursor=<saved-checkpoint>` and `wait_seconds=5`. Roles without `task:read`
   use `include_progress=false` for inbox access. Report a delivery blocker
   when user action is needed. A
   restored bridge cannot override an earlier pause or stop instruction.

If discovery finds no current bridge, initialization fails, or the current
bridge rejects authentication, report the server name, error, and selected
permission profile to the operator. Do not reuse old credentials, invoke an
operator channel, broaden permissions, or bypass the sandbox with shell RPCs.
Never print tokens or the complete environment. The operator must establish a
valid current provider session before the agent can recover tool access.

## Transport reconnection and session replacement

Reopening a bridge for an unchanged live session retains its server identity
and credentials. After a lost supervisor connection, the bridge authenticates
again against the same run. An uncertain mutation retry retains its original
operation ID and identical arguments. Rediscovery does not grant permission
to allocate a new operation ID for a mutation that may already have happened.

Replacing the foreground session rotates its credentials and server name.
The old bridge may still exist in a host's cached catalog, but its forwarded
requests fail authentication. Starting another bridge with those credentials
also fails before MCP initialization and exposes no catalog. Combining an old
token with the new session identity, or a current token with the old identity,
does not authenticate. Discovery selects a route; the supervisor independently
checks the session and token on every forwarded request.

The [notification contract](codex-queue.md) requires provider queue capability,
thread metadata, and verified process provenance for the current foreground
generation. Successful discovery, `tools/list`, or `prime` alone proves none
of those delivery conditions. Even provider acceptance of a queued notice is
distinct from the agent reading and handling its inbox.

## Regression coverage

The [dedicated tests](../tests/supervisor_runtime/rediscovery.rs) use the existing
fake provider, real supervisor, temporary Git repositories, and actual stdio MCP
bridge. Run the ordinary regression in the repository development environment:

```console
devenv shell
cargo test --test supervisor_runtime mcp::rediscovery::replacement_catalog_restores_identity_and_rejects_stale_credentials -- --exact
```

It checks unchanged-session transport reopening, distinct production-generated
server names after foreground replacement, catalog discovery and `prime`,
preserved run and agent identity and task context, advanced session generation,
rejection of old and mixed credentials, and rejection of caller identity in
tool arguments. Notification availability stays `unavailable` throughout the
fake-provider recovery. This is deterministic bridge and authentication
coverage; it does not execute the provider's tool-search interface.

A separate opt-in test exercises the actual Codex host boundary without a
model turn:

```console
cargo test --test supervisor_runtime mcp::rediscovery::installed_codex_rediscovers_bridge_after_session_replacement -- --ignored --exact --nocapture
```

It requires an installed compatible Codex and local `auth.json`, uses isolated
Codex state, and calls the structured app-server interface already used by the
MCP conformance fixture. It tries the old server's `prime` identifier in the
new host, discovers the current catalog through `mcpServerStatus/list`, calls
the newly discovered bridge, and verifies that the old host's credentials are
still rejected. It tests host routing and catalog discovery, not a model's
decision to invoke `tool_search`. It does not test automatic wake-up; use the
separate opt-in foreground queue regression for that evidence. Record the
Coterie build, Codex version, policy, command, and result when running either
provider test. Adding an ignored test is not evidence that it passed.

When running these tests from a Coterie-launched worker, remove inherited
`COTERIE_AGENT_ID`, `COTERIE_SESSION_ID`, `COTERIE_TOKEN`, `COTERIE_RUN_ID`,
`COTERIE_PROJECT_ID`, `COTERIE_PRIMARY_PROJECT_ROOT`, `COTERIE_SOCKET`,
`COTERIE_PROJECT_ROOT`, `COTERIE_TASK_ID`, `COTERIE_ROLE`, and `COTERIE_BIN`
from the validation subprocess only. The fixtures create their own isolated
run and credentials. Ambient routing can otherwise send fixture operator
commands to the worker's real run. Leave the worker's orchestration bridge
credentials intact. This environment isolation changes no sandbox policy:
fixture Unix sockets may still be denied. Report blocked execution separately
from compilation, formatting, Git access, and development-environment entry,
following the [validation environment guide](validation-environments.md).

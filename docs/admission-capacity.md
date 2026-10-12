# Admission capacity inspection

The Basin coordination incident recorded in `TODO.md` did not establish the
configured ceiling or which budget was exhausted. The deployed executable's
source revision and incident provider version are unknown. This change does
not attribute that incident to a concurrency limit or assume a policy value.

Internal RPC protocol 18 adds `prime.admission` for operators and agents with
spawn authority. The saved run snapshot supplies ceilings and source layers
and fields. Current durable records supply usage. The view distinguishes
active concurrency, lifetime agent count, active instances of authorized
background roles, and explicit spawns within the rolling 60-second window.
Source file paths, provider command bindings, and credentials are excluded.
Missing historical provenance remains unknown. Unknown agent states continue
to consume active slots according to the existing admission rule.

The view reports remaining capacity and release behavior. A terminal agent
state releases active concurrency and role slots, while lifetime admissions
remain consumed. Rate slots expire with the rolling window. Task recovery
creates no agent; each fresh continuation or review spawn consumes another
lifetime admission and rate slot. Operation replay consumes neither again.
When two or fewer lifetime admissions remain, the view warns the coordinator
to reserve capacity for recovery and review. Bootstrap and MCP tool guidance
direct callers to inspect the view before planning those launches.

At most 16 authorized background roles appear, with an omitted count. Role
names and policy fields use the existing 512-byte previews. The section
shortens its role list to stay within 32 KiB of compact JSON. An admission
conflict identifies the applicable exhausted field, usage, ceiling, source,
and action even for a role omitted from the view. Observations are advisory:
the supervisor serializes admissions and rechecks ceilings before recording
intent. Reads and mutations retain generation and capability checks.

Waiting for terminal agents or the rolling window can release active or rate
slots. An exhausted lifetime ceiling requires operator action: preserve
recoverable work, stop the run with `coterie stop`, adjust trusted policy if
needed, and launch a new run with `coterie`. Active snapshots cannot be raised
by editing configuration or retrying requests. Retained work must be carried
forward explicitly and still requires review, integration, target validation,
and accepted closure. A caller without spawn authority asks an authorized
coordinator or the operator to launch.

Verification covers:

- `supervisor::admission::tests`: initial capacity, capability-based role
  visibility, stale and revoked credentials, foreground generation replacement,
  unknown lifecycle states,
  unbounded role capacity, missing historical provenance, large role lists,
  escaping-heavy sources, and rolling-window expiry without replay charges.
- `supervisor::crash_tests::admission_tests`: exited workers release active
  slots while retaining lifetime and rate usage; repeated recovery and spawn
  replay preserve usage and source workspaces; separate rate, role, and
  concurrency conflicts leave rejected tasks open and record no launch intent.
- `admission::concurrent_admissions_recheck_the_same_saved_ceiling`: two CLI
  clients compete for one active slot, exactly one succeeds, and editing
  project policy does not raise the running snapshot's ceiling.
- `mcp::mcp_authenticates_forwards_and_replays_agent_operations`: authenticated
  coordinator discovery and `prime` expose the view and its policy source.
- `cli::context_tests`: generated schema and example agreement, human and JSON
  output, and diagnostics remaining off successful standard output.

Existing recovery, integration, validation, and closure tests remain the
acceptance evidence. Agent or provider exit never accepts a task. Real Codex
coverage remains opt-in, and this work does not resume the closed Basin run.

The full `task check` gate passed, including formatting, Clippy, nextest,
rustdoc, the public documentation build and link checks, dependency audits,
release verification, and Nix evaluation. Real-provider tests were not enabled.

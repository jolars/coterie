# September 23 worker exits

The retained run `cr-01M36CJ6S3PH09F9VXEKTD8JFG` confirms execution timeouts
for both workers in the M6 coordination follow-up. This diagnosis uses the run's
SQLite event stream, session records, process-control records, and configuration
snapshot under `~/.local/state/coterie/runs/`.

| Assignment | Session | Start (UTC) | Timeout control (UTC) | Exit observation |
| --- | --- | --- | --- | --- |
| `ca-01M36FNAC4A27GGYTQXC4WYPF2` | `cs-01M36FNAC4KE96YYXY84XDPH5G` | 06:36:14 | 07:36:14, event 493 | 07:36:14, event 494, process code 1 |
| `ca-01M36JGZR6AZE6AY7DR21R0GJV` | `cs-01M36JGZR6TNN5E68Q62XXH6Z9` | 07:26:18 | 08:26:18, event 718 | 08:26:18, event 719, process code 1 |

Both `session_controls` rows have reason `execution_timeout` and reached the
`completed` phase. Their request timestamps are exactly 3,600 seconds after
their session creation timestamps. The saved run snapshot sets
`job_timeout_seconds = 3600`, `interrupt_grace_ms = 250`, and
`shutdown_timeout_ms = 5000`. Each lifecycle event records a process exit with
code 1. The evidence establishes that the supervisor requested timeout control
and observed each process exit immediately afterward. It does not identify which
signal or provider behavior produced code 1 within that second.

The snapshot records `providers.codex.command = ["codex"]`. The Coterie
transcripts identify Codex threads
`01a0ccfa-aa6f-7111-95c6-b2ba265fefb9` and
`01a0cd28-7ffb-76b0-a4a0-2ae6f5350626`, respectively. Their matching
`~/.codex/sessions/2026/09/23/rollout-*.jsonl` files each record
`cli_version = 0.154.0` in the first `session_meta` entry and name the exact
assignment worktree as `cwd`. This identifies the provider version for both
sessions; it does not establish a provider defect. The separately recorded
Coterie executable hash identifies Coterie, not Codex.

The lead's denied `events` and `status` requests are consistent with the
operator-only authorization of those commands. The agent-readable progress
projection now supplies deadlines, warnings, and exit observations while a
worker is waiting on review or a commit.

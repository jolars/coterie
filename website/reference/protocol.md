# JSON, retries, and exit codes

Use the global `--json` option on public subcommands to receive versioned machine-readable output. Successful JSON goes to standard output; diagnostics go to standard error. Interactive `coterie` launch cannot use `--json` because the provider owns its terminal. `events --follow` and `logs --follow` emit one envelope per page.

## Response envelopes

Every response has `schema_version: 1`. A read-only success places its result under `data`:

```json
{"schema_version":1,"data":{"status":"active"}}
```

A mutation success includes its operation ID:

```json
{"schema_version":1,"operation_id":"co-01ARZ3NDEKTSV4RRFFQ69G5FAV","data":{"task_id":"ct-01ARZ3NDEKTSV4RRFFQ69G5FAV"}}
```

Errors place a stable code and readable message under `error`, with `details` when available:

```json
{"schema_version":1,"error":{"code":"invalid_argument","message":"task ID is invalid","details":{"argument":"task_id"}}}
```

Branch on `error.code`, not message text. The typed envelope schemas are [success](/schemas/cli-success-v1.schema.json), [mutation success](/schemas/cli-mutation-success-v1.schema.json), [error](/schemas/cli-error-v1.schema.json), and [mutation error](/schemas/cli-mutation-error-v1.schema.json). Command-specific schemas, including [progress](/schemas/cli-progress-v1.schema.json), [prime](/schemas/cli-prime-v1.schema.json), [logs](/schemas/cli-logs-v1.schema.json), and [run recovery](/schemas/cli-run-recover-v1.schema.json), are available under `/schemas/`.

## Operation IDs and retries

Mutating run commands accept `--operation-id <co-ULID>`. If omitted, Coterie allocates one before dispatch and returns it in its response. When a result is uncertain, retry with the **same ID and identical arguments**. Coterie returns the recorded outcome without repeating a completed side effect. A changed request under the same ID conflicts. Read-only commands do not use operation IDs. `config lock` is a local atomic file operation and has no run operation ID.

An integration retry keeps its original plan. If the target changed independently, Coterie refuses to silently replan. A rejected preflight may require correcting the state and using a new ID; the command diagnostic describes whether the original ID can be retried.

## Paged reads

`progress`, `inbox`, `events`, and `logs` return cursors. Pass each command's returned cursor unchanged for the next page; cursor ownership and meaning differ between commands. `task show` and `assignment show` return UTF-8 text pages for a JSON document. Concatenate `data.text` and retain the returned `revision` while paging. A revision conflict means the document changed; start again at offset zero.

`progress` also reports current background-job deadlines and remaining seconds
from the saved run policy. A warning appears shortly before a job reaches its
limit. Complete pending review and commit handoff while the worker can still
submit. The progress stream records timeout-control reasons and observed exit
codes separately from task submission. Missing exit evidence remains unknown;
an exit never accepts a task.

Read-only polling never acknowledges inbox messages. An authenticated agent uses `inbox ack` for messages it has handled. Provider output, task submission, and accepted task closure remain distinct observations.

## Exit codes

| Code | Category | Meaning |
| ---: | --- | --- |
| 0 | `success` | Command completed. |
| 1 | `internal` | Internal failure or corrupt state. |
| 2 | `usage` | Invalid argument or request value. |
| 3 | `configuration` | Invalid or incompatible configuration. |
| 4 | `not_found` | Requested resource does not exist. |
| 5 | `conflict` | Current state fails an operation precondition. |
| 6 | `permission` | Authentication or authorization failed. |
| 7 | `unavailable` | Required service or provider cannot respond. |

The stable error codes are `invalid_argument`, `invalid_configuration`, `not_found`, `conflict`, `unauthenticated`, `permission_denied`, `unavailable`, `corrupt_state`, and `internal`. The [generated exit-code table](https://github.com/jolars/coterie/blob/main/tests/golden/cli-exit-codes-v1.json) is the machine-readable contract. The [detailed CLI contract](https://github.com/jolars/coterie/blob/main/docs/cli-contract.md) covers command-specific fields and failure behavior.

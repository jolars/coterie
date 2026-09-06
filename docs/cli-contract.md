# CLI contract

This document defines version 1 of Coterie's programmatic output and process
exit codes. The binary remains behavior-free until later milestones implement
commands, but those commands must use this contract.

## JSON output

Programmatic commands selected with `--json` emit exactly one compact JSON
object followed by a newline. A successful response goes to standard output,
and standard error remains empty. A failed response goes to standard error,
and standard output remains empty. Human-readable diagnostics also go to
standard error, but do not share a stream with successful JSON.

Every response contains `"schema_version": 1`. A read-only success places its
command-specific result under `data`:

```json
{"schema_version":1,"data":{"status":"active"}}
```

A mutation success also contains the operation ID:

```json
{"schema_version":1,"operation_id":"co-01ARZ3NDEKTSV4RRFFQ69G5FAV","data":{"task_id":"ct-01ARZ3NDEKTSV4RRFFQ69G5FAV"}}
```

An error places its stable code, human-readable message, and any
error-specific fields under `error`. `details` is omitted when empty:

```json
{"schema_version":1,"error":{"code":"invalid_argument","message":"task ID is invalid","details":{"argument":"task_id"}}}
```

Once a mutation has an operation ID, its error response includes that ID at the
top level. Callers must branch on `error.code`, not on the message.

The generated JSON Schemas are:

- [`cli-success-v1.schema.json`](../schemas/cli-success-v1.schema.json)
- [`cli-mutation-success-v1.schema.json`](../schemas/cli-mutation-success-v1.schema.json)
- [`cli-error-v1.schema.json`](../schemas/cli-error-v1.schema.json)
- [`cli-mutation-error-v1.schema.json`](../schemas/cli-mutation-error-v1.schema.json)

## Operation IDs

Every mutating CLI command accepts the common
`--operation-id <co-ULID>` option. If it is omitted, the CLI generates an
operation ID before dispatch. The RPC request carries that ID, and every
response after allocation returns it. A programmatic caller retries an
uncertain mutation with the same ID. Read-only commands neither accept nor
return an operation ID.

Errors detected before an operation ID can be parsed or allocated use the
ordinary error envelope without `operation_id`.

## Exit codes

Exit codes describe broad handling categories; `error.code` supplies the
specific machine-readable cause.

| Code | Category | Meaning |
| ---: | --- | --- |
| 0 | `success` | The command completed successfully. |
| 1 | `internal` | Coterie encountered an internal failure or corrupt state. |
| 2 | `usage` | A command-line argument or request value was invalid. |
| 3 | `configuration` | Configuration was invalid or incompatible. |
| 4 | `not_found` | A requested resource does not exist. |
| 5 | `conflict` | Current state does not satisfy an operation precondition. |
| 6 | `permission` | Authentication or authorization failed. |
| 7 | `unavailable` | A required service or provider cannot currently respond. |

The versioned machine-readable table is
[`cli-exit-codes-v1.json`](../tests/golden/cli-exit-codes-v1.json).

The version 1 error codes map as follows:

| Error code | Exit category |
| --- | --- |
| `invalid_argument` | `usage` |
| `invalid_configuration` | `configuration` |
| `not_found` | `not_found` |
| `conflict` | `conflict` |
| `unauthenticated` | `permission` |
| `permission_denied` | `permission` |
| `unavailable` | `unavailable` |
| `corrupt_state` | `internal` |
| `internal` | `internal` |

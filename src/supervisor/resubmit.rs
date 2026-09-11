//! Explicit submission recovery without moving writable ownership.

use super::*;
use crate::state::resubmit::Resubmission;

#[allow(clippy::too_many_arguments)]
pub(super) fn resubmit_task<B: WorkspaceBackend>(
    store: &mut Store,
    workspaces: &WorkspaceSupervisor<B>,
    run_id: RunId,
    caller: &AuthenticatedCaller,
    operation_id: OperationId,
    submission: Resubmission,
    fingerprint: Option<&str>,
) -> Result<RpcResponse, RpcFailure> {
    require_current_caller(store, run_id, caller)?;
    require_capability(store, run_id, caller, "task", "resubmit")?;
    if submission.summary.trim().is_empty()
        || submission.reason.trim().is_empty()
    {
        return Err(invalid_argument(
            "resubmission requires a nonempty --summary and --reason",
        ));
    }
    for commit in [&submission.expected_result, &submission.result_commit] {
        if commit.len() != 40
            || !commit.bytes().all(|byte| {
                byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)
            })
        {
            return Err(invalid_argument(
                "--expected-result and --result require full lowercase Git commit IDs",
            ));
        }
    }
    if submission.expected_result == submission.result_commit {
        return Err(invalid_argument(
            "--result must differ from --expected-result; use the original operation ID to retry an uncertain resubmission",
        ));
    }
    let mutation = Mutation {
        id: operation_id,
        run_id,
        kind: "task.resubmit".to_owned(),
        actor_agent_id: caller.agent_id(),
        request: serde_json::to_value(&submission)
            .map_err(|error| rpc_state_failure(error.into()))?,
        created_at: rpc_timestamp()?,
    };
    let existing = store
        .transaction(|r| r.operation(operation_id))
        .map_err(rpc_state_failure)?;
    let prepared = if existing.is_none() {
        let (assignment, _, _) = store
            .transaction(|r| r.resubmission_preflight(run_id, &submission))
            .map_err(rpc_state_failure)?;
        workspaces
            .validate_resubmission(
                store,
                assignment.scope(),
                &submission.expected_result,
                &submission.result_commit,
            )
            .map_err(rpc_workspace_failure)?;
        Some(task_by_id(store, run_id, assignment.task_id)?)
    } else {
        None
    };
    let result = store
        .mutate_with_fingerprint(&mutation, fingerprint, |r| {
            let mut task =
                prepared.ok_or_else(|| StoreError::OperationIncomplete {
                    id: operation_id,
                    status: "missing resubmission preflight".to_owned(),
                })?;
            let (previous_result, current_result) =
                r.apply_resubmission(&mutation, &submission)?;
            task.result = Some(current_result);
            Ok(RpcResponse::TaskResubmitted {
                operation_id,
                submission: crate::protocol::ResubmissionSummary {
                    assignment_id: submission.assignment_id,
                    previous_result,
                    task,
                },
            })
        })
        .map_err(rpc_state_failure)?;
    Ok(mutation_value(result))
}

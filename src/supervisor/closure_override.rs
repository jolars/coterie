//! Read-only preconditions for explicit operator acceptance.

use super::*;
use crate::protocol::ClosureOverrideRequest;
use crate::state::{
    AssignmentRecord, OperatorClosureOverride, TaskRecord, WorkspaceRecord,
};

#[allow(clippy::too_many_arguments)]
pub(super) fn validate<B: WorkspaceBackend>(
    store: &mut Store,
    workspaces: &WorkspaceSupervisor<B>,
    run_id: RunId,
    task_id: TaskId,
    task: Option<&TaskRecord>,
    assignment: Option<&AssignmentRecord>,
    workspace: Option<&WorkspaceRecord>,
    request: &ClosureOverrideRequest,
    summary: &str,
) -> Result<OperatorClosureOverride, RpcFailure> {
    if request.reason.trim().is_empty() {
        return Err(invalid_argument("override reason cannot be empty"));
    }
    for (field, value) in [
        ("result", &request.result_commit),
        ("target", &request.target_commit),
    ] {
        if value.len() != 40
            || !value.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(invalid_argument(format!(
                "override {field} commit must be a full 40-digit Git object ID"
            )));
        }
    }
    let task = task
        .filter(|task| task.run_id == run_id)
        .ok_or_else(|| not_found(format!("task `{task_id}` does not exist")))?;
    if task.status != TaskStatus::Submitted {
        return Err(conflict(format!(
            "task `{task_id}` must be submitted before an operator closure override"
        )));
    }
    let assignment = assignment
        .filter(|assignment| {
            assignment.id == request.assignment_id
                && assignment.run_id == run_id
                && assignment.task_id == task_id
                && assignment.state == "completed"
                && assignment.completed_at.is_some()
        })
        .ok_or_else(|| {
            conflict(
                "override must name the task's latest completed assignment",
            )
        })?;
    let workspace = workspace.filter(|workspace| workspace.kind == "worktree"
        && workspace.state == ExternalResourceState::Observed
        && workspace.target_commit.is_none())
        .ok_or_else(|| conflict("override requires an observed, unintegrated Git worktree; use ordinary `coterie task close` for integrated work"))?;
    let result_commit = request.result_commit.to_ascii_lowercase();
    if workspace.result_commit.as_deref() != Some(result_commit.as_str()) {
        return Err(conflict(
            "override result commit must match the recorded submission; inspect `coterie prime` before retrying",
        ));
    }
    let pending = store
        .transaction(|repositories| {
            Ok(repositories
                .operations_requiring_reconciliation(run_id)?
                .iter()
                .any(|operation| {
                    operation.kind == "workspace.integrate"
                        && operation.request.get("assignment_id")
                            == Some(&json!(assignment.id))
                }))
        })
        .map_err(rpc_state_failure)?;
    if pending {
        return Err(conflict(
            "the assignment has a pending integration; reconcile it before overriding closure",
        ));
    }
    let target_commit = request.target_commit.to_ascii_lowercase();
    let target_reference = workspaces
        .validate_external_closure(store, assignment.scope(), &target_commit)
        .map_err(rpc_workspace_failure)?;
    Ok(OperatorClosureOverride {
        assignment_id: assignment.id,
        project_id: task.project_id,
        base_commit: workspace.base_commit.clone().ok_or_else(|| {
            conflict("the assignment has no recorded base commit")
        })?,
        result_commit,
        target_reference,
        target_commit,
        reason: request.reason.clone(),
        validation_summary: summary.to_owned(),
    })
}

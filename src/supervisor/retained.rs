//! Capability-checked submission without starting a replacement worker.

use super::*;
use crate::state::retained::RetainedSubmission;

#[allow(clippy::too_many_arguments)]
pub(super) fn submit_retained<P: Provider, B: WorkspaceBackend>(
    store: &mut Store,
    sessions: &mut AgentSessionSupervisor<P>,
    workspaces: &WorkspaceSupervisor<B>,
    run_id: RunId,
    caller: &AuthenticatedCaller,
    operation_id: OperationId,
    submission: RetainedSubmission,
    fingerprint: Option<&str>,
) -> Result<RpcResponse, RpcFailure> {
    require_current_caller(store, run_id, caller)?;
    require_capability(store, run_id, caller, "task", "submit-retained")?;
    if [
        &submission.summary,
        &submission.reason,
        &submission.review.text,
        &submission.review.source,
    ]
    .iter()
    .any(|value| value.trim().is_empty())
    {
        return Err(invalid_argument(
            "retained submission requires a summary, reason, independent review, and review source",
        ));
    }
    if submission.result_commit.len() != 40
        || !submission
            .result_commit
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(invalid_argument(
            "--result requires a full lowercase Git commit ID",
        ));
    }
    let mutation = Mutation {
        id: operation_id,
        run_id,
        kind: "task.submit-retained".into(),
        actor_agent_id: caller.agent_id(),
        request: serde_json::to_value(&submission)
            .map_err(|error| rpc_state_failure(error.into()))?,
        created_at: rpc_timestamp()?,
    };
    let existing = store
        .transaction(|r| r.operation(operation_id))
        .map_err(rpc_state_failure)?;
    let prepared = if existing.is_none() {
        let (assignment, _) = store
            .transaction(|r| {
                r.interrupted_assignment_preflight(
                    run_id,
                    submission.assignment_id,
                    false,
                    Some(&submission.result_commit),
                )
            })
            .map_err(rpc_state_failure)?;
        if caller.agent_id() == Some(assignment.agent_id) {
            return Err(RpcFailure::new(
                RpcFailureCode::PermissionDenied,
                "retained submission requires an independent caller",
            ));
        }
        let scope = crate::auth::SessionScope {
            run_id,
            agent_id: assignment.agent_id,
            session_id: assignment
                .session_id
                .expect("preflight requires a session"),
            generation: assignment.generation,
        };
        if !sessions
            .verify_recovery_exit(store, scope)
            .map_err(rpc_session_failure)?
        {
            return Err(conflict(
                "the provider cannot verify process inactivity; preserve the worktree and inspect `coterie doctor`",
            ));
        }
        crate::fault::point("retained.inspect.before");
        let observed = workspaces
            .observe_result_commit(store, assignment.scope())
            .map_err(rpc_workspace_failure)?;
        if observed.as_deref() != Some(&submission.result_commit) {
            return Err(conflict(
                "the clean owned worktree tip must equal --result; independently review the current commit before retrying `task submit-retained`",
            ));
        }
        crate::fault::point("retained.inspect.after");
        Some(task_by_id(store, run_id, assignment.task_id)?)
    } else {
        None
    };
    let response = store
        .mutate_with_fingerprint(&mutation, fingerprint, |r| {
            let mut task =
                prepared.ok_or_else(|| StoreError::OperationIncomplete {
                    id: operation_id,
                    status: "missing retained submission preflight".into(),
                })?;
            task.result = Some(r.submit_retained(&mutation, &submission)?);
            task.status = TaskStatus::Submitted;
            task.ready = false;
            Ok(RpcResponse::TaskRetainedSubmitted {
                operation_id,
                submission: crate::protocol::RetainedSubmissionSummary {
                    assignment_id: submission.assignment_id,
                    task,
                },
            })
        })
        .map_err(rpc_state_failure)?;
    Ok(mutation_value(response))
}

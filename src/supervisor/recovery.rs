//! Recovery admission and replay without transferring workspace ownership.

use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) fn recover_task<P: Provider, B: WorkspaceBackend>(
    store: &mut Store,
    sessions: &mut AgentSessionSupervisor<P>,
    workspaces: &WorkspaceSupervisor<B>,
    run_id: RunId,
    caller: &AuthenticatedCaller,
    operation_id: OperationId,
    assignment_id: AssignmentId,
    reason: String,
    fingerprint: Option<&str>,
) -> Result<RpcResponse, RpcFailure> {
    require_current_caller(store, run_id, caller)?;
    require_capability(store, run_id, caller, "task", "recover")?;
    if reason.trim().is_empty() {
        return Err(invalid_argument("recovery requires a nonempty --reason"));
    }
    let mutation = Mutation {
        id: operation_id,
        run_id,
        kind: "task.recover".to_owned(),
        actor_agent_id: caller.agent_id(),
        request: json!({"assignment_id": assignment_id, "reason": reason}),
        created_at: rpc_timestamp()?,
    };
    let existing = store
        .transaction(|r| r.operation(operation_id))
        .map_err(rpc_state_failure)?;
    if existing.is_none() {
        let (assignment, _) = store
            .transaction(|r| r.recovery_preflight(run_id, assignment_id))
            .map_err(rpc_state_failure)?;
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
                "the provider cannot verify process inactivity; preserve the workspace and inspect `coterie doctor` before retrying `coterie task recover`",
            ));
        }
        if workspaces
            .observe_source(store, assignment.scope())
            .map_err(|error| {
                let mut failure = rpc_workspace_failure(error);
                failure.message.push_str("; preserve the source and inspect `coterie doctor` before retrying `coterie task recover`");
                failure
            })?
            != ExternalResourceState::Observed
        {
            return Err(conflict(
                "recovery source ownership is uncertain; preserve the worktree and inspect `coterie doctor`",
            ));
        }
    }
    let outcome = store
        .mutate_with_fingerprint(&mutation, fingerprint, |r| {
            let recovery =
                r.recover_assignment(&mutation, assignment_id, &reason)?;
            Ok(RpcResponse::TaskRecovered {
                operation_id,
                recovery,
            })
        })
        .map_err(rpc_state_failure)?;
    Ok(mutation_value(outcome))
}

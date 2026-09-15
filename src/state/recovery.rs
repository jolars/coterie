//! Explicit retirement and continuation links, stored in immutable events.

use super::*;
use crate::protocol::RecoverySummary;
use crate::protocol::recovery::RecoveryHandoff;

impl Repositories<'_, '_> {
    pub(crate) fn recovery_preflight(
        &self,
        run_id: RunId,
        assignment_id: AssignmentId,
    ) -> Result<(AssignmentRecord, WorkspaceRecord), StoreError> {
        let reject = |reason: &str| StoreError::RecoveryConflict {
            assignment_id,
            reason: reason.to_owned(),
        };
        if !self.run(run_id)?.is_some_and(|run| run.status == "active")
            || self.run_shutdown(run_id)?.is_some()
        {
            return Err(reject(
                "the run is stopped or draining; preserve the workspace and inspect `coterie doctor`",
            ));
        }
        let assignment = self
            .assignment(assignment_id)?
            .filter(|assignment| assignment.run_id == run_id)
            .ok_or_else(|| {
                reject(
                    "assignment not found in this run; inspect `coterie prime`",
                )
            })?;
        let task = self
            .task(assignment.task_id)?
            .ok_or_else(|| reject("task missing; inspect `coterie doctor`"))?;
        if task.status != TaskStatus::InProgress
            || assignment.completed_at.is_some()
            || !matches!(assignment.state.as_str(), "active" | "draining")
            || self
                .active_assignment_for_task(run_id, task.id)?
                .map(|active| active.id)
                != Some(assignment_id)
            || !self.claim(assignment.claim_id)?.is_some_and(|claim| {
                claim.run_id == run_id
                    && claim.task_id == task.id
                    && claim.agent_id == assignment.agent_id
                    && claim.released_at.is_none()
            })
        {
            return Err(reject(
                "the assignment is no longer active; inspect `coterie prime`; submitted work uses `coterie task resubmit`",
            ));
        }
        let session = assignment
            .session_id
            .map(|id| self.session(id))
            .transpose()?
            .flatten()
            .ok_or_else(|| {
                reject("no observed session exit; inspect `coterie doctor`")
            })?;
        let scope = crate::auth::SessionScope {
            run_id,
            agent_id: assignment.agent_id,
            session_id: session.id,
            generation: assignment.generation,
        };
        if !self.session_scope_is_current(scope)?
            || session.state != LifecycleState::Exited
            || session.reconciliation_state != ExternalResourceState::Observed
            || session.ended_at.is_none()
            || session.provider_session_id.is_none()
            || !self
                .agent(assignment.agent_id)?
                .is_some_and(|agent| agent.state == LifecycleState::Exited)
            || !self
                .session_credential(session.id)?
                .is_some_and(|credential| credential.revoked_at.is_some())
        {
            return Err(reject(
                "process inactivity is not verified for the current generation; wait for an observed exit and inspect `coterie doctor` before retrying `coterie task recover`",
            ));
        }
        let pending: bool = self.transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM session_controls WHERE session_id = ?1 AND phase <> 'completed')
                OR EXISTS(SELECT 1 FROM operations WHERE run_id = ?2 AND kind = 'agent.spawn'
                    AND json_extract(result_json, '$.assignment_id') = ?3
                    AND (status <> 'succeeded' OR reconciliation_state IS NOT 'observed'))
                OR EXISTS(SELECT 1 FROM operations WHERE run_id = ?2 AND kind = 'workspace.integrate'
                    AND json_extract(request_json, '$.assignment_id') = ?3)",
            params![session.id, run_id, assignment_id], |row| row.get(0),
        )?;
        if pending {
            return Err(reject(
                "launch, control, or integration intent is unresolved; inspect `coterie doctor` and reconcile the original operation before `coterie task recover`",
            ));
        }
        let exit_recorded: bool = self.transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM events WHERE run_id = ?1
                AND event_type = 'session.lifecycle_changed' AND actor = 'provider' AND subject = ?2
                AND json_extract(payload_json, '$.data.generation') = ?3
                AND json_extract(payload_json, '$.data.state') = 'exited'
                AND json_extract(payload_json, '$.data.exit.reason') IN ('process', 'interrupted', 'terminated'))",
            params![run_id, session.id, session.generation], |row| row.get(0),
        )?;
        if !exit_recorded
            || session.process_owner != SessionProcessOwner::Supervisor
        {
            return Err(reject(
                "no verified provider process-exit record; preserve the worktree and inspect `coterie doctor`",
            ));
        }
        let workspace = self.workspace_for_scope(assignment.scope())?;
        if workspace.kind != "worktree"
            || workspace.state != ExternalResourceState::Observed
            || workspace.result_commit.is_some()
            || workspace.target_commit.is_some()
        {
            return Err(reject(
                "an observed, unsubmitted Git worktree is required; inspect `coterie doctor`; submitted work uses `coterie task resubmit`",
            ));
        }
        Ok((assignment, workspace))
    }

    pub(crate) fn recover_assignment(
        &self,
        mutation: &Mutation,
        assignment_id: AssignmentId,
        reason: &str,
        handoff: &RecoveryHandoff,
    ) -> Result<RecoverySummary, StoreError> {
        let (assignment, workspace) =
            self.recovery_preflight(mutation.run_id, assignment_id)?;
        let recovery = RecoverySummary {
            task_id: assignment.task_id,
            assignment_id,
            session_id: assignment
                .session_id
                .expect("preflight requires a session"),
            project_id: workspace.project_id,
            generation: assignment.generation,
            workspace_path: workspace.path.to_string_lossy().into_owned(),
            workspace_path_bytes: workspace
                .path
                .as_os_str()
                .as_bytes()
                .to_vec(),
            base_commit: workspace.base_commit,
            reason: reason.to_owned(),
            continuation_assignment_id: None,
            handoff: Some(Box::new(handoff.brief())),
        };
        crate::fault::point("recovery.handoff.record.before");
        self.transaction.execute(
            "INSERT INTO recovery_handoffs (assignment_id, run_id, operation_id, document_json) VALUES (?1, ?2, ?3, ?4)",
            params![assignment_id, mutation.run_id, mutation.id, serde_json::to_string(handoff)?],
        )?;
        crate::fault::point("recovery.handoff.record.after");
        self.apply_task_transition(&TaskTransitionMutation {
            operation_id: mutation.id,
            run_id: mutation.run_id,
            actor_agent_id: mutation.actor_agent_id,
            task_id: assignment.task_id,
            transition: TaskTransition::Reopen,
            result: None,
            operator_override: None,
            summary: None,
            transitioned_at: mutation.created_at,
        })?;
        for (kind, subject, data) in [
            (
                EventKind::TaskRecovered,
                assignment.task_id.to_string(),
                serde_json::to_value(&recovery)?,
            ),
            (
                EventKind::TaskLifecycleChanged,
                assignment.task_id.to_string(),
                json!({"previous_status": "in_progress", "status": "open", "transition": "recover"}),
            ),
            (
                EventKind::AssignmentLifecycleChanged,
                assignment.id.to_string(),
                json!({"previous_state": assignment.state, "state": "released"}),
            ),
            (
                EventKind::ClaimReleased,
                assignment.claim_id.to_string(),
                json!({"claim_id": assignment.claim_id, "assignment_id": assignment.id}),
            ),
        ] {
            self.append_event(&NewEvent {
                run_id: mutation.run_id, kind, actor: mutation_actor(mutation.actor_agent_id),
                subject, project_id: Some(workspace.project_id), agent_id: Some(assignment.agent_id),
                task_id: Some(assignment.task_id), operation_id: Some(mutation.id),
                correlation_id: None, causation_id: None, data,
                summary: format!("Retired interrupted assignment {} for continuation; source worktree preserved.", assignment.id),
                created_at: mutation.created_at,
            })?;
        }
        Ok(recovery)
    }

    pub(crate) fn recovery_handoff(
        &self,
        run_id: RunId,
        assignment_id: AssignmentId,
    ) -> Result<Option<RecoveryHandoff>, StoreError> {
        let document: Option<String> = self.transaction.query_row(
            "SELECT document_json FROM recovery_handoffs WHERE run_id = ?1 AND assignment_id = ?2",
            params![run_id, assignment_id], |row| row.get(0),
        ).optional()?;
        document
            .map(|text| serde_json::from_str(&text).map_err(StoreError::from))
            .transpose()
    }

    pub(crate) fn task_recoveries(
        &self,
        run_id: RunId,
    ) -> Result<Vec<RecoverySummary>, StoreError> {
        let mut statement = self.transaction.prepare(
            "SELECT json_extract(source.payload_json, '$.data'),
                (SELECT subject FROM events AS continuation WHERE continuation.run_id = source.run_id
                    AND continuation.event_type = 'assignment.continued'
                    AND json_extract(continuation.payload_json, '$.data.previous_assignment_id') = json_extract(source.payload_json, '$.data.assignment_id')
                    ORDER BY sequence LIMIT 1)
             FROM events AS source WHERE run_id = ?1 AND event_type = 'task.recovered' ORDER BY sequence",
        )?;
        let rows = statement.query_map([run_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<AssignmentId>>(1)?,
            ))
        })?;
        rows.map(|row| {
            let (source, continuation) = row?;
            let mut recovery: RecoverySummary = serde_json::from_str(&source)?;
            recovery.continuation_assignment_id = continuation;
            Ok(recovery)
        })
        .collect()
    }

    pub(crate) fn pending_recovery(
        &self,
        run_id: RunId,
        task_id: TaskId,
    ) -> Result<Option<RecoverySummary>, StoreError> {
        Ok(self
            .task_recoveries(run_id)?
            .into_iter()
            .rev()
            .find(|source| {
                source.task_id == task_id
                    && source.continuation_assignment_id.is_none()
            }))
    }

    pub(crate) fn link_continuation(
        &self,
        claim: &ClaimTaskMutation,
    ) -> Result<(), StoreError> {
        if let Some(source) =
            self.pending_recovery(claim.run_id, claim.task_id)?
        {
            self.append_event(&NewEvent {
                run_id: claim.run_id,
                kind: EventKind::AssignmentContinued,
                actor: mutation_actor(claim.actor_agent_id),
                subject: claim.assignment_id.to_string(),
                project_id: Some(source.project_id),
                agent_id: Some(claim.agent_id),
                task_id: Some(claim.task_id),
                operation_id: Some(claim.operation_id),
                correlation_id: None,
                causation_id: None,
                data: json!({"previous_assignment_id": source.assignment_id}),
                summary: format!(
                    "Assignment {} continues preserved assignment {}.",
                    claim.assignment_id, source.assignment_id
                ),
                created_at: claim.claimed_at,
            })?;
        }
        Ok(())
    }
}

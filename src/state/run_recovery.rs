//! Same-run reactivation preserves historical records and revocation fences.

use super::*;
use crate::protocol::run_recovery::RunRecoveryReport;

impl Repositories<'_, '_> {
    /// A historical stop remains replayable without stopping its continuation.
    pub(crate) fn recovered_shutdown_result(
        &self,
        run_id: RunId,
        operation_id: OperationId,
    ) -> Result<Option<crate::protocol::RpcResponse>, StoreError> {
        let recovered: bool = self.transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM events WHERE run_id = ?1 AND event_type = 'run.recovered'
                AND json_extract(payload_json, '$.data.recovery.previous_stop_operation_id') = ?2)",
            params![run_id, operation_id], |row| row.get(0),
        )?;
        if !recovered {
            return Ok(None);
        }
        let operation = self
            .operation(operation_id)?
            .filter(|operation| {
                operation.run_id == run_id
                    && operation.kind == "run.stop"
                    && operation.status == "completed"
            })
            .ok_or(StoreError::OperationConflict { id: operation_id })?;
        let result =
            serde_json::from_value(operation.result.ok_or(
                StoreError::MissingOperationResult { id: operation_id },
            )?)?;
        if result
            != (crate::protocol::RpcResponse::ShuttingDown {
                run_id,
                operation_id,
            })
        {
            return Err(StoreError::OperationConflict { id: operation_id });
        }
        Ok(Some(result))
    }

    pub(crate) fn stopped_run_preflight(
        &self,
        run_id: RunId,
        operation_id: OperationId,
    ) -> Result<Vec<(WorkspaceRecord, ProjectRecord)>, StoreError> {
        let reject = |reason: &str| StoreError::RunRecoveryConflict {
            run_id,
            reason: reason.to_owned(),
        };
        if !self.run(run_id)?.is_some_and(|run| {
            run.status == "stopped" && run.stopped_at.is_some()
        }) {
            return Err(reject(
                "the run is active or has no completed stop; reconnect with `coterie` for an active run, or finish `coterie stop` before stopped-run recovery",
            ));
        }
        let shutdown = self
            .run_shutdown(run_id)?
            .ok_or_else(|| reject("completed shutdown evidence is missing"))?;
        if shutdown.phase != supervision::ShutdownPhase::Completed
            || !self.operation(shutdown.operation_id)?.is_some_and(
                |operation| {
                    operation.run_id == run_id
                        && operation.kind == "run.stop"
                        && operation.status == "completed"
                        && operation.reconciliation_state
                            == Some(ExternalResourceState::Observed)
                },
            )
        {
            return Err(reject(
                "shutdown is incomplete; preserve state and finish the original stop operation",
            ));
        }
        let uncertain: bool = self.transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM operations WHERE run_id = ?1 AND id <> ?2
                AND (status = 'pending' OR reconciliation_state IN ('desired', 'unknown')))
             OR EXISTS(SELECT 1 FROM session_controls AS control WHERE control.run_id = ?1
                AND NOT EXISTS(SELECT 1 FROM sessions WHERE id = control.session_id
                    AND run_id = control.run_id AND agent_id = control.agent_id AND generation = control.generation))
             OR EXISTS(SELECT 1 FROM session_controls AS control WHERE control.run_id = ?1 AND phase <> 'completed'
                AND NOT EXISTS(SELECT 1 FROM agents WHERE id = control.agent_id AND run_id = control.run_id AND generation = control.generation))
             OR EXISTS(SELECT 1 FROM workspaces WHERE run_id = ?1 AND state IN ('desired', 'unknown'))
             OR EXISTS(SELECT 1 FROM assignments WHERE run_id = ?1 AND completed_at IS NULL
                AND NOT EXISTS(SELECT 1 FROM workspaces WHERE assignment_id = assignments.id))",
            params![run_id, operation_id], |row| row.get(0),
        )?;
        if uncertain {
            return Err(reject(
                "resource intent or ownership remains unresolved; preserve state and inspect `coterie doctor`",
            ));
        }
        for session in self.sessions(run_id)? {
            let recorded_exit: bool = self.transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM events WHERE run_id = ?1 AND subject = ?2
                    AND event_type = 'session.lifecycle_changed'
                    AND json_extract(payload_json, '$.data.generation') = ?3
                    AND json_extract(payload_json, '$.data.state') = 'exited'
                    AND ((actor = 'provider' AND json_extract(payload_json, '$.data.exit.reason') IN ('process', 'interrupted', 'terminated'))
                        OR (actor = 'foreground' AND (json_type(payload_json, '$.data.details.code') = 'integer'
                            OR json_type(payload_json, '$.data.details.signal') = 'integer'))))",
                params![run_id, session.id, session.generation], |row| row.get(0),
            )?;
            if session.state != LifecycleState::Exited
                || !recorded_exit
                || session.ended_at.is_none()
                || session.reconciliation_state
                    != ExternalResourceState::Observed
                || !self
                    .session_credential(session.id)?
                    .is_some_and(|credential| credential.revoked_at.is_some())
            {
                return Err(reject(
                    "process inactivity and credential revocation must be observed for every session; preserve state and inspect `coterie doctor`",
                ));
            }
        }
        let mut retained = Vec::new();
        for workspace in self.workspaces(run_id)? {
            let assignment = self
                .assignment(workspace.assignment_id)?
                .ok_or_else(|| reject("workspace assignment is missing"))?;
            let task = self
                .task(assignment.task_id)?
                .ok_or_else(|| reject("assignment task is missing"))?;
            if assignment.completed_at.is_some()
                && (task.status != TaskStatus::Submitted
                    || self
                        .latest_assignment_for_task(run_id, task.id)?
                        .is_none_or(|latest| latest.id != assignment.id))
            {
                continue;
            }
            if assignment.run_id != run_id
                || assignment.generation != workspace.generation
                || task.run_id != run_id
                || task.project_id != workspace.project_id
                || !self.agent(assignment.agent_id)?.is_some_and(|agent| {
                    agent.run_id == run_id
                        && agent.generation == assignment.generation
                })
                || !assignment
                    .session_id
                    .map(|id| self.session(id))
                    .transpose()?
                    .flatten()
                    .is_some_and(|session| {
                        session.run_id == run_id
                            && session.agent_id == assignment.agent_id
                            && session.generation == assignment.generation
                    })
                || !self.claim(assignment.claim_id)?.is_some_and(|claim| {
                    claim.run_id == run_id
                        && claim.agent_id == assignment.agent_id
                        && claim.task_id == task.id
                        && (assignment.completed_at.is_some()
                            || claim.released_at.is_none())
                })
                || workspace.state != ExternalResourceState::Observed
            {
                return Err(reject(
                    "assignment ownership is inconsistent; preserve the workspace and inspect `coterie doctor`",
                ));
            }
            let project = self
                .project(workspace.project_id)?
                .ok_or_else(|| reject("workspace project is missing"))?;
            if project.run_id != run_id {
                return Err(reject("workspace project belongs to another run"));
            }
            retained.push((workspace, project));
        }
        Ok(retained)
    }

    pub(crate) fn reactivate_run(
        &self,
        mutation: &Mutation,
    ) -> Result<RunRecoveryReport, StoreError> {
        self.stopped_run_preflight(mutation.run_id, mutation.id)?;
        let run = self
            .run(mutation.run_id)?
            .expect("preflight verified the run");
        let shutdown = self
            .run_shutdown(mutation.run_id)?
            .expect("preflight verified shutdown");
        let result = RunRecoveryReport {
            run_id: mutation.run_id,
            operation_id: mutation.id,
            previous_stop_operation_id: shutdown.operation_id,
            previous_stopped_at: run.stopped_at.expect("preflight verified stop time"),
            recovered_at: mutation.created_at,
            next_step: "Launch `coterie` for a fresh session; inspect `prime` and use `task recover` before spawning unfinished task continuations.".to_owned(),
        };
        self.transaction.execute("UPDATE runs SET status = 'active', stopped_at = NULL WHERE id = ?1", [mutation.run_id])?;
        // A synchronous exit can complete shutdown before the next control poll.
        // Admission and fresh provider inspection establish its terminal outcome.
        for control in self.session_controls(mutation.run_id)? {
            if control.phase != supervision::ControlPhase::Completed {
                self.set_control_phase(
                    &control,
                    supervision::ControlPhase::Completed,
                    mutation.created_at.saturating_mul(1000),
                )?;
            }
        }
        self.append_event(&NewEvent {
            run_id: mutation.run_id, kind: EventKind::RunRecovered,
            actor: "operator".to_owned(), subject: mutation.run_id.to_string(),
            project_id: None, agent_id: None, task_id: None,
            operation_id: Some(mutation.id), correlation_id: None, causation_id: None,
            data: json!({"reason": mutation.request["reason"], "recovery": result,
                "previous_shutdown": {"operation_id": shutdown.operation_id, "phase": shutdown.phase,
                    "requested_at_ms": shutdown.requested_at_ms, "interrupt_until_ms": shutdown.interrupt_until_ms,
                    "deadline_ms": shutdown.deadline_ms}}),
            summary: "Reactivated stopped run; historical sessions remain revoked and assignments retain their ownership.".to_owned(),
            created_at: mutation.created_at,
        })?;
        self.transaction.execute(
            "DELETE FROM run_shutdowns WHERE run_id = ?1",
            [mutation.run_id],
        )?;
        crate::fault::point("run.recover.state_written");
        Ok(result)
    }
}

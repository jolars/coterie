//! Durable restart admission, deadlines, and phased process control.

use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{
    EventKind, ExternalResourceState, NewEvent, OperationRecord, Repositories,
    StoreError,
};
use crate::auth::SessionScope;
use crate::id::{OperationId, RunId};

#[derive(
    Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize,
)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ShutdownPhase {
    Interrupting,
    Terminating,
    Reconciling,
    TimedOut,
    Completed,
}

#[derive(
    Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize,
)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ControlPhase {
    Interrupt,
    Terminate,
    Kill,
    TimedOut,
    Completed,
}

#[derive(
    Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize,
)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ControlReason {
    Shutdown,
    StartupTimeout,
    ExecutionTimeout,
}

fn encoded<T: Serialize>(value: T) -> String {
    serde_json::to_value(value)
        .expect("a supervision enum is serializable")
        .as_str()
        .expect("a supervision enum is a string")
        .to_owned()
}

fn decode<T: serde::de::DeserializeOwned>(
    row: &rusqlite::Row<'_>,
    index: usize,
) -> rusqlite::Result<T> {
    let value: String = row.get(index)?;
    serde_json::from_value(json!(value)).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RunShutdown {
    pub(crate) operation_id: OperationId,
    pub(crate) phase: ShutdownPhase,
    pub(crate) requested_at_ms: i64,
    pub(crate) interrupt_until_ms: i64,
    pub(crate) deadline_ms: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SessionControl {
    pub(crate) scope: SessionScope,
    pub(crate) reason: ControlReason,
    pub(crate) phase: ControlPhase,
    pub(crate) delivered: bool,
    pub(crate) requested_at_ms: i64,
    pub(crate) interrupt_until_ms: i64,
    pub(crate) kill_at_ms: i64,
    pub(crate) deadline_ms: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LaunchAdmission {
    Allowed,
    Backoff { until: i64 },
    Quarantined,
    Unknown,
}

impl Repositories<'_, '_> {
    /// Returns an event cursor only when inactivity is positively established.
    pub(crate) fn idle_shutdown_cursor(
        &self,
        run_id: RunId,
    ) -> Result<Option<i64>, StoreError> {
        let (eligible, cursor): (bool, i64) = self.transaction.query_row(
            "SELECT
                EXISTS(SELECT 1 FROM runs WHERE id = ?1 AND status = 'active')
                AND NOT EXISTS(SELECT 1 FROM run_shutdowns WHERE run_id = ?1)
                AND NOT EXISTS(SELECT 1 FROM sessions WHERE run_id = ?1
                    AND (state <> 'exited' OR reconciliation_state <> 'observed'))
                AND NOT EXISTS(SELECT 1 FROM agents WHERE run_id = ?1
                    AND state IN ('starting', 'running', 'unknown'))
                AND NOT EXISTS(SELECT 1 FROM operations WHERE run_id = ?1
                    AND (status = 'pending' OR reconciliation_state IN ('desired', 'unknown')))
                AND NOT EXISTS(SELECT 1 FROM session_controls WHERE run_id = ?1
                    AND phase <> 'completed')
                AND NOT EXISTS(SELECT 1 FROM workspaces WHERE run_id = ?1
                    AND state IN ('desired', 'unknown')),
                COALESCE((SELECT MAX(sequence) FROM events WHERE run_id = ?1), 0)",
            [run_id], |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        Ok(eligible.then_some(cursor))
    }

    #[cfg(test)]
    pub(crate) fn session_launch_attempt_count(
        &self,
        session_id: crate::id::SessionId,
    ) -> Result<i64, StoreError> {
        Ok(self.transaction.query_row("SELECT attempts FROM session_launch_attempts WHERE session_id = ?1", [session_id], |row| row.get(0))?)
    }

    pub(crate) fn admit_session_launch(
        &self,
        scope: SessionScope,
        now: i64,
        policy: crate::config::SupervisionPolicy,
    ) -> Result<LaunchAdmission, StoreError> {
        if !self.session_scope_is_current(scope)?
            || self.run_shutdown(scope.run_id)?.is_some()
        {
            return Ok(LaunchAdmission::Unknown);
        }
        let previous = self.transaction.query_row("SELECT window_started_at, attempts, next_attempt_at, in_flight, \
             quarantined FROM session_launch_attempts WHERE session_id = ?1", [scope.session_id], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?, row.get::<_, i64>(2)?, row.get::<_, bool>(3)?, row.get::<_, bool>(4)?))).optional()?;
        let (window, attempts) =
            if let Some((window, attempts, next, in_flight, quarantined)) =
                previous
            {
                if quarantined {
                    return Ok(LaunchAdmission::Quarantined);
                }
                if in_flight {
                    return Ok(LaunchAdmission::Unknown);
                }
                if now < next {
                    return Ok(LaunchAdmission::Backoff { until: next });
                }
                if now.saturating_sub(window) >= policy.restart_window_seconds {
                    (now, 0)
                } else if attempts >= policy.max_launch_attempts {
                    return Ok(LaunchAdmission::Quarantined);
                } else {
                    (window, attempts)
                }
            } else {
                (now, 0)
            };
        let delay = policy
            .restart_backoff_seconds
            .saturating_mul(1_i64 << attempts.min(20));
        self.transaction.execute("INSERT INTO session_launch_attempts (session_id, run_id, agent_id, \
             generation, window_started_at, attempts, next_attempt_at, in_flight) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 1) ON CONFLICT(session_id) DO UPDATE \
             SET window_started_at = excluded.window_started_at, attempts = \
             excluded.attempts, next_attempt_at = excluded.next_attempt_at, in_flight = \
             1", params![scope.session_id, scope.run_id, scope.agent_id, scope.generation, window, attempts + 1, now.saturating_add(delay)])?;
        Ok(LaunchAdmission::Allowed)
    }

    pub(crate) fn fail_session_launch(
        &self,
        scope: SessionScope,
        now: i64,
        policy: crate::config::SupervisionPolicy,
    ) -> Result<bool, StoreError> {
        if !self.session_scope_is_current(scope)? {
            return Ok(false);
        }
        self.transaction.execute("UPDATE session_launch_attempts SET in_flight = 0, quarantined = (attempts \
             >= ?2) WHERE session_id = ?1", params![scope.session_id, policy.max_launch_attempts])?;
        let quarantined: bool = self.transaction.query_row("SELECT quarantined FROM session_launch_attempts WHERE session_id = ?1", [scope.session_id], |row| row.get(0))?;
        if quarantined {
            self.supervision_event(scope.run_id, EventKind::SessionRestartLimited, scope.session_id.to_string(), Some(scope.agent_id), None, json!({"generation": scope.generation, "attempts": policy.max_launch_attempts, "window_seconds": policy.restart_window_seconds, "reason": "launch_failures"}), "Quarantined the session after repeated launch failures; inspect its task and workspace before recovery.", now.saturating_mul(1000))?;
        }
        Ok(quarantined)
    }

    pub(crate) fn session_launch_is_retryable(
        &self,
        scope: SessionScope,
    ) -> Result<bool, StoreError> {
        if !self.session_scope_is_current(scope)? {
            return Ok(false);
        }
        Ok(self.transaction.query_row("SELECT in_flight = 0 AND quarantined = 0 FROM session_launch_attempts WHERE session_id = ?1", [scope.session_id], |row| row.get(0)).optional()?.unwrap_or(false))
    }

    pub(crate) fn record_session_failure(
        &self,
        scope: SessionScope,
        now: i64,
        policy: crate::config::SupervisionPolicy,
    ) -> Result<bool, StoreError> {
        if !self.session_scope_is_current(scope)? {
            return Ok(false);
        }
        let changed = self.transaction.execute("INSERT OR IGNORE INTO session_failures (session_id, run_id, agent_id, \
             failed_at) VALUES (?1, ?2, ?3, ?4)", params![scope.session_id, scope.run_id, scope.agent_id, now])?;
        if changed == 0 {
            return Ok(self
                .agent_quarantine_until(scope.run_id, scope.agent_id, now)?
                .is_some());
        }
        let failures: i64 = self.transaction.query_row("SELECT count(*) FROM session_failures WHERE run_id = ?1 AND agent_id = ?2 AND failed_at > ?3", params![scope.run_id, scope.agent_id, now.saturating_sub(policy.restart_window_seconds)], |row| row.get(0))?;
        let quarantined = failures >= policy.max_launch_attempts;
        if quarantined {
            let until = now.saturating_add(policy.restart_window_seconds);
            self.transaction.execute("UPDATE session_failures SET quarantine_until = ?2 WHERE session_id = ?1", params![scope.session_id, until])?;
            self.supervision_event(scope.run_id, EventKind::SessionRestartLimited, scope.session_id.to_string(), Some(scope.agent_id), None, json!({"generation": scope.generation, "failures": failures, "window_seconds": policy.restart_window_seconds, "quarantine_until": until, "reason": "process_failures"}), "Quarantined a crash loop; foreground replacement is blocked until the restart window expires.", now.saturating_mul(1000))?;
        }
        Ok(quarantined)
    }

    /// Stops retrying a spawn whose preflight never established a session.
    pub(crate) fn quarantine_unstarted_agent(
        &self,
        run_id: RunId,
        agent_id: crate::id::AgentId,
        operation_id: OperationId,
        now: i64,
    ) -> Result<(), StoreError> {
        let Some(agent) = self.agent(agent_id)? else {
            return Ok(());
        };
        if agent.run_id != run_id
            || agent.state != crate::providers::LifecycleState::Starting
            || self.latest_session_for_agent(run_id, agent_id)?.is_some()
        {
            return Ok(());
        }
        self.transaction.execute("UPDATE agents SET state = 'quarantined' WHERE id = ?1 AND run_id = ?2 AND generation = ?3", params![agent_id, run_id, agent.generation])?;
        self.supervision_event(run_id, EventKind::AgentLifecycleChanged, agent_id.to_string(), Some(agent_id), Some(operation_id), json!({"generation": agent.generation, "previous_state": "starting", "state": "quarantined", "reason": "preflight_failures"}), "Quarantined a launch after repeated preflight failures; no provider session was created.", now.saturating_mul(1000))
    }

    pub(crate) fn agent_quarantine_until(
        &self,
        run_id: RunId,
        agent_id: crate::id::AgentId,
        now: i64,
    ) -> Result<Option<i64>, StoreError> {
        Ok(self.transaction.query_row("SELECT max(quarantine_until) FROM session_failures WHERE run_id = ?1 AND \
             agent_id = ?2 AND quarantine_until > ?3", params![run_id, agent_id, now], |row| row.get(0))?)
    }

    pub(crate) fn run_shutdown(
        &self,
        run_id: RunId,
    ) -> Result<Option<RunShutdown>, StoreError> {
        Ok(self.transaction.query_row(
            "SELECT operation_id, phase, requested_at_ms, interrupt_until_ms, \
             deadline_ms FROM run_shutdowns WHERE run_id = ?1",
            [run_id], |row| Ok(RunShutdown { operation_id: row.get(0)?, phase: decode(row, 1)?, requested_at_ms: row.get(2)?, interrupt_until_ms: row.get(3)?, deadline_ms: row.get(4)? }),
        ).optional()?)
    }

    pub(crate) fn begin_run_shutdown(
        &self,
        run_id: RunId,
        operation_id: OperationId,
        now_ms: i64,
        grace_ms: i64,
        timeout_ms: i64,
    ) -> Result<(), StoreError> {
        if let Some(shutdown) = self.run_shutdown(run_id)? {
            return if shutdown.operation_id == operation_id {
                Ok(())
            } else {
                Err(StoreError::OperationConflict { id: operation_id })
            };
        }
        if self.operation(operation_id)?.is_some() {
            return Err(StoreError::OperationConflict { id: operation_id });
        }
        if !self.run(run_id)?.is_some_and(|run| run.status == "active") {
            return Err(StoreError::RunNotActive { id: run_id });
        }
        self.insert_operation(&OperationRecord {
            id: operation_id,
            run_id,
            kind: "run.stop".into(),
            actor_agent_id: None,
            status: "pending".into(),
            request: json!({}),
            result: None,
            attempt_count: 1,
            reconciliation_state: Some(ExternalResourceState::Desired),
            reconciliation_attempt_count: 0,
            reconciliation_error: None,
            reconciled_at: None,
            created_at: now_ms / 1000,
            updated_at: now_ms / 1000,
        })?;
        self.transaction.execute(
            "INSERT INTO run_shutdowns (run_id, operation_id, phase, requested_at_ms, \
             interrupt_until_ms, deadline_ms) VALUES (?1, ?2, 'interrupting', ?3, ?4, \
             ?5)",
            params![run_id, operation_id, now_ms, now_ms.saturating_add(grace_ms), now_ms.saturating_add(timeout_ms)],
        )?;
        self.supervision_event(
            run_id,
            EventKind::RunShutdownChanged,
            run_id.to_string(),
            None,
            Some(operation_id),
            json!({"phase": ShutdownPhase::Interrupting}),
            "Stopping new launches and draining assignments.",
            now_ms,
        )?;
        for agent in self.agents(run_id)? {
            if let Some(assignment) =
                self.active_assignment_for_agent(run_id, agent.id)?
            {
                self.drain_assignment(&assignment, now_ms)?;
            }
        }
        Ok(())
    }

    fn drain_assignment(
        &self,
        assignment: &super::AssignmentRecord,
        now_ms: i64,
    ) -> Result<(), StoreError> {
        if assignment.state == "draining" {
            return Ok(());
        }
        self.transaction.execute("UPDATE assignments SET state = 'draining' WHERE id = ?1 AND completed_at IS NULL", [assignment.id])?;
        self.append_event(&NewEvent {
            run_id: assignment.run_id, kind: EventKind::AssignmentLifecycleChanged,
            actor: "supervisor".into(), subject: assignment.id.to_string(), project_id: None,
            agent_id: Some(assignment.agent_id), task_id: Some(assignment.task_id), operation_id: None,
            correlation_id: None, causation_id: None,
            data: json!({"previous_state": assignment.state, "state": "draining", "generation": assignment.generation}),
            summary: format!("Draining assignment {} while preserving its task and workspace.", assignment.id), created_at: now_ms / 1000,
        })?;
        Ok(())
    }

    pub(crate) fn set_shutdown_phase(
        &self,
        run_id: RunId,
        phase: ShutdownPhase,
        now_ms: i64,
    ) -> Result<(), StoreError> {
        let Some(shutdown) = self.run_shutdown(run_id)? else {
            return Ok(());
        };
        if shutdown.phase == phase || shutdown.phase == ShutdownPhase::Completed
        {
            return Ok(());
        }
        self.transaction.execute(
            "UPDATE run_shutdowns SET phase = ?2 WHERE run_id = ?1",
            params![run_id, encoded(phase)],
        )?;
        self.supervision_event(
            run_id,
            EventKind::RunShutdownChanged,
            run_id.to_string(),
            None,
            Some(shutdown.operation_id),
            json!({"previous_phase": shutdown.phase, "phase": phase}),
            "Shutdown phase changed.",
            now_ms,
        )
    }

    pub(crate) fn complete_shutdown_operation(
        &self,
        run_id: RunId,
        response: &serde_json::Value,
        now_ms: i64,
    ) -> Result<(), StoreError> {
        let shutdown = self
            .run_shutdown(run_id)?
            .ok_or(StoreError::RunNotActive { id: run_id })?;
        self.set_shutdown_phase(run_id, ShutdownPhase::Completed, now_ms)?;
        self.transaction.execute("UPDATE operations SET status = 'completed', result_json = ?2, updated_at = ?3 WHERE id = ?1", params![shutdown.operation_id, serde_json::to_string(response)?, now_ms / 1000])?;
        self.record_operation_reconciliation(
            shutdown.operation_id,
            ExternalResourceState::Observed,
            None,
            now_ms / 1000,
        )?;
        Ok(())
    }

    pub(crate) fn session_controls(
        &self,
        run_id: RunId,
    ) -> Result<Vec<SessionControl>, StoreError> {
        let mut query = self.transaction.prepare("SELECT session_id, agent_id, generation, reason, phase, delivered, \
             requested_at_ms, interrupt_until_ms, kill_at_ms, deadline_ms FROM \
             session_controls WHERE run_id = ?1 ORDER BY session_id")?;
        Ok(query
            .query_map([run_id], |row| {
                Ok(SessionControl {
                    scope: SessionScope {
                        run_id,
                        session_id: row.get(0)?,
                        agent_id: row.get(1)?,
                        generation: row.get(2)?,
                    },
                    reason: decode(row, 3)?,
                    phase: decode(row, 4)?,
                    delivered: row.get(5)?,
                    requested_at_ms: row.get(6)?,
                    interrupt_until_ms: row.get(7)?,
                    kill_at_ms: row.get(8)?,
                    deadline_ms: row.get(9)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?)
    }

    pub(crate) fn request_session_control(
        &self,
        scope: SessionScope,
        reason: ControlReason,
        now_ms: i64,
        grace_ms: i64,
        timeout_ms: i64,
    ) -> Result<(), StoreError> {
        if !self.session_scope_is_current(scope)? {
            return Ok(());
        }
        let changed = self.transaction.execute("INSERT OR IGNORE INTO session_controls (session_id, run_id, agent_id, \
             generation, reason, phase, requested_at_ms, interrupt_until_ms, \
             kill_at_ms, deadline_ms) VALUES (?1, ?2, ?3, ?4, ?5, 'interrupt', ?6, ?7, \
             ?8, ?9)",
            params![scope.session_id, scope.run_id, scope.agent_id, scope.generation, encoded(reason), now_ms, now_ms.saturating_add(grace_ms), now_ms.saturating_add(timeout_ms / 2), now_ms.saturating_add(timeout_ms)])?;
        if changed == 0 {
            return Ok(());
        }
        if let Some(assignment) =
            self.active_assignment_for_agent(scope.run_id, scope.agent_id)?
        {
            self.drain_assignment(&assignment, now_ms)?;
        }
        self.supervision_event(scope.run_id, EventKind::SessionControlChanged, scope.session_id.to_string(), Some(scope.agent_id), None, json!({"generation": scope.generation, "phase": ControlPhase::Interrupt, "reason": reason}), "Requested bounded session shutdown.", now_ms)
    }

    pub(crate) fn set_control_phase(
        &self,
        control: &SessionControl,
        phase: ControlPhase,
        now_ms: i64,
    ) -> Result<(), StoreError> {
        if !self.session_scope_is_current(control.scope)?
            || phase == control.phase
        {
            return Ok(());
        }
        self.transaction.execute("UPDATE session_controls SET phase = ?2, delivered = 0 WHERE session_id = ?1", params![control.scope.session_id, encoded(phase)])?;
        self.supervision_event(control.scope.run_id, EventKind::SessionControlChanged, control.scope.session_id.to_string(), Some(control.scope.agent_id), None, json!({"generation": control.scope.generation, "previous_phase": control.phase, "phase": phase, "reason": control.reason}), "Session control phase changed.", now_ms)
    }

    pub(crate) fn mark_control_delivered(
        &self,
        control: &SessionControl,
    ) -> Result<(), StoreError> {
        if self.session_scope_is_current(control.scope)? {
            self.transaction.execute("UPDATE session_controls SET delivered = 1 WHERE session_id = ?1 AND phase = ?2", params![control.scope.session_id, encoded(control.phase)])?;
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn supervision_event(
        &self,
        run_id: RunId,
        kind: EventKind,
        subject: String,
        agent_id: Option<crate::id::AgentId>,
        operation_id: Option<OperationId>,
        data: serde_json::Value,
        summary: &str,
        now_ms: i64,
    ) -> Result<(), StoreError> {
        self.append_event(&NewEvent {
            run_id,
            kind,
            actor: "supervisor".into(),
            subject,
            project_id: None,
            agent_id,
            task_id: None,
            operation_id,
            correlation_id: None,
            causation_id: None,
            data,
            summary: summary.into(),
            created_at: now_ms / 1000,
        })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::id::{OperationId, RunId};
    use crate::state::{RunRecord, Store, StoreError};

    #[test]
    fn shutdown_intent_survives_reopen_and_keeps_its_original_deadlines() {
        let path = std::env::temp_dir()
            .join(format!("coterie-shutdown-{}.sqlite3", RunId::generate()));
        let run_id = RunId::generate();
        let operation_id = OperationId::generate();
        let mut store = Store::open(&path).unwrap();
        store
            .transaction(|repositories| {
                repositories.insert_run(&RunRecord {
                    id: run_id,
                    status: "active".into(),
                    created_at: 1,
                    stopped_at: None,
                })?;
                repositories.begin_run_shutdown(
                    run_id,
                    operation_id,
                    1_000,
                    250,
                    5_000,
                )?;
                Ok(())
            })
            .unwrap();
        drop(store);
        let mut store = Store::open(&path).unwrap();
        store
            .transaction(|repositories| {
                let first = repositories.run_shutdown(run_id)?.unwrap();
                repositories.begin_run_shutdown(
                    run_id,
                    operation_id,
                    2_000,
                    250,
                    5_000,
                )?;
                assert_eq!(
                    repositories.run_shutdown(run_id)?,
                    Some(first.clone())
                );
                assert_eq!(first.interrupt_until_ms, 1_250);
                assert_eq!(first.deadline_ms, 6_000);
                assert!(matches!(
                    repositories.begin_run_shutdown(
                        run_id,
                        OperationId::generate(),
                        2_000,
                        250,
                        5_000
                    ),
                    Err(StoreError::OperationConflict { .. })
                ));
                Ok(())
            })
            .unwrap();
        drop(store);
        std::fs::remove_file(path).unwrap();
    }
}

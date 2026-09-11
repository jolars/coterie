//! Bounded projections of durable lifecycle events for progress inspection.

use super::{Repositories, RunId, StoreError, params};
use crate::protocol::progress::{ProgressChange, ProgressState};

pub(crate) struct ProgressScan {
    pub(crate) changes: Vec<ProgressChange>,
    pub(crate) next_sequence: i64,
    pub(crate) high_watermark: i64,
}

impl Repositories<'_, '_> {
    pub(crate) fn progress_after(
        &self,
        run_id: RunId,
        after: i64,
        limit: u16,
    ) -> Result<ProgressScan, StoreError> {
        let high_watermark = self.transaction.query_row(
            "SELECT COALESCE(MAX(sequence), 0) FROM events WHERE run_id = ?1",
            [run_id],
            |row| row.get(0),
        )?;
        // Project in SQLite so legacy oversized bodies never enter the response
        // or require decoding whole operator events in the progress reader.
        // Text projections retain one byte beyond the longest valid value so
        // oversized fields fail validation instead of truncating into valid data.
        let mut query = self.transaction.prepare(
            "SELECT sequence, CASE event_type
                WHEN 'task.created' THEN json_object('kind', 'task',
                    'task_id', CAST(substr(CAST(subject AS BLOB), 1, 30) AS TEXT), 'project_id', CAST(substr(CAST(project_id AS BLOB), 1, 30) AS TEXT),
                    'status', CAST(substr(CAST(json_extract(payload_json, '$.data.status') AS BLOB), 1, 12) AS TEXT))
                WHEN 'task.lifecycle_changed' THEN json_object('kind', 'task',
                    'task_id', CAST(substr(CAST(subject AS BLOB), 1, 30) AS TEXT), 'project_id', CAST(substr(CAST(project_id AS BLOB), 1, 30) AS TEXT),
                    'status', CAST(substr(CAST(json_extract(payload_json, '$.data.status') AS BLOB), 1, 12) AS TEXT))
                WHEN 'assignment.created' THEN json_object('kind', 'assignment',
                    'assignment_id', CAST(substr(CAST(subject AS BLOB), 1, 30) AS TEXT), 'task_id', CAST(substr(CAST(task_id AS BLOB), 1, 30) AS TEXT), 'agent_id', CAST(substr(CAST(agent_id AS BLOB), 1, 30) AS TEXT),
                    'state', CAST(substr(CAST(json_extract(payload_json, '$.data.state') AS BLOB), 1, 12) AS TEXT))
                WHEN 'assignment.lifecycle_changed' THEN json_object('kind', 'assignment',
                    'assignment_id', CAST(substr(CAST(subject AS BLOB), 1, 30) AS TEXT), 'task_id', CAST(substr(CAST(task_id AS BLOB), 1, 30) AS TEXT), 'agent_id', CAST(substr(CAST(agent_id AS BLOB), 1, 30) AS TEXT),
                    'state', CAST(substr(CAST(json_extract(payload_json, '$.data.state') AS BLOB), 1, 12) AS TEXT))
                WHEN 'assignment.session_associated' THEN json_object('kind', 'assignment_session',
                    'assignment_id', CAST(substr(CAST(subject AS BLOB), 1, 30) AS TEXT), 'task_id', CAST(substr(CAST(task_id AS BLOB), 1, 30) AS TEXT), 'agent_id', CAST(substr(CAST(agent_id AS BLOB), 1, 30) AS TEXT),
                    'session_id', CAST(substr(CAST(json_extract(payload_json, '$.data.session_id') AS BLOB), 1, 30) AS TEXT))
                WHEN 'agent.created' THEN json_object('kind', 'agent',
                    'agent_id', CAST(substr(CAST(subject AS BLOB), 1, 30) AS TEXT), 'generation', CASE WHEN json_type(payload_json, '$.data.generation') = 'integer' AND typeof(json_extract(payload_json, '$.data.generation')) = 'integer' THEN json_extract(payload_json, '$.data.generation') ELSE NULL END,
                    'state', CAST(substr(CAST(json_extract(payload_json, '$.data.state') AS BLOB), 1, 12) AS TEXT))
                WHEN 'agent.lifecycle_changed' THEN json_object('kind', 'agent',
                    'agent_id', CAST(substr(CAST(subject AS BLOB), 1, 30) AS TEXT), 'generation', CASE WHEN json_type(payload_json, '$.data.generation') = 'integer' AND typeof(json_extract(payload_json, '$.data.generation')) = 'integer' THEN json_extract(payload_json, '$.data.generation') ELSE NULL END,
                    'state', CAST(substr(CAST(json_extract(payload_json, '$.data.state') AS BLOB), 1, 12) AS TEXT))
                WHEN 'session.started' THEN json_object('kind', 'session',
                    'session_id', CAST(substr(CAST(subject AS BLOB), 1, 30) AS TEXT), 'agent_id', CAST(substr(CAST(agent_id AS BLOB), 1, 30) AS TEXT),
                    'generation', CASE WHEN json_type(payload_json, '$.data.generation') = 'integer' AND typeof(json_extract(payload_json, '$.data.generation')) = 'integer' THEN json_extract(payload_json, '$.data.generation') ELSE NULL END,
                    'state', CAST(substr(CAST(json_extract(payload_json, '$.data.state') AS BLOB), 1, 12) AS TEXT))
                WHEN 'session.lifecycle_changed' THEN json_object('kind', 'session',
                    'session_id', CAST(substr(CAST(subject AS BLOB), 1, 30) AS TEXT), 'agent_id', CAST(substr(CAST(agent_id AS BLOB), 1, 30) AS TEXT),
                    'generation', CASE WHEN json_type(payload_json, '$.data.generation') = 'integer' AND typeof(json_extract(payload_json, '$.data.generation')) = 'integer' THEN json_extract(payload_json, '$.data.generation') ELSE NULL END,
                    'state', CAST(substr(CAST(json_extract(payload_json, '$.data.state') AS BLOB), 1, 12) AS TEXT))
                ELSE NULL END,
                CASE WHEN event_type IN ('task.created', 'task.lifecycle_changed',
                    'assignment.created', 'assignment.lifecycle_changed',
                    'assignment.session_associated', 'agent.created',
                    'agent.lifecycle_changed', 'session.started', 'session.lifecycle_changed')
                    THEN CASE WHEN json_type(payload_json, '$.schema_version') = 'integer'
                        THEN json_extract(payload_json, '$.schema_version') ELSE NULL END
                    ELSE NULL END
             FROM events WHERE run_id = ?1 AND sequence > ?2 AND sequence <= ?3
             ORDER BY sequence LIMIT 256",
        )?;
        let mut rows = query.query(params![run_id, after, high_watermark])?;
        let mut changes = Vec::new();
        let mut next_sequence = after;
        while let Some(row) = rows.next()? {
            let sequence: i64 = row.get(0)?;
            let projected: Option<String> = row.get(1)?;
            if let Some(projected) = projected {
                if changes.len() == usize::from(limit) {
                    break;
                }
                let invalid = || StoreError::InvalidProgressEvent { sequence };
                if row.get::<_, Option<i64>>(2)? != Some(1) {
                    return Err(invalid());
                }
                let state: ProgressState =
                    serde_json::from_str(&projected).map_err(|_| invalid())?;
                changes.push(ProgressChange {
                    sequence: u64::try_from(sequence).map_err(|_| invalid())?,
                    state,
                });
            }
            next_sequence = sequence;
        }
        Ok(ProgressScan {
            changes,
            next_sequence,
            high_watermark,
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::protocol::progress::{AssignmentState, ProgressState};
    use crate::state::*;

    fn fixture() -> (Store, RunId, ProjectId, AgentId, TaskId) {
        let mut store = Store::open_in_memory().unwrap();
        let run = RunId::generate();
        let project = ProjectId::generate();
        let agent = AgentId::generate();
        let task = TaskId::generate();
        store
            .transaction(|r| {
                r.insert_test_run(&RunRecord {
                    id: run,
                    status: "active".into(),
                    created_at: 1,
                    stopped_at: None,
                })?;
                r.insert_project(&ProjectRecord {
                    id: project,
                    run_id: run,
                    alias: "primary".into(),
                    original_path: "/tmp".into(),
                    canonical_path: "/tmp".into(),
                    identity: ProjectIdentity::Directory {
                        canonical_directory: "/tmp".into(),
                    },
                    is_primary: true,
                    attached_at: 1,
                })?;
                r.insert_agent(&AgentRecord {
                    id: agent,
                    run_id: run,
                    role: "worker".into(),
                    generation: 1,
                    state: LifecycleState::Running,
                    created_at: 1,
                })?;
                r.insert_task(&TaskRecord {
                    id: task,
                    run_id: run,
                    project_id: project,
                    group_id: None,
                    title: "private title".into(),
                    description: "private body".repeat(100_000),
                    status: TaskStatus::Open,
                    result: None,
                    created_at: 1,
                    updated_at: 1,
                })?;
                Ok(())
            })
            .unwrap();
        (store, run, project, agent, task)
    }

    #[test]
    fn progress_preserves_submission_and_independent_provider_exit_order() {
        let (mut store, run, project, agent, task) = fixture();
        let assignment = AssignmentId::generate();
        store
            .claim_task(&ClaimTaskMutation {
                operation_id: OperationId::generate(),
                run_id: run,
                actor_agent_id: Some(agent),
                task_id: task,
                agent_id: agent,
                assignment_id: assignment,
                claimed_at: 2,
            })
            .unwrap();
        store
            .transition_task(&TaskTransitionMutation {
                operation_id: OperationId::generate(),
                run_id: run,
                actor_agent_id: Some(agent),
                task_id: task,
                transition: TaskTransition::Submit,
                operator_override: None,
                result: None,
                summary: Some("private result".into()),
                transitioned_at: 3,
            })
            .unwrap();
        store
            .transaction(|r| {
                r.append_event(&NewEvent {
                    run_id: run,
                    kind: EventKind::AgentLifecycleChanged,
                    actor: "provider".into(),
                    subject: agent.to_string(),
                    project_id: None,
                    agent_id: Some(agent),
                    task_id: None,
                    operation_id: None,
                    correlation_id: None,
                    causation_id: None,
                    data: json!({"generation": 1, "state": "exited"}),
                    summary: "private exit detail".into(),
                    created_at: 4,
                })?;
                Ok(())
            })
            .unwrap();
        let page = store
            .transaction(|r| r.progress_after(run, 0, 100))
            .unwrap();
        assert!(page.changes.iter().any(|c| c.state
            == ProgressState::Task {
                task_id: task,
                project_id: project,
                status: TaskStatus::Submitted
            }));
        assert!(page.changes.iter().any(|c| c.state
            == ProgressState::Assignment {
                assignment_id: assignment,
                task_id: task,
                agent_id: agent,
                state: AssignmentState::Completed
            }));
        assert!(matches!(
            page.changes.last().unwrap().state,
            ProgressState::Agent {
                state: LifecycleState::Exited,
                ..
            }
        ));
        let encoded = serde_json::to_string(&page.changes).unwrap();
        assert!(!encoded.contains("private"));
        let mut cursor = 0;
        let mut changes = Vec::new();
        while cursor < page.high_watermark {
            let next = store
                .transaction(|r| r.progress_after(run, cursor, 1))
                .unwrap();
            assert!(next.changes.len() <= 1);
            assert!(next.next_sequence > cursor);
            cursor = next.next_sequence;
            changes.extend(next.changes);
        }
        assert_eq!(changes, page.changes);
        assert_eq!(
            store
                .transaction(|r| r.progress_after(run, cursor, 100))
                .unwrap()
                .changes,
            Vec::new()
        );
    }

    #[test]
    fn progress_bounds_scans_and_skips_operator_events_without_payloads() {
        let (mut store, run, _, agent, _) = fixture();
        store
            .transaction(|r| {
                for _ in 0..300 {
                    r.append_event(&NewEvent {
                        run_id: run,
                        kind: EventKind::MessageSent,
                        actor: "operator".into(),
                        subject: "private".into(),
                        project_id: None,
                        agent_id: Some(agent),
                        task_id: None,
                        operation_id: None,
                        correlation_id: None,
                        causation_id: None,
                        data: json!({"body": "secret"}),
                        summary: "private".into(),
                        created_at: 2,
                    })?;
                }
                Ok(())
            })
            .unwrap();
        let first = store
            .transaction(|r| r.progress_after(run, 0, 100))
            .unwrap();
        assert!(first.changes.is_empty());
        assert_eq!(first.next_sequence, 256);
        assert_eq!(first.high_watermark, 300);
        let second = store
            .transaction(|r| r.progress_after(run, first.next_sequence, 100))
            .unwrap();
        assert!(second.changes.is_empty());
        assert_eq!(second.next_sequence, 300);
        store
            .transaction(|r| {
                r.append_event(&NewEvent {
                    run_id: run,
                    kind: EventKind::AgentLifecycleChanged,
                    actor: "provider".into(),
                    subject: agent.to_string(),
                    project_id: None,
                    agent_id: Some(agent),
                    task_id: None,
                    operation_id: None,
                    correlation_id: None,
                    causation_id: None,
                    data: json!({"generation": 1, "state": "unknown"}),
                    summary: "Private provider detail".into(),
                    created_at: 3,
                })?;
                Ok(())
            })
            .unwrap();
        let latest = store
            .transaction(|r| r.progress_after(run, second.next_sequence, 100))
            .unwrap();
        assert_eq!(latest.next_sequence, 301);
        assert!(matches!(
            latest.changes[0].state,
            ProgressState::Agent {
                state: LifecycleState::Unknown,
                ..
            }
        ));
    }

    #[test]
    fn progress_projects_legacy_oversized_events_and_fails_closed_on_invalid_states()
     {
        let (mut store, run, project, _, task) = fixture();
        let huge = "private body".repeat(200_000);
        // This simulates a legacy event written before the event insertion bound.
        store.transaction(|r| {
            r.transaction.execute(
                "INSERT INTO events (id, run_id, sequence, event_type, actor, subject,
                    project_id, task_id, payload_json, summary, created_at)
                 VALUES (?1, ?2, 1, 'task.created', 'operator', ?3, ?4, ?3, ?5, 'private summary', 1)",
                params![EventId::generate(), run, task, project, json!({"schema_version": 1, "data": {"status": "open", "title": huge, "result": huge}}).to_string()],
            )?;
            Ok(())
        }).unwrap();
        let page = store
            .transaction(|r| r.progress_after(run, 0, 100))
            .unwrap();
        let encoded = serde_json::to_string(&page.changes).unwrap();
        assert!(encoded.len() < 512);
        assert!(!encoded.contains("private"));
        assert_eq!(page.next_sequence, 1);
        store.transaction(|r| {
            r.transaction.execute(
                "INSERT INTO events (id, run_id, sequence, event_type, actor, subject,
                    payload_json, summary, created_at)
                 VALUES (?1, ?2, 2, 'message.sent', 'operator', 'private', ?3, 'private summary', 2)",
                params![EventId::generate(), run, json!({"schema_version": 1, "data": {"body": huge}}).to_string()],
            )?;
            Ok(())
        }).unwrap();
        let excluded = store
            .transaction(|r| r.progress_after(run, 1, 100))
            .unwrap();
        assert!(excluded.changes.is_empty());
        assert_eq!(excluded.next_sequence, 2);
        store
            .transaction(|r| {
                r.append_event(&NewEvent {
                    run_id: run,
                    kind: EventKind::TaskLifecycleChanged,
                    actor: "operator".into(),
                    subject: task.to_string(),
                    project_id: Some(project),
                    agent_id: None,
                    task_id: Some(task),
                    operation_id: None,
                    correlation_id: None,
                    causation_id: None,
                    data: json!({"status": "private corrupt state"}),
                    summary: "private summary".into(),
                    created_at: 2,
                })?;
                Ok(())
            })
            .unwrap();
        let error = store
            .transaction(|r| r.progress_after(run, 0, 100))
            .err()
            .unwrap();
        assert!(matches!(
            error,
            StoreError::InvalidProgressEvent { sequence: 3 }
        ));
        assert!(!error.to_string().contains("private"));
    }

    #[test]
    fn progress_rejects_oversized_fields_without_truncating_into_valid_states()
    {
        for field in [
            "status",
            "subject",
            "generation",
            "boolean_generation",
            "version",
        ] {
            let (mut store, run, project, agent, task) = fixture();
            let huge = "private".repeat(200_000);
            let (kind, subject, data) = match field {
                "status" => (
                    "task.created",
                    task.to_string(),
                    json!({"status": format!("open\u{0}{huge}")}),
                ),
                "subject" => (
                    "task.created",
                    format!("{task}{huge}"),
                    json!({"status": "open"}),
                ),
                "generation" => (
                    "agent.created",
                    agent.to_string(),
                    json!({"state": "starting", "generation": huge}),
                ),
                "boolean_generation" => (
                    "agent.created",
                    agent.to_string(),
                    json!({"state": "starting", "generation": true}),
                ),
                "version" => (
                    "task.created",
                    task.to_string(),
                    json!({"status": "open"}),
                ),
                _ => unreachable!(),
            };
            store.transaction(|r| {
                r.transaction.execute(
                    "INSERT INTO events (id, run_id, sequence, event_type, actor, subject,
                        project_id, task_id, payload_json, summary, created_at)
                     VALUES (?1, ?2, 1, ?3, 'operator', ?4, ?5, ?6, ?7, 'private summary', 1)",
                    params![EventId::generate(), run, kind, subject, project, task, json!({"schema_version": if field == "version" { json!(true) } else { json!(1) }, "data": data}).to_string()],
                )?;
                Ok(())
            }).unwrap();
            let error = store
                .transaction(|r| r.progress_after(run, 0, 100))
                .err()
                .unwrap();
            assert!(
                matches!(
                    error,
                    StoreError::InvalidProgressEvent { sequence: 1 }
                ),
                "{error:?}"
            );
            assert!(!error.to_string().contains("private"));
        }
    }
}

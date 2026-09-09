//! Snapshot diagnostics that never migrate or repair the database.

use super::*;
use crate::doctor::{CheckStatus, DoctorReport};

impl Store {
    pub(crate) fn open_read_only(path: &Path) -> Result<Self, StoreError> {
        let connection = Connection::open_with_flags(
            path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
                | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        connection.busy_timeout(BUSY_TIMEOUT)?;
        connection.pragma_update(None, "query_only", true)?;
        Ok(Self { connection })
    }

    pub(crate) fn diagnose(
        &mut self,
        run_id: RunId,
    ) -> Result<DoctorReport, StoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Deferred)?;
        let mut report = DoctorReport {
            run_id: Some(run_id),
            checks: Vec::new(),
        };
        let integrity: String =
            transaction
                .query_row("PRAGMA quick_check", [], |row| row.get(0))?;
        report.add(
            "database_integrity",
            if integrity == "ok" {
                CheckStatus::Ok
            } else {
                CheckStatus::Error
            },
            None,
            integrity,
        );
        let foreign_keys: i64 = transaction.query_row(
            "SELECT count(*) FROM pragma_foreign_key_check",
            [],
            |row| row.get(0),
        )?;
        report.add(
            "foreign_keys",
            if foreign_keys == 0 {
                CheckStatus::Ok
            } else {
                CheckStatus::Error
            },
            None,
            format!("{foreign_keys} foreign-key violations."),
        );
        let applied = transaction.prepare("SELECT version, name, source FROM schema_migrations ORDER BY version")?
            .query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        let migrations_match = applied.len() == MIGRATIONS.len()
            && applied.iter().zip(MIGRATIONS).all(
                |((version, name, source), migration)| {
                    *version == migration.version
                        && name == migration.name
                        && source == migration.sql
                },
            );
        report.add("database_migrations", if migrations_match { CheckStatus::Ok } else { CheckStatus::Error }, None,
            if migrations_match { "All applied migrations match the compiled schema." } else { "Schema is pending, newer, modified, or noncontiguous; preserve the database and use a compatible Coterie binary." });
        if !migrations_match {
            return Ok(report);
        }
        let status: Option<String> = transaction
            .query_row(
                "SELECT status FROM runs WHERE id = ?1",
                [run_id],
                |row| row.get(0),
            )
            .optional()?;
        report.add(
            "run",
            if status.is_some() {
                CheckStatus::Ok
            } else {
                CheckStatus::Error
            },
            Some(run_id.to_string()),
            format!(
                "Durable run status: {}.",
                status.as_deref().unwrap_or("missing")
            ),
        );
        for (check, sql, advice) in [
            (
                "operations",
                "SELECT id FROM operations WHERE run_id = ?1 AND (status NOT IN ('succeeded', 'completed', 'failed') OR reconciliation_state IN ('desired', 'unknown', 'lost')) ORDER BY id",
                "Operation requires reconciliation or operator inspection; retry its original operation ID.",
            ),
            (
                "assignments",
                "SELECT a.id FROM assignments a JOIN agents g ON g.id = a.agent_id LEFT JOIN sessions s ON s.id = a.session_id WHERE a.run_id = ?1 AND a.completed_at IS NULL AND (a.generation <> g.generation OR s.id IS NULL OR s.generation <> a.generation OR s.state IN ('exited', 'lost', 'unknown', 'quarantined')) ORDER BY a.id",
                "Unfinished assignment has no verified current live session; preserve its claim and workspace.",
            ),
            (
                "sessions",
                "SELECT id FROM sessions WHERE run_id = ?1 AND (state IN ('lost', 'unknown', 'quarantined') OR reconciliation_state IN ('unknown', 'lost')) ORDER BY id",
                "Session needs inspection; unknown process ownership is not permission to adopt or kill it.",
            ),
            (
                "task_cycles",
                "WITH RECURSIVE reach(task, dependency) AS (SELECT task_id, dependency_task_id FROM task_dependencies WHERE run_id = ?1 UNION SELECT r.task, d.dependency_task_id FROM reach r JOIN task_dependencies d ON d.task_id = r.dependency WHERE d.run_id = ?1) SELECT DISTINCT task FROM reach WHERE task = dependency ORDER BY task",
                "Task dependency cycle blocks readiness; preserve tasks and inspect the dependency graph.",
            ),
        ] {
            let subjects = transaction
                .prepare(sql)?
                .query_map([run_id], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            if subjects.is_empty() {
                report.add(
                    check,
                    CheckStatus::Ok,
                    None,
                    "No inconsistency found in the durable snapshot.",
                );
            } else {
                for subject in subjects {
                    report.add(
                        check,
                        CheckStatus::Warning,
                        Some(subject),
                        advice,
                    );
                }
            }
        }
        transaction.commit()?;
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offline_inspection_cannot_create_or_mutate_a_database() {
        let root = std::env::temp_dir()
            .join(format!("coterie-doctor-{}", RunId::generate()));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("state.sqlite3");
        assert!(Store::open_read_only(&path).is_err());
        assert!(!path.exists());
        let mut store = Store::open(&path).unwrap();
        let run = RunRecord {
            id: RunId::generate(),
            status: "active".into(),
            created_at: 1,
            stopped_at: None,
        };
        store
            .transaction(|repositories| repositories.insert_run(&run))
            .unwrap();
        drop(store);
        let mut store = Store::open_read_only(&path).unwrap();
        let report = store.diagnose(run.id).unwrap();
        assert!(
            report
                .checks
                .iter()
                .all(|check| check.status == CheckStatus::Ok)
        );
        let another = RunRecord {
            id: RunId::generate(),
            ..run.clone()
        };
        assert!(
            store
                .transaction(|repositories| repositories.insert_run(&another))
                .is_err()
        );
        assert_eq!(store.diagnose(run.id).unwrap(), report);
        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn event_pages_bound_large_payloads_without_skipping_sequences() {
        let mut store = Store::open_in_memory().unwrap();
        let run = RunRecord {
            id: RunId::generate(),
            status: "active".into(),
            created_at: 1,
            stopped_at: None,
        };
        store
            .transaction(|repositories| {
                repositories.insert_run(&run)?;
                for _ in 0..4 {
                    repositories.append_event(&NewEvent {
                        run_id: run.id,
                        kind: EventKind::RunStarted,
                        actor: "operator".into(),
                        subject: run.id.to_string(),
                        project_id: None,
                        agent_id: None,
                        task_id: None,
                        operation_id: None,
                        correlation_id: None,
                        causation_id: None,
                        data: json!({"text": "x".repeat(400_000)}),
                        summary: "Large event fixture.".into(),
                        created_at: 1,
                    })?;
                }
                let first = repositories.events_after(run.id, 0, 1000)?;
                assert_eq!(first.len(), 2);
                let second = repositories.events_after(
                    run.id,
                    first[1].sequence,
                    1000,
                )?;
                assert_eq!(
                    second
                        .iter()
                        .map(|event| event.sequence)
                        .collect::<Vec<_>>(),
                    vec![3, 4]
                );
                Ok(())
            })
            .unwrap();
    }
}

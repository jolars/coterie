//! Configuration commits atomically with its run and is never replaced on recovery.

use super::*;
use crate::config::{EffectiveConfig, RunConfiguration};

impl Store {
    pub(crate) fn configuration(
        &mut self,
        run_id: RunId,
    ) -> Result<EffectiveConfig, StoreError> {
        self.transaction(|repositories| repositories.configuration(run_id))
    }
}

impl Repositories<'_, '_> {
    pub(crate) fn has_run_configuration(
        &self,
        run_id: RunId,
    ) -> Result<bool, StoreError> {
        Ok(self.transaction.query_row("SELECT EXISTS(SELECT 1 FROM configuration_snapshots WHERE run_id = ?1 AND scope = 'run')", [run_id], |row| row.get(0))?)
    }

    pub(crate) fn recent_spawns(
        &self,
        run_id: RunId,
        now: i64,
    ) -> Result<i64, StoreError> {
        Ok(self.transaction.query_row(
            "SELECT count(*) FROM operations WHERE run_id = ?1 AND kind = 'agent.spawn' AND created_at > ?2",
            params![run_id, now.saturating_sub(60)], |row| row.get(0),
        )?)
    }

    pub(crate) fn configuration(
        &self,
        run_id: RunId,
    ) -> Result<EffectiveConfig, StoreError> {
        Ok(self.run_configuration(run_id)?.restore())
    }

    pub(crate) fn run_configuration(
        &self,
        run_id: RunId,
    ) -> Result<RunConfiguration, StoreError> {
        let stored: Option<(i64, String, String)> = self.transaction.query_row(
            "SELECT schema_version, fingerprint, document_json FROM configuration_snapshots WHERE run_id = ?1 AND scope = 'run'",
            [run_id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        ).optional()?;
        let invalid = || StoreError::InvalidConfigurationSnapshot { run_id };
        let (version, fingerprint, document) = stored.ok_or_else(invalid)?;
        let snapshot: RunConfiguration =
            serde_json::from_str(&document).map_err(|_| invalid())?;
        if version != 1 || !snapshot.valid(&fingerprint) {
            return Err(invalid());
        }
        Ok(snapshot)
    }

    pub(crate) fn snapshot_configuration(
        &self,
        run_id: RunId,
        config: &EffectiveConfig,
        created_at: i64,
    ) -> Result<(), StoreError> {
        let snapshot = RunConfiguration::new(config.clone());
        self.transaction.execute(
            "INSERT INTO configuration_snapshots (run_id, scope, schema_version, fingerprint, document_json, created_at) VALUES (?1, 'run', 1, ?2, ?3, ?4)",
            params![run_id, snapshot.fingerprint(), serde_json::to_string(&snapshot)?, created_at],
        )?;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn insert_test_run(
        &self,
        run: &RunRecord,
    ) -> Result<(), StoreError> {
        self.insert_run(run)?;
        let config = crate::config::resolve(
            &Default::default(),
            &Default::default(),
            &Default::default(),
        )
        .unwrap();
        self.snapshot_configuration(run.id, &config, run.created_at)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run() -> RunRecord {
        RunRecord {
            id: RunId::generate(),
            status: "active".into(),
            created_at: 10,
            stopped_at: None,
        }
    }

    #[test]
    fn run_and_configuration_roll_back_together() {
        let mut store = Store::open_in_memory().unwrap();
        let run = run();
        let result: Result<(), StoreError> =
            store.transaction(|repositories| {
                repositories.insert_test_run(&run)?;
                assert!(repositories.has_run_configuration(run.id)?);
                Err(StoreError::RunNotActive { id: run.id })
            });
        assert!(result.is_err());
        store
            .transaction(|repositories| {
                assert!(repositories.run(run.id)?.is_none());
                assert!(!repositories.has_run_configuration(run.id)?);
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn current_schema_never_infers_missing_or_invalid_policy() {
        for damage in ["missing", "schema", "fingerprint", "role", "document"] {
            let mut store = Store::open_in_memory().unwrap();
            let run = run();
            store
                .transaction(|repositories| {
                    repositories.insert_run(&run)?;
                    if damage == "missing" {
                        return Ok(());
                    }
                    let config = crate::config::resolve(
                        &Default::default(),
                        &Default::default(),
                        &Default::default(),
                    )
                    .unwrap();
                    let snapshot = RunConfiguration::new(config);
                    let mut document = serde_json::to_value(&snapshot)?;
                    match damage {
                        "role" => {
                            document["effective"]["roles"]
                                .as_object_mut()
                                .unwrap()
                                .remove("worker");
                        }
                        "document" => document["schema_version"] = json!(2),
                        _ => (),
                    }
                    repositories.insert_configuration_snapshot(
                        &ConfigurationSnapshotRecord {
                            id: 1,
                            run_id: run.id,
                            project_id: None,
                            scope: "run".into(),
                            schema_version: if damage == "schema" {
                                2
                            } else {
                                1
                            },
                            fingerprint: if damage == "fingerprint" {
                                "0".repeat(64)
                            } else {
                                snapshot.fingerprint()
                            },
                            document,
                            created_at: 10,
                        },
                    )
                })
                .unwrap();
            assert!(
                matches!(
                    store.configuration(run.id),
                    Err(StoreError::InvalidConfigurationSnapshot { .. })
                ),
                "{damage}"
            );
        }
    }

    #[test]
    fn spawn_rate_uses_committed_operations_in_a_rolling_window() {
        let mut store = Store::open_in_memory().unwrap();
        let run = run();
        store
            .transaction(|repositories| repositories.insert_test_run(&run))
            .unwrap();
        let mutation = Mutation {
            id: OperationId::generate(),
            run_id: run.id,
            kind: "agent.spawn".into(),
            actor_agent_id: None,
            request: json!({}),
            created_at: 10,
        };
        store.mutate(&mutation, |_| Ok(())).unwrap();
        store
            .mutate::<()>(&mutation, |_| {
                panic!("an idempotent retry cannot spawn again")
            })
            .unwrap();
        store
            .transaction(|repositories| {
                assert_eq!(repositories.recent_spawns(run.id, 69)?, 1);
                assert_eq!(repositories.recent_spawns(run.id, 70)?, 0);
                assert_eq!(
                    repositories.recent_spawns(RunId::generate(), 69)?,
                    0
                );
                Ok(())
            })
            .unwrap();
    }
}

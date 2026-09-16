use super::*;

fn historical_run(fixture: &TestEnvironment, version: i64) -> (PathBuf, Value) {
    write_global(fixture, "[supervision]\nidle_timeout_seconds = 0");
    let mut supervisor = fixture
        .command()
        .args(["__supervisor", RUN_ID, PROJECT_ID])
        .arg(&fixture.project)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    wait_until("upgrade fixture supervisor", || {
        fixture.index_entry_count() == 1
    });
    let task = fixture.run_json(&["task", "create", "Retained work", "--json"]);
    supervisor.kill().unwrap();
    supervisor.wait().unwrap();

    let database = fixture
        .state
        .join("coterie/runs")
        .join(RUN_ID)
        .join("state.sqlite3");
    let prior = database.with_extension("prior");
    let connection = rusqlite::Connection::open(&prior).unwrap();
    connection
        .execute(
            "ATTACH DATABASE ?1 AS current",
            [database.to_str().unwrap()],
        )
        .unwrap();
    connection.execute_batch("CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY, name TEXT NOT NULL, source TEXT NOT NULL, applied_at INTEGER NOT NULL DEFAULT (unixepoch())) STRICT;").unwrap();
    let migrations = connection
        .prepare("SELECT source FROM current.schema_migrations WHERE version <= ?1 ORDER BY version")
        .unwrap()
        .query_map([version], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    for sql in migrations {
        connection.execute_batch(&sql).unwrap();
    }
    connection.execute("INSERT INTO schema_migrations SELECT * FROM current.schema_migrations WHERE version <= ?1", [version]).unwrap();
    // Build the actual historical schema instead of merely rewinding its ledger.
    for table in ["runs", "projects", "tasks", "operations", "events"] {
        connection
            .execute_batch(&format!(
                "INSERT INTO {table} SELECT * FROM current.{table}"
            ))
            .unwrap();
    }
    if version >= 11 {
        connection.execute_batch("INSERT INTO configuration_snapshots SELECT * FROM current.configuration_snapshots; DROP TRIGGER configuration_snapshots_are_append_only;").unwrap();
        if version < 13 {
            connection.execute_batch("UPDATE configuration_snapshots SET document_json = json_remove(document_json, '$.effective.supervision.idle_timeout_seconds', '$.provenance.\"supervision.idle_timeout_seconds\"');").unwrap();
        }
        if version < 12 {
            connection.execute_batch("UPDATE configuration_snapshots SET document_json = json_remove(document_json, '$.effective.allowed_project_roots', '$.provenance.allowed_project_roots');").unwrap();
        }
        connection.execute_batch("CREATE TRIGGER configuration_snapshots_are_append_only BEFORE UPDATE ON configuration_snapshots BEGIN SELECT RAISE(ABORT, 'configuration snapshots are append-only'); END;").unwrap();
    }
    drop(connection);
    fs::set_permissions(&prior, fs::Permissions::from_mode(0o600)).unwrap();
    fs::rename(prior, &database).unwrap();
    (database, task["data"]["task"].clone())
}

#[test]
fn startup_upgrades_historical_snapshots_before_validation() {
    for version in 10..=14 {
        let fixture = TestEnvironment::new();
        let (database, task) = historical_run(&fixture, version);
        let index = fs::read(fixture.only_index_entry()).unwrap();
        fixture.launch(&[]);
        assert_eq!(fs::read(fixture.only_index_entry()).unwrap(), index);
        let tasks = fixture.run_json(&["task", "ready", "--json"]);
        assert_eq!(tasks["data"]["tasks"][0], task);
        let connection = rusqlite::Connection::open(&database).unwrap();
        let document: String = connection.query_row("SELECT document_json FROM configuration_snapshots WHERE scope = 'run'", [], |row| row.get(0)).unwrap();
        let snapshot: Value = serde_json::from_str(&document).unwrap();
        assert_eq!(
            snapshot["effective"]["supervision"]["idle_timeout_seconds"],
            0
        );
        fixture.launch(&[]);
        let unchanged: String = connection.query_row("SELECT document_json FROM configuration_snapshots WHERE scope = 'run'", [], |row| row.get(0)).unwrap();
        assert_eq!(unchanged, document);
        fixture.run_json(&["stop", "--json"]);
    }
}

#[test]
fn stop_upgrades_historical_snapshots_without_adopting_current_policy() {
    for version in 10..=14 {
        let fixture = TestEnvironment::new();
        let (database, task) = historical_run(&fixture, version);
        write_global(&fixture, "invalid configuration");
        let stopped = fixture.run_json(&["stop", "--json"]);
        assert_eq!(stopped["data"]["run_id"], RUN_ID);
        assert_eq!(fixture.index_entry_count(), 0);
        let connection = rusqlite::Connection::open(&database).unwrap();
        let retained: String = connection
            .query_row("SELECT id FROM tasks", [], |row| row.get(0))
            .unwrap();
        assert_eq!(retained, task["id"]);
        let timeout: i64 = connection.query_row("SELECT json_extract(document_json, '$.effective.supervision.idle_timeout_seconds') FROM configuration_snapshots WHERE scope = 'run'", [], |row| row.get(0)).unwrap();
        assert_eq!(timeout, 0);
    }
}

#[test]
fn doctor_identifies_pending_upgrade_without_decoding_or_mutating_old_policy() {
    let fixture = TestEnvironment::new();
    let (database, _) = historical_run(&fixture, 12);
    let before = fs::read(&database).unwrap();
    let report = fixture.run_json(&["doctor", "--json"]);
    let checks = report["data"]["report"]["checks"].as_array().unwrap();
    let migrations = checks
        .iter()
        .find(|check| check["check"] == "database_migrations")
        .unwrap();
    assert_eq!(migrations["status"], "warning");
    assert!(migrations["message"].as_str().unwrap().contains("pending"));
    let snapshot = checks
        .iter()
        .find(|check| check["check"] == "configuration_snapshot")
        .unwrap();
    assert_eq!(snapshot["status"], "unavailable");
    assert!(snapshot["message"].as_str().unwrap().contains("migration"));
    assert!(
        !snapshot["message"]
            .as_str()
            .unwrap()
            .contains("`coterie doctor`")
    );
    assert_eq!(fs::read(database).unwrap(), before);
}

#[test]
fn upgraded_runs_still_reject_changed_policy_before_launch_or_reconciliation() {
    let fixture = TestEnvironment::new();
    let (database, _) = historical_run(&fixture, 12);
    write_global(&fixture, "");
    let output = run(fixture.command());
    assert!(!output.status.success(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("changed: supervision"),
        "{output:?}"
    );
    let connection = rusqlite::Connection::open(&database).unwrap();
    let sessions: i64 = connection
        .query_row("SELECT count(*) FROM sessions", [], |row| row.get(0))
        .unwrap();
    assert_eq!(sessions, 0);
    let document: String = connection.query_row("SELECT document_json FROM configuration_snapshots WHERE scope = 'run'", [], |row| row.get(0)).unwrap();
    let snapshot: Value = serde_json::from_str(&document).unwrap();
    assert_eq!(
        snapshot["effective"]["supervision"]["idle_timeout_seconds"],
        0
    );
    write_global(&fixture, "[supervision]\nidle_timeout_seconds = 0");
    fixture.launch(&[]);
    fixture.run_json(&["stop", "--json"]);
}

#[test]
fn invalid_migration_history_cannot_defer_snapshot_validation() {
    for (damage, expected) in [
        (
            "UPDATE schema_migrations SET source = 'changed' WHERE version = 12",
            "has been modified",
        ),
        (
            "INSERT INTO schema_migrations (version, name, source) VALUES (999, 'future', 'future')",
            "newer than supported",
        ),
        (
            "DELETE FROM schema_migrations WHERE version = 11",
            "noncontiguous",
        ),
    ] {
        let fixture = TestEnvironment::new();
        let (database, _) = historical_run(&fixture, 12);
        let connection = rusqlite::Connection::open(&database).unwrap();
        connection.execute_batch(damage).unwrap();
        drop(connection);
        let before = fs::read(&database).unwrap();
        let output = run(fixture.command());
        assert!(!output.status.success(), "{output:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(expected),
            "{output:?}"
        );
        let report = fixture.run_json(&["doctor", "--json"]);
        let checks = report["data"]["report"]["checks"].as_array().unwrap();
        for name in ["database_migrations", "configuration_snapshot"] {
            let check =
                checks.iter().find(|check| check["check"] == name).unwrap();
            assert_eq!(check["status"], "error");
            assert!(check["message"].as_str().unwrap().contains(expected));
        }
        assert_eq!(fs::read(database).unwrap(), before);
    }
}

#[test]
fn migration_deferral_never_accepts_corrupt_snapshots() {
    for (version, damage) in [
        (
            12,
            "UPDATE configuration_snapshots SET fingerprint = 'changed'",
        ),
        (
            15,
            "UPDATE configuration_snapshots SET document_json = json_remove(document_json, '$.effective.supervision.idle_timeout_seconds')",
        ),
    ] {
        let fixture = TestEnvironment::new();
        let (database, _) = historical_run(&fixture, version);
        let connection = rusqlite::Connection::open(&database).unwrap();
        connection
            .execute_batch(
                "DROP TRIGGER configuration_snapshots_are_append_only;",
            )
            .unwrap();
        connection.execute_batch(damage).unwrap();
        connection.execute_batch("CREATE TRIGGER configuration_snapshots_are_append_only BEFORE UPDATE ON configuration_snapshots BEGIN SELECT RAISE(ABORT, 'configuration snapshots are append-only'); END;").unwrap();
        drop(connection);
        let output = run(fixture.command());
        assert!(!output.status.success(), "{output:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("missing or invalid configuration snapshot"),
            "{output:?}"
        );
        let connection = rusqlite::Connection::open(&database).unwrap();
        let sessions: i64 = connection
            .query_row("SELECT count(*) FROM sessions", [], |row| row.get(0))
            .unwrap();
        assert_eq!(sessions, 0);
    }
}

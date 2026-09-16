use super::*;

fn supervisor(fixture: &TestEnvironment) -> std::process::Child {
    let child = fixture
        .command()
        .args(["__supervisor", RUN_ID, PROJECT_ID])
        .arg(&fixture.project)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    wait_until("supervisor publication", || {
        fixture.index_entry_count() == 1
    });
    child
}

#[test]
fn offline_stop_from_an_attached_project_retires_the_same_run() {
    let fixture = TestEnvironment::new();
    let mut supervisor = supervisor(&fixture);
    let library = fixture.root.join("library");
    Repository::init(&library).unwrap();
    fixture.run_json(&[
        "project",
        "attach",
        library.to_str().unwrap(),
        "--json",
    ]);
    supervisor.kill().unwrap();
    supervisor.wait().unwrap();
    write_global(&fixture, "invalid configuration");
    let output = run({
        let mut command = fixture.command();
        command.current_dir(&library).args(["stop", "--json"]);
        command
    });
    assert!(output.status.success(), "{output:?}");
    let stopped: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stopped["data"]["run_id"], RUN_ID);
    assert_eq!(fixture.index_entry_count(), 0);
}

#[test]
fn offline_stop_refuses_missing_state_and_unsafe_sockets() {
    for damage in ["database", "socket"] {
        let fixture = TestEnvironment::new();
        let mut supervisor = supervisor(&fixture);
        supervisor.kill().unwrap();
        supervisor.wait().unwrap();
        let index = fixture.only_index_entry();
        let original_index = fs::read(&index).unwrap();
        let database = fixture
            .state
            .join("coterie/runs")
            .join(RUN_ID)
            .join("state.sqlite3");
        let socket = fixture
            .runtime
            .join("coterie")
            .join(format!("{RUN_ID}.sock"));
        fs::remove_file(&socket).unwrap();
        if damage == "database" {
            fs::rename(&database, database.with_extension("preserved"))
                .unwrap();
        } else {
            fs::write(&socket, "preserve this file").unwrap();
        }
        let output = run({
            let mut command = fixture.command();
            command.args(["stop", "--json"]);
            command
        });
        assert!(!output.status.success(), "{output:?}");
        assert_eq!(fs::read(&index).unwrap(), original_index);
        if damage == "database" {
            assert!(!database.exists());
        } else {
            assert_eq!(
                fs::read_to_string(&socket).unwrap(),
                "preserve this file"
            );
            let connection = rusqlite::Connection::open(&database).unwrap();
            let shutdowns: i64 = connection
                .query_row("SELECT count(*) FROM run_shutdowns", [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(shutdowns, 0);
        }
    }
}

#[test]
fn offline_stop_replays_completion_after_interrupted_retirement() {
    let fixture = TestEnvironment::new();
    assert!(run(fixture.connect_command()).status.success());
    let index = fixture.only_index_entry();
    let original_index = fs::read(&index).unwrap();
    let operation = "co-01ARZ3NDEKTSV4RRFFQ69G5FAX";
    let arguments = ["stop", "--operation-id", operation, "--json"];
    let stopped = fixture.run_json(&arguments);
    fs::write(&index, original_index).unwrap();
    fs::set_permissions(&index, fs::Permissions::from_mode(0o600)).unwrap();
    write_global(&fixture, "invalid configuration");
    assert_eq!(fixture.run_json(&arguments), stopped);
    assert_eq!(fixture.index_entry_count(), 0);
    let database = fixture
        .state
        .join("coterie/runs")
        .join(stopped["data"]["run_id"].as_str().unwrap())
        .join("state.sqlite3");
    let connection = rusqlite::Connection::open(&database).unwrap();
    let events: i64 = connection
        .query_row(
            "SELECT count(*) FROM events WHERE event_type = 'run.stopped'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(events, 1);
}

#[test]
fn stop_preserves_uncertain_foreground_sessions_but_accepts_proven_absence() {
    let fixture = TestEnvironment::new();
    write_global(&fixture, "[supervision]\nshutdown_timeout_ms = 500");
    let mut supervisor = fixture
        .command()
        .args(["__supervisor", RUN_ID, PROJECT_ID])
        .arg(&fixture.project)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    wait_until("supervisor publication", || {
        fixture.index_entry_count() == 1
    });
    fixture.launch(&[]);
    supervisor.kill().unwrap();
    supervisor.wait().unwrap();
    let database = fixture
        .state
        .join("coterie/runs")
        .join(RUN_ID)
        .join("state.sqlite3");
    let connection = rusqlite::Connection::open(&database).unwrap();
    // Reproduce a lost wrapper observation without relying on a process race.
    connection.execute("UPDATE sessions SET state = 'running', ended_at = NULL, provider_session_id = ?1", [format!("process:{}", std::process::id())]).unwrap();
    connection
        .execute("UPDATE agents SET state = 'running'", [])
        .unwrap();
    let operation = "co-01ARZ3NDEKTSV4RRFFQ69G5FAX";
    let output = run({
        let mut command = fixture.command();
        command.args(["stop", "--operation-id", operation, "--json"]);
        command
    });
    assert_eq!(output.status.code(), Some(7), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("timed out waiting for agent processes"),
        "{output:?}"
    );
    let session: (String, String) = connection
        .query_row(
            "SELECT state, reconciliation_state FROM sessions",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(session, ("unknown".into(), "unknown".into()));
    assert_eq!(fixture.index_entry_count(), 1);
    // The adapter can prove ESRCH for this nonexistent PID, without claiming
    // an exit status or adopting a live process from its PID alone.
    connection
        .execute(
            "UPDATE sessions SET provider_session_id = 'process:2147483647'",
            [],
        )
        .unwrap();
    let stopped =
        fixture.run_json(&["stop", "--operation-id", operation, "--json"]);
    assert_eq!(stopped["data"]["status"], "stopped");
    let session: (String, String) = connection
        .query_row(
            "SELECT state, reconciliation_state FROM sessions",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(session, ("lost".into(), "lost".into()));
}

#[test]
fn stop_recovers_an_offline_run_using_its_saved_policy() {
    for missing_socket in [false, true] {
        for configuration in ["", "invalid configuration"] {
            let fixture = TestEnvironment::new();
            write_global(&fixture, "[supervision]\nidle_timeout_seconds = 0");
            let mut supervisor = fixture
                .command()
                .args(["__supervisor", RUN_ID, PROJECT_ID])
                .arg(&fixture.project)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            wait_until("supervisor publication", || {
                fixture.index_entry_count() == 1
            });
            let task = fixture.run_json(&[
                "task",
                "create",
                "Retained work",
                "--json",
            ]);
            supervisor.kill().unwrap();
            supervisor.wait().unwrap();
            let socket = fixture
                .runtime
                .join("coterie")
                .join(format!("{RUN_ID}.sock"));
            if missing_socket {
                fs::remove_file(&socket).unwrap();
            }
            let database = fixture
                .state
                .join("coterie/runs")
                .join(RUN_ID)
                .join("state.sqlite3");
            let connection = rusqlite::Connection::open(&database).unwrap();
            let snapshot: String = connection.query_row("SELECT document_json FROM configuration_snapshots WHERE scope = 'run'", [], |row| row.get(0)).unwrap();
            write_global(&fixture, configuration);
            let operation = "co-01ARZ3NDEKTSV4RRFFQ69G5FAX";
            let stopped = fixture.run_json(&[
                "stop",
                "--operation-id",
                operation,
                "--json",
            ]);
            assert_eq!(stopped["data"]["run_id"], RUN_ID);
            assert_eq!(stopped["data"]["status"], "stopped");
            assert_eq!(stopped["operation_id"], operation);
            assert_eq!(fixture.index_entry_count(), 0);
            assert!(!socket.exists());
            let retained: String = connection.query_row("SELECT document_json FROM configuration_snapshots WHERE scope = 'run'", [], |row| row.get(0)).unwrap();
            assert_eq!(retained, snapshot);
            let retained_task: String = connection
                .query_row("SELECT id FROM tasks", [], |row| row.get(0))
                .unwrap();
            assert_eq!(retained_task, task["data"]["task"]["id"]);
            let sessions: i64 = connection
                .query_row("SELECT count(*) FROM sessions", [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(sessions, 0);
            let state: (String, String, String) = connection.query_row("SELECT runs.status, run_shutdowns.phase, run_shutdowns.operation_id FROM runs JOIN run_shutdowns ON runs.id = run_shutdowns.run_id", [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))).unwrap();
            assert_eq!(
                state,
                ("stopped".into(), "completed".into(), operation.into())
            );
        }
    }
}

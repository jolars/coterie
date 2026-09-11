use super::*;

fn review_script() -> String {
    FAKE_CODEX.replace(
        "  \"$coterie\" inbox ack 1 --json > /dev/null || exit 21\n",
        "",
    )
}

#[test]
fn sequential_reviews_preserve_assignments_and_replay_spawns() {
    let fixture = TestEnvironment::new();
    fs::write(fixture.root.join("bin/codex"), review_script()).unwrap();
    fixture.launch(&[]);
    let status = fixture.run_json(&["status", "--json"]);
    let run_id = status["data"]["run_id"].as_str().unwrap();
    let database = fixture
        .state
        .join("coterie/runs")
        .join(run_id)
        .join("state.sqlite3");
    let socket = fixture
        .runtime
        .join("coterie")
        .join(format!("{run_id}.sock"));
    fs::write(socket.with_extension("sock.release"), "release").unwrap();
    let mut assignments = Vec::new();
    for title in ["First review", "Second review"] {
        let task = fixture.run_json(&["task", "create", title, "--json"]);
        let task_id = task["data"]["task"]["id"].as_str().unwrap();
        let operation = format!("co-{}", ulid::Ulid::generate());
        let args = [
            "spawn",
            "reviewer",
            "--task",
            task_id,
            "--operation-id",
            &operation,
            "--json",
        ];
        let spawned = fixture.run_json(&args);
        assignments.push(
            spawned["data"]["assignment_id"]
                .as_str()
                .unwrap()
                .to_owned(),
        );
        wait_until("review process exit", || {
            let connection = rusqlite::Connection::open(&database).unwrap();
            connection.query_row("SELECT state = 'exited' AND reconciliation_state = 'observed' FROM sessions WHERE id = ?1", [spawned["data"]["session_id"].as_str().unwrap()], |row| row.get::<_, bool>(0)).unwrap()
        });
        let replayed = fixture.run_json(&args);
        for field in ["assignment_id", "session_id", "task_id"] {
            assert_eq!(replayed["data"][field], spawned["data"][field]);
        }
        assert_eq!(
            replayed["data"]["agent"]["id"],
            spawned["data"]["agent"]["id"]
        );
        fixture.run_json(&[
            "task",
            "close",
            task_id,
            "--summary",
            "Reviewed and validated",
            "--json",
        ]);
    }
    assert_ne!(assignments[0], assignments[1]);
    let connection = rusqlite::Connection::open(&database).unwrap();
    let count: i64 = connection.query_row("SELECT count(*) FROM workspaces JOIN assignments ON assignments.id = workspaces.assignment_id WHERE workspaces.kind = 'read_only' AND assignments.state = 'completed' AND workspaces.path = ?1", [fixture.project.as_os_str().as_bytes()], |row| row.get(0)).unwrap();
    assert_eq!(count, 2);
    fixture.run_json(&["stop", "--json"]);
}

#[test]
fn custom_shared_workspaces_preserve_limits_and_recover_without_duplicate_bindings()
 {
    for kind in ["read-only", "project"] {
        for git in [true, false] {
            let fixture = TestEnvironment::new_with_repository(git);
            let config = include_str!("../../examples/config/global.toml")
                .replace(
                    "workspace = \"worktree\"",
                    &format!("workspace = \"{kind}\""),
                )
                .replace(
                    "permission_profile = \"implementation\"",
                    if kind == "read-only" {
                        "permission_profile = \"inspect\""
                    } else {
                        "permission_profile = \"interactive\""
                    },
                )
                .replace("max_instances = 3", "max_instances = 2");
            write_global(&fixture, &config);
            let script = review_script().replace("  printf '{\"type\":\"turn.completed\",\"usage\":{}}\\n'", "  while [ ! -e \"$COTERIE_SOCKET.exit\" ]; do :; done\n  printf '{\"type\":\"turn.completed\",\"usage\":{}}\\n'");
            fs::write(fixture.root.join("bin/codex"), script).unwrap();
            let mut command = fixture.command();
            command
                .args(["__supervisor", RUN_ID, PROJECT_ID])
                .arg(&fixture.project)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped());
            let mut supervisor = command.spawn().unwrap();
            wait_until("custom supervisor", || {
                fixture.index_entry_count() == 1
            });
            let database = fixture
                .state
                .join("coterie/runs")
                .join(RUN_ID)
                .join("state.sqlite3");
            let connection = rusqlite::Connection::open(&database).unwrap();
            let socket = fixture
                .runtime
                .join("coterie")
                .join(format!("{RUN_ID}.sock"));
            let first =
                fixture.run_json(&["task", "create", "First", "--json"]);
            let first_id = first["data"]["task"]["id"].as_str().unwrap();
            let second =
                fixture.run_json(&["task", "create", "Second", "--json"]);
            let second_id = second["data"]["task"]["id"].as_str().unwrap();
            let operation = format!("co-{}", ulid::Ulid::generate());
            let args = [
                "spawn",
                "builder",
                "--task",
                first_id,
                "--operation-id",
                &operation,
                "--json",
            ];
            let spawned = fixture.run_json(&args);
            let next_operation = format!("co-{}", ulid::Ulid::generate());
            let next_args = [
                "spawn",
                "builder",
                "--task",
                second_id,
                "--operation-id",
                &next_operation,
                "--json",
            ];
            if kind == "read-only" {
                let mut one = fixture.command();
                one.args(next_args).stdout(Stdio::piped());
                let mut two = fixture.command();
                two.args(next_args).stdout(Stdio::piped());
                let first_request = one.spawn().unwrap();
                let second_request = two.spawn().unwrap();
                let a = first_request.wait_with_output().unwrap();
                let b = second_request.wait_with_output().unwrap();
                assert!(
                    a.status.success() && b.status.success(),
                    "{a:?} {b:?}"
                );
                assert_eq!(a.stdout, b.stdout);
                let third =
                    fixture.run_json(&["task", "create", "Third", "--json"]);
                rejected(
                    &fixture,
                    &[
                        "spawn",
                        "builder",
                        "--task",
                        third["data"]["task"]["id"].as_str().unwrap(),
                        "--json",
                    ],
                    "active instance limit",
                );
            } else {
                rejected(&fixture, &next_args, "workspace path is reserved");
            }
            fs::write(socket.with_extension("sock.release"), "release")
                .unwrap();
            wait_until("submission before process exit", || {
                connection.query_row("SELECT state = 'completed' FROM assignments WHERE id = ?1", [spawned["data"]["assignment_id"].as_str().unwrap()], |row| row.get::<_, bool>(0)).unwrap()
            });
            if kind == "project" {
                rejected(&fixture, &next_args, "workspace path is reserved");
                assert_eq!(
                    connection
                        .query_row(
                            "SELECT count(*) FROM operations WHERE id = ?1",
                            [&next_operation],
                            |row| row.get::<_, i64>(0)
                        )
                        .unwrap(),
                    0
                );
            }
            fs::write(socket.with_extension("sock.exit"), "exit").unwrap();
            wait_until("all custom jobs exited", || {
                connection.query_row("SELECT count(*) FROM sessions WHERE state <> 'exited' OR reconciliation_state <> 'observed'", [], |row| row.get::<_, i64>(0)).unwrap() == 0
            });
            let before = durable_restart_snapshot(&connection);
            supervisor.kill().unwrap();
            supervisor.wait().unwrap();
            let restarted = run(fixture.connect_command());
            assert!(restarted.status.success(), "{restarted:?}");
            assert_eq!(durable_restart_snapshot(&connection), before);
            let replayed = fixture.run_json(&args);
            assert_eq!(
                replayed["data"]["assignment_id"],
                spawned["data"]["assignment_id"]
            );
            fixture.run_json(&next_args);
            wait_until("two completed bindings", || {
                connection.query_row("SELECT count(*) FROM assignments WHERE state = 'completed'", [], |row| row.get::<_, i64>(0)).unwrap() == 2
            });
            let count = connection
                .query_row(
                    "SELECT count(*) FROM workspaces WHERE path = ?1",
                    [fixture.project.as_os_str().as_bytes()],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap();
            assert_eq!(count, 2);
            for task in [first_id, second_id] {
                fixture.run_json(&[
                    "task",
                    "close",
                    task,
                    "--summary",
                    "Validated",
                    "--json",
                ]);
            }
            fixture.run_json(&["stop", "--json"]);
        }
    }
}

#[test]
fn recovery_resumes_a_second_review_intent_and_preserves_the_first() {
    let fixture = TestEnvironment::new();
    let executable = fixture.root.join("bin/codex");
    fs::write(&executable, review_script()).unwrap();
    let mut command = fixture.command();
    command
        .args(["__supervisor", RUN_ID, PROJECT_ID])
        .arg(&fixture.project)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let mut supervisor = command.spawn().unwrap();
    wait_until("review supervisor", || fixture.index_entry_count() == 1);
    let database = fixture
        .state
        .join("coterie/runs")
        .join(RUN_ID)
        .join("state.sqlite3");
    let connection = rusqlite::Connection::open(&database).unwrap();
    let release = fixture
        .runtime
        .join("coterie")
        .join(format!("{RUN_ID}.sock.release"));
    fs::write(release, "release").unwrap();
    let first = fixture.run_json(&["task", "create", "First review", "--json"]);
    let first_id = first["data"]["task"]["id"].as_str().unwrap();
    fixture.run_json(&["spawn", "reviewer", "--task", first_id, "--json"]);
    wait_until("first review exit", || {
        connection.query_row("SELECT count(*) FROM sessions WHERE state <> 'exited' OR reconciliation_state <> 'observed'", [], |row| row.get::<_, i64>(0)).unwrap() == 0
    });
    let previous: String = connection.query_row("SELECT json_object('assignment', assignments.id, 'state', assignments.state, 'summary', assignments.summary, 'session', assignments.session_id, 'path', hex(workspaces.path), 'generation', workspaces.generation) FROM assignments JOIN workspaces ON workspaces.assignment_id = assignments.id WHERE assignments.task_id = ?1", [first_id], |row| row.get(0)).unwrap();
    let second =
        fixture.run_json(&["task", "create", "Second review", "--json"]);
    let second_id = second["data"]["task"]["id"].as_str().unwrap();
    let operation = format!("co-{}", ulid::Ulid::generate());
    let args = [
        "spawn",
        "reviewer",
        "--task",
        second_id,
        "--operation-id",
        &operation,
        "--json",
    ];
    fs::write(&executable, "#!/bin/sh\nexit 1\n").unwrap();
    let failed = run({
        let mut command = fixture.command();
        command.args(args);
        command
    });
    assert!(!failed.status.success(), "{failed:?}");
    let desired: String = connection
        .query_row(
            "SELECT reconciliation_state FROM operations WHERE id = ?1",
            [&operation],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(desired, "desired");
    let assignment: String = connection
        .query_row(
            "SELECT id FROM assignments WHERE task_id = ?1",
            [second_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM workspaces", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        2
    );
    supervisor.kill().unwrap();
    supervisor.wait().unwrap();
    fs::write(&executable, review_script()).unwrap();
    let restarted = run(fixture.connect_command());
    assert!(restarted.status.success(), "{restarted:?}");
    wait_until("recovered second review", || {
        connection
            .query_row(
                "SELECT state = 'completed' FROM assignments WHERE id = ?1",
                [&assignment],
                |row| row.get::<_, bool>(0),
            )
            .unwrap()
    });
    for _ in 0..2 {
        let replayed = fixture.run_json(&args);
        assert_eq!(replayed["data"]["assignment_id"], assignment);
    }
    let preserved: String = connection.query_row("SELECT json_object('assignment', assignments.id, 'state', assignments.state, 'summary', assignments.summary, 'session', assignments.session_id, 'path', hex(workspaces.path), 'generation', workspaces.generation) FROM assignments JOIN workspaces ON workspaces.assignment_id = assignments.id WHERE assignments.task_id = ?1", [first_id], |row| row.get(0)).unwrap();
    assert_eq!(preserved, previous);
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM workspaces", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        2
    );
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM sessions", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        2
    );
    fixture.run_json(&["stop", "--json"]);
}

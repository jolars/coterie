use super::*;

#[test]
fn progress_wait_allows_mutations_and_resumes_after_reader_disconnect() {
    let fixture = TestEnvironment::new();
    fixture.launch(&[]);
    let initial = fixture.run_json(&["progress", "--json"]);
    let cursor = initial["data"]["next_cursor"].as_str().unwrap();
    let mut command = fixture.command();
    command
        .args(["progress", "--after", cursor, "--wait", "5", "--json"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut waiting = command.spawn().unwrap();
    thread::sleep(Duration::from_millis(200));
    assert!(waiting.try_wait().unwrap().is_none());
    let created = fixture.run_json(&[
        "task",
        "create",
        "Private title",
        "--description",
        "Private body",
        "--json",
    ]);
    let task = &created["data"]["task"]["id"];
    wait_until("progress wait to observe a concurrent mutation", || {
        waiting.try_wait().unwrap().is_some()
    });
    let output = waiting.wait_with_output().unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty());
    let observed: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(observed["data"]["timed_out"], false);
    assert!(
        observed["data"]["changes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["task_id"] == *task)
    );
    assert!(!String::from_utf8_lossy(&output.stdout).contains("Private"));
    let cursor = observed["data"]["next_cursor"].as_str().unwrap();
    let timeout = fixture
        .run_json(&["progress", "--after", cursor, "--wait", "1", "--json"]);
    assert_eq!(timeout["data"]["timed_out"], true);
    assert_eq!(timeout["data"]["changes"], serde_json::json!([]));
    let mut disconnect = fixture.command();
    disconnect
        .args(["progress", "--after", cursor, "--wait", "5", "--json"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut disconnected = disconnect.spawn().unwrap();
    thread::sleep(Duration::from_millis(150));
    disconnected.kill().unwrap();
    disconnected.wait().unwrap();
    let second =
        fixture.run_json(&["task", "create", "After disconnect", "--json"]);
    let resumed = fixture
        .run_json(&["progress", "--after", cursor, "--limit", "1", "--json"]);
    assert!(
        resumed["data"]["changes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["task_id"] == second["data"]["task"]["id"])
    );
    assert_eq!(
        fixture.run_json(&[
            "progress", "--after", cursor, "--limit", "1", "--json"
        ]),
        resumed
    );
    fixture.run_json(&["stop", "--json"]);
}

#[test]
fn progress_cursor_reconnects_to_durable_state_after_supervisor_restart() {
    let fixture = TestEnvironment::new();
    let mut command = fixture.command();
    command
        .args(["__supervisor", RUN_ID, PROJECT_ID])
        .arg(&fixture.project)
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let mut supervisor = command.spawn().unwrap();
    wait_until("initial supervisor publication", || {
        fixture.index_entry_count() == 1
    });
    let initial = fixture.run_json(&["progress", "--json"]);
    let cursor = initial["data"]["next_cursor"].as_str().unwrap();
    fixture.run_json(&["task", "create", "First durable change", "--json"]);
    let before = fixture.run_json(&["progress", "--after", cursor, "--json"]);
    supervisor.kill().unwrap();
    supervisor.wait().unwrap();
    let unavailable = run({
        let mut command = fixture.command();
        command.args(["progress", "--after", cursor, "--json"]);
        command
    });
    assert_eq!(unavailable.status.code(), Some(7), "{unavailable:?}");
    assert!(unavailable.stdout.is_empty());
    let restarted = run(fixture.connect_command());
    assert!(restarted.status.success(), "{restarted:?}");
    let after = fixture.run_json(&["progress", "--after", cursor, "--json"]);
    assert_eq!(after, before);
    fixture.run_json(&["stop", "--json"]);
}

#[test]
fn progress_agent_cli_requires_capability_and_preserves_operator_boundaries() {
    for allowed in [false, true] {
        let fixture = TestEnvironment::new();
        let config = include_str!("../../examples/config/global.toml").replace(
            "\"spawn:builder\", \"send:*\", \"task:*\", \"logs:*\"",
            if allowed {
                "\"task:read\""
            } else {
                "\"send:*\""
            },
        );
        write_global(&fixture, &config);
        let capture = fixture.root.join("agent-environment");
        let mut command = fixture.command();
        command
            .env("COTERIE_FAKE_MODE", "contract")
            .env("COTERIE_FAKE_CAPTURE", &capture)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let mut foreground = command.spawn().unwrap();
        wait_until("complete custom-role environment", || {
            fs::read(&capture).is_ok_and(|bytes| {
                bytes.ends_with(&[0])
                    && String::from_utf8_lossy(&bytes)
                        .contains("env:COTERIE_TOKEN=")
            })
        });
        let environment = captured_environment(&capture);
        let output = run({
            let mut command = fixture.agent_command(&environment);
            command.args(["progress", "--json"]);
            command
        });
        if allowed {
            assert!(output.status.success(), "{output:?}");
            assert!(output.stderr.is_empty());
            let initial: Value =
                serde_json::from_slice(&output.stdout).unwrap();
            let cursor = initial["data"]["next_cursor"].as_str().unwrap();
            let mut operation = fixture.command();
            operation.args(["progress", "--after", cursor, "--json"]);
            assert_eq!(run(operation).status.code(), Some(2));
            for command in ["status", "events"] {
                let output = run({
                    let mut request = fixture.agent_command(&environment);
                    request.args([command, "--json"]);
                    request
                });
                assert_eq!(output.status.code(), Some(6));
                assert!(output.stdout.is_empty());
            }
            fixture.run_json(&[
                "task",
                "create",
                "Private lifecycle task",
                "--json",
            ]);
            let changed = fixture.run_agent_json(
                &["progress", "--after", cursor, "--json"],
                &environment,
            );
            assert!(
                changed["data"]["changes"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|c| c["kind"] == "task")
            );
            assert!(!changed.to_string().contains("Private"));
        } else {
            assert_eq!(output.status.code(), Some(6));
            assert!(output.stdout.is_empty());
            let error: Value = serde_json::from_slice(&output.stderr).unwrap();
            assert_eq!(error["error"]["code"], "permission_denied");
        }
        drop(foreground.stdin.take());
        assert!(foreground.wait().unwrap().success());
        let stale = run({
            let mut command = fixture.agent_command(&environment);
            command.args(["progress", "--json"]);
            command
        });
        assert_eq!(stale.status.code(), Some(6));
        let error: Value = serde_json::from_slice(&stale.stderr).unwrap();
        assert_eq!(error["error"]["code"], "unauthenticated");
        fixture.run_json(&["stop", "--json"]);
    }
}

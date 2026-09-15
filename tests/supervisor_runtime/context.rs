use super::*;

#[test]
fn prime_bounds_long_descriptions_and_full_details_remain_available() {
    let fixture = TestEnvironment::new();
    fixture.launch(&[]);
    let description =
        "Long task context with Unicode λ and escaped \"text\".\n".repeat(1000);
    let mut ids = Vec::new();
    for index in 0..5 {
        let created = fixture.run_json(&[
            "task",
            "create",
            &format!("Task {index}"),
            "--description",
            &description,
            "--json",
        ]);
        ids.push(created["data"]["task"]["id"].as_str().unwrap().to_owned());
    }
    for json_output in [false, true] {
        let mut command = fixture.command();
        command.arg("prime");
        if json_output {
            command.arg("--json");
        }
        let output = run(command);
        assert!(output.status.success(), "{output:?}");
        assert!(output.stderr.is_empty());
        assert!(
            output.stdout.len() < 32 * 1024,
            "prime repeated full descriptions: {} bytes",
            output.stdout.len()
        );
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        let data = if json_output { &value["data"] } else { &value };
        assert_eq!(data["tasks"].as_array().unwrap().len(), 5);
        assert_eq!(data["tasks"][0]["description"]["truncated"], true);
    }
    let detail = read_detail(&fixture, "task", &ids[0]);
    assert_eq!(detail["task"]["description"], description);
    fixture.run_json(&["stop", "--json"]);
}

pub(super) fn read_detail(
    fixture: &TestEnvironment,
    kind: &str,
    id: &str,
) -> Value {
    let mut text = String::new();
    let mut after = 0;
    let mut revision: Option<String> = None;
    loop {
        let cursor = after.to_string();
        let mut args = vec![
            kind, "show", id, "--after", &cursor, "--limit", "4096", "--json",
        ];
        if let Some(revision) = &revision {
            args.extend(["--revision", revision]);
        }
        let page = fixture.run_json(&args);
        let data = &page["data"];
        text.push_str(data["text"].as_str().unwrap());
        after = data["next_cursor"].as_u64().unwrap();
        revision = Some(data["revision"].as_str().unwrap().to_owned());
        if data["eof"] == true {
            break;
        }
    }
    serde_json::from_str(&text).unwrap()
}

fn install_captured_provider(fixture: &TestEnvironment) {
    fs::write(
        fixture.root.join("bin/codex"),
        format!(
            "{}{}",
            FAKE_CODEX.split_once("is_job=false").unwrap().0,
            recovery::JOB
        ),
    )
    .unwrap();
}

#[test]
fn long_completed_reports_are_compact_and_current_task_survives_transitions_and_reconnect()
 {
    let fixture = TestEnvironment::new();
    install_captured_provider(&fixture);
    fixture.launch(&[]);
    let description = "Task scope and acceptance criteria.\n".repeat(1500);
    let report = "Validated research evidence, including Unicode λ and escaped \"text\".\n".repeat(900);
    let validation = "Independent acceptance evidence.\n".repeat(1500);
    let mut documents_bytes = 0;
    let mut last_task = String::new();
    for index in 0..5 {
        let task = fixture.run_json(&[
            "task",
            "create",
            &format!("Report {index}"),
            "--description",
            &description,
            "--json",
        ]);
        let id = task["data"]["task"]["id"].as_str().unwrap();
        let spawn =
            fixture.run_json(&["spawn", "reviewer", "--task", id, "--json"]);
        let assignment = spawn["data"]["assignment_id"].as_str().unwrap();
        let (capture, environment, _) =
            recovery::captured_job(&fixture, &spawn);
        let initial =
            fixture.run_agent_json(&["prime", "--json"], &environment);
        assert_eq!(initial["data"]["active_task"]["id"], id);
        assert_eq!(
            initial["data"]["current_task"]["assignment"]["id"],
            assignment
        );
        let first_detail =
            fixture.run_json(&["task", "show", id, "--limit", "256", "--json"]);
        fixture.run_agent_json(
            &[
                "finish",
                "--status",
                "completed",
                "--summary",
                &report,
                "--json",
            ],
            &environment,
        );
        let submitted = fixture
            .run_agent_json(&["prime", "--limit", "1", "--json"], &environment);
        assert!(submitted["data"]["active_task"].is_null());
        assert_eq!(submitted["data"]["current_task"]["id"], id);
        assert_eq!(submitted["data"]["current_task"]["status"], "submitted");
        assert_eq!(
            submitted["data"]["current_task"]["next_action"],
            "validate_and_close"
        );
        assert_eq!(
            submitted["data"]["current_task"]["assignment"]["summary"]["truncated"],
            true
        );
        let changed = run({
            let mut command = fixture.command();
            command.args([
                "task",
                "show",
                id,
                "--after",
                "256",
                "--revision",
                first_detail["data"]["revision"].as_str().unwrap(),
                "--json",
            ]);
            command
        });
        assert_eq!(changed.status.code(), Some(5), "{changed:?}");
        assert!(changed.stdout.is_empty());
        let detail = read_detail(&fixture, "assignment", assignment);
        assert_eq!(detail["assignment"]["summary"], report);
        assert_eq!(detail["assignment"]["task_id"], id);
        documents_bytes += serde_json::to_vec(&detail).unwrap().len();
        fixture.run_json(&[
            "task",
            "close",
            id,
            "--summary",
            &validation,
            "--json",
        ]);
        let closed = fixture
            .run_agent_json(&["prime", "--limit", "1", "--json"], &environment);
        assert_eq!(closed["data"]["current_task"]["status"], "closed");
        assert_eq!(closed["data"]["current_task"]["next_action"], "none");
        if index == 4 {
            let current =
                fixture.run_agent_json(&["prime", "--json"], &environment);
            measure_transcript_inspection(
                &fixture,
                &spawn,
                &capture,
                &environment,
                &current,
            );
        }
        let detail = read_detail(&fixture, "task", id);
        assert_eq!(detail["task"]["description"], description);
        assert_eq!(detail["task"]["result"]["validation_summary"], validation);
        assert_eq!(detail["assignments"], serde_json::json!([assignment]));
        documents_bytes += serde_json::to_vec(&detail).unwrap().len();
        fs::write(format!("{}.exit", capture.display()), "exit").unwrap();
        wait_until("reporting worker exit", || {
            fixture.run_json(&["status", "--json"])["data"]["agents"]
                .as_array()
                .unwrap()
                .iter()
                .any(|agent| {
                    agent["id"] == spawn["data"]["agent"]["id"]
                        && agent["state"] == "exited"
                })
        });
        last_task = id.to_owned();
    }
    let before = fixture.run_json(&["prime", "--json"]);
    fixture.launch(&[]);
    let after = fixture.run_json(&["prime", "--json"]);
    assert_eq!(
        before["data"]["identity"]["run_id"],
        after["data"]["identity"]["run_id"]
    );
    assert_eq!(before["data"]["tasks"], after["data"]["tasks"]);
    assert_eq!(after["data"]["tasks"].as_array().unwrap().len(), 5);
    assert_eq!(after["data"]["tasks"][4]["id"], last_task);
    for json_output in [false, true] {
        let mut command = fixture.command();
        command.arg("prime");
        if json_output {
            command.arg("--json");
        }
        let output = run(command);
        assert!(output.status.success() && output.stderr.is_empty());
        assert!(
            output.stdout.len() <= 32 * 1024,
            "{} bytes",
            output.stdout.len()
        );
        eprintln!(
            "context measurement: json={json_output}, prime_bytes={}, full_document_bytes={documents_bytes}",
            output.stdout.len()
        );
    }
    fixture.run_json(&["stop", "--json"]);
}

#[test]
fn prime_pages_escape_heavy_context_without_skipping_tasks() {
    let fixture = TestEnvironment::new();
    fixture.launch(&[]);
    let text = "\u{1}".repeat(4000);
    let mut expected = Vec::new();
    for index in 0..30 {
        let created = fixture.run_json(&[
            "task",
            "create",
            &format!("{index} {text}"),
            "--description",
            &text,
            "--json",
        ]);
        expected
            .push(created["data"]["task"]["id"].as_str().unwrap().to_owned());
    }
    let mut actual = Vec::new();
    let mut after: Option<String> = None;
    loop {
        let mut args = vec!["prime", "--limit", "50", "--json"];
        if let Some(after) = &after {
            args.extend(["--after-task", after]);
        }
        let page = fixture.run_json(&args);
        assert!(serde_json::to_vec(&page).unwrap().len() < 68 * 1024);
        let tasks = page["data"]["tasks"].as_array().unwrap();
        assert!(!tasks.is_empty());
        actual
            .extend(tasks.iter().map(|t| t["id"].as_str().unwrap().to_owned()));
        after = Some(page["data"]["next_task"].as_str().unwrap().to_owned());
        if page["data"]["has_more"] == false {
            break;
        }
        assert!(
            tasks.len() < 30,
            "the serialized byte budget must shorten this page"
        );
    }
    assert_eq!(actual, expected);
    fixture.run_json(&["stop", "--json"]);
}

fn measure_transcript_inspection(
    fixture: &TestEnvironment,
    spawn: &Value,
    capture: &Path,
    environment: &[(String, String)],
    prime: &Value,
) {
    let mut client = mcp::McpClient::start(fixture.agent_command(environment));
    client.initialize();
    let mcp_prime = client.call("prime", serde_json::json!({}));
    assert_eq!(mcp_prime["result"]["isError"], false);
    assert_eq!(
        mcp_prime["result"]["structuredContent"]["data"]["tasks"],
        prime["data"]["tasks"]
    );
    assert!(serde_json::to_vec(&mcp_prime).unwrap().len() <= 48 * 1024);
    let captured = fs::read_to_string(capture).unwrap();
    let bootstrap = captured
        .split('\0')
        .find(|arg| arg.starts_with("arg:") && arg.contains("Call the `prime`"))
        .unwrap();
    let startup = format!(
        "{}\n",
        serde_json::json!({"type":"fixture.bootstrap", "text":bootstrap})
    );
    let context = format!(
        "{}\n",
        serde_json::json!({"type":"fixture.context", "prime":mcp_prime})
    );
    let activity = "{\"type\":\"fixture.activity\",\"text\":\"Validating the current task.\"}\n";
    let repeated =
        format!("{}{}{activity}", startup.repeat(4), context.repeat(4));
    let pending = format!("{}.output-pending", capture.display());
    fs::write(&pending, &repeated).unwrap();
    fs::rename(pending, format!("{}.output", capture.display())).unwrap();
    let agent = spawn["data"]["agent"]["id"].as_str().unwrap();
    let mut tail = Value::Null;
    wait_until(
        "recent transcript activity after repeated bootstrap and context",
        || {
            tail = fixture.run_json(&[
                "logs", agent, "--tail", "--limit", "4096", "--json",
            ]);
            tail["data"]["transcript"]
                .as_str()
                .unwrap()
                .ends_with(activity)
        },
    );
    let data = &tail["data"];
    assert!(data["start_cursor"].as_u64().unwrap() > 0);
    assert!(data["transcript"].as_str().unwrap().len() <= 4099);
    assert!(serde_json::to_vec(&tail).unwrap().len() <= 26 * 1024);
    let own = fixture.run_agent_json(
        &["logs", agent, "--tail", "--limit", "4096", "--json"],
        environment,
    );
    assert_eq!(own, tail);
    let mcp_tail = client.call(
        "logs",
        serde_json::json!({"agent":agent,"after":0,"limit":4096,"tail":true}),
    );
    assert_eq!(
        mcp_tail["result"]["structuredContent"]["data"]["transcript"],
        data["transcript"]
    );
    let assignment = spawn["data"]["assignment_id"].as_str().unwrap();
    let mut text = String::new();
    let mut after = 0;
    let mut revision: Option<String> = None;
    loop {
        let response = client.call("assignment_show", serde_json::json!({"assignment_id":assignment,"after":after,"revision":revision,"limit":65536}));
        assert_eq!(response["result"]["isError"], false);
        let page = &response["result"]["structuredContent"]["data"];
        text.push_str(page["text"].as_str().unwrap());
        after = page["next_cursor"].as_u64().unwrap();
        revision = Some(page["revision"].as_str().unwrap().to_owned());
        if page["eof"] == true {
            break;
        }
    }
    assert_eq!(
        serde_json::from_str::<Value>(&text).unwrap(),
        read_detail(fixture, "assignment", assignment)
    );
    let mut command = fixture.command();
    command.args(["logs", agent, "--tail", "--limit", "4096"]);
    let human = run(command);
    assert!(human.status.success() && human.stderr.is_empty());
    assert_eq!(
        serde_json::from_slice::<Value>(&human.stdout).unwrap(),
        *data
    );
    let mut full = String::new();
    let mut cursor = 0;
    let mut pages = 0;
    let mut serialized_bytes = 0;
    loop {
        let page = fixture.run_json(&[
            "logs",
            agent,
            "--after",
            &cursor.to_string(),
            "--session",
            spawn["data"]["session_id"].as_str().unwrap(),
            "--limit",
            "4096",
            "--json",
        ]);
        serialized_bytes += serde_json::to_vec(&page).unwrap().len();
        pages += 1;
        let page = &page["data"];
        full.push_str(page["transcript"].as_str().unwrap());
        cursor = page["next_cursor"].as_u64().unwrap();
        if page["eof"] == true {
            break;
        }
    }
    assert!(full.ends_with(&repeated));
    assert_eq!(full.len() as u64, data["total_bytes"].as_u64().unwrap());
    assert_eq!(full.matches("fixture.bootstrap").count(), 4);
    assert_eq!(full.matches("fixture.context").count(), 4);
    eprintln!(
        "transcript measurement: bootstrap_bytes={}, serialized_context_bytes={}, repetitions=4, raw_bytes={}, full_page_count={pages}, serialized_full_pages_bytes={serialized_bytes}, serialized_tail_bytes={}",
        startup.len(),
        context.len(),
        full.len(),
        serde_json::to_vec(&tail).unwrap().len()
    );
    verify_tail_follow(fixture, spawn, capture);
}

fn verify_tail_follow(
    fixture: &TestEnvironment,
    spawn: &Value,
    capture: &Path,
) {
    let output = fixture.root.join("tail-follow.jsonl");
    let mut command = fixture.command();
    command.args([
        "logs",
        spawn["data"]["agent"]["id"].as_str().unwrap(),
        "--tail",
        "--limit",
        "32",
        "--follow",
        "--json",
    ]);
    command.stdout(fs::File::create(&output).unwrap());
    let mut follower = command.spawn().unwrap();
    let read_pages = || -> Vec<Value> {
        fs::read_to_string(&output)
            .unwrap()
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    };
    wait_until("initial tail page from follower", || {
        !read_pages().is_empty()
    });
    let initial = read_pages()[0]["data"]["transcript"]
        .as_str()
        .unwrap()
        .to_owned();
    let next = "{\"type\":\"fixture.activity\",\"text\":\"Additional validation λ.\"}\n";
    fs::write(format!("{}.output-pending", capture.display()), next).unwrap();
    fs::rename(
        format!("{}.output-pending", capture.display()),
        format!("{}.output", capture.display()),
    )
    .unwrap();
    wait_until("followed activity after initial tail", || {
        let text: String = read_pages()
            .iter()
            .filter_map(|p| p["data"]["transcript"].as_str())
            .collect();
        text == format!("{initial}{next}")
    });
    fs::write(format!("{}.exit", capture.display()), "exit").unwrap();
    assert!(follower.wait().unwrap().success());
    let pages = read_pages();
    assert_eq!(pages.last().unwrap()["data"]["terminal"], true);
    assert!(
        pages
            .iter()
            .all(|p| p["data"]["session_id"] == spawn["data"]["session_id"])
    );
    for pair in pages.windows(2) {
        assert_eq!(
            pair[0]["data"]["next_cursor"],
            pair[1]["data"]["start_cursor"]
        );
    }
}

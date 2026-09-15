use super::*;

pub(super) const JOB: &str = r#"
if [ -z "${COTERIE_TASK_ID-}" ]; then exit 0; fi
capture="$COTERIE_SOCKET.$COTERIE_AGENT_ID"
{
  for argument in "$@"; do printf 'arg:%s\0' "$argument"; done
  for variable in COTERIE_PROJECT_ROOT COTERIE_PROJECT_ID COTERIE_PRIMARY_PROJECT_ROOT COTERIE_RUN_ID COTERIE_AGENT_ID COTERIE_SESSION_ID COTERIE_ROLE COTERIE_SOCKET COTERIE_TOKEN COTERIE_TASK_ID COTERIE_BIN; do
    eval "value=\${$variable}"
    printf 'env:%s=%s\0' "$variable" "$value"
  done
} > "$capture.pending"
mv "$capture.pending" "$capture"
printf '{"type":"thread.started","thread_id":"recovery-job"}\n'
while [ ! -e "$capture.exit" ]; do
  if [ ! -e "/proc/$PPID" ]; then exit 0; fi
  if [ -e "$capture.output" ]; then cat "$capture.output"; rm "$capture.output"; fi
  sleep 0.02
done
exit 23
"#;

pub(super) fn captured_job(
    fixture: &TestEnvironment,
    spawn: &Value,
) -> (PathBuf, Vec<(String, String)>, PathBuf) {
    let prime = fixture.run_json(&["prime", "--json"]);
    let capture = fixture.runtime.join("coterie").join(format!(
        "{}.sock.{}",
        prime["data"]["identity"]["run_id"].as_str().unwrap(),
        spawn["data"]["agent"]["id"].as_str().unwrap()
    ));
    wait_until("recovery job bootstrap", || capture.exists());
    let environment = captured_environment(&capture);
    let workspace =
        PathBuf::from(environment_value(&environment, "COTERIE_PROJECT_ROOT"));
    (capture, environment, workspace)
}

#[test]
fn read_only_worktree_has_no_commit_handoff() {
    let fixture = TestEnvironment::new();
    write_global(
        &fixture,
        &include_str!("../../examples/config/global.toml").replace(
            "permission_profile = \"implementation\"",
            "permission_profile = \"inspect\"",
        ),
    );
    fs::write(
        fixture.root.join("bin/codex"),
        format!(
            "{}{}",
            FAKE_CODEX.split_once("is_job=false").unwrap().0,
            JOB
        ),
    )
    .unwrap();
    fixture.launch(&[]);
    let task =
        fixture.run_json(&["task", "create", "Read-only assignment", "--json"]);
    let spawn = fixture.run_json(&[
        "spawn",
        "builder",
        "--task",
        task["data"]["task"]["id"].as_str().unwrap(),
        "--json",
    ]);
    let (_, environment, _) = captured_job(&fixture, &spawn);
    assert_eq!(
        fixture.run_agent_json(&["prime", "--json"], &environment)["data"]["commit_handoffs"],
        serde_json::json!([])
    );
    fixture.run_json(&["stop", "--json"]);
}

#[test]
fn recovery_continuation_integrates_and_releases_dependencies_only_after_closure()
 {
    let fixture = TestEnvironment::new();
    write_global(
        &fixture,
        &include_str!("../../examples/config/global.toml")
            .replace("coordinator", "planner"),
    );
    fs::write(
        fixture.root.join("bin/codex"),
        format!(
            "{}{}",
            FAKE_CODEX.split_once("is_job=false").unwrap().0,
            JOB
        ),
    )
    .unwrap();
    fixture.launch(&[]);
    let task = fixture.run_json(&[
        "task",
        "create",
        "Recover unfinished work",
        "--json",
    ]);
    let task_id = task["data"]["task"]["id"].as_str().unwrap();
    let dependent = fixture.run_json(&[
        "task",
        "create",
        "Wait for accepted work",
        "--after",
        task_id,
        "--json",
    ]);
    let spawn =
        fixture.run_json(&["spawn", "builder", "--task", task_id, "--json"]);
    let assignment = spawn["data"]["assignment_id"].as_str().unwrap();
    let (capture, old_environment, source) = captured_job(&fixture, &spawn);
    for json_output in [false, true] {
        let mut command = fixture.agent_command(&old_environment);
        command.arg("prime");
        if json_output {
            command.arg("--json");
        }
        let output = run(command);
        assert!(output.status.success());
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        let data = if json_output { &value["data"] } else { &value };
        let handoff = &data["commit_handoffs"][0];
        assert_eq!(handoff["assignment_id"], assignment);
        assert_eq!(handoff["task_id"], task_id);
        assert_eq!(handoff["agent_id"], spawn["data"]["agent"]["id"]);
        assert_eq!(handoff["workspace_path"], source.to_str().unwrap());
        assert_eq!(
            handoff["workspace_path_bytes"],
            serde_json::json!(source.as_os_str().as_bytes())
        );
        assert_eq!(
            handoff["permission_profile"]["filesystem"],
            "workspace-write"
        );
        assert_eq!(handoff["provider"], "codex");
        let repo = Repository::open(&source).unwrap();
        assert_eq!(
            handoff["owned_reference"],
            repo.head().unwrap().name().unwrap()
        );
        assert_eq!(
            handoff["base_commit"],
            repo.head().unwrap().target().unwrap().to_string()
        );
    }
    let source_head = commit_file(
        &source,
        Path::new("committed.txt"),
        "preserve the commit\n",
        "interrupted implementation",
    );
    fs::write(source.join("untracked.txt"), "preserve dirty work\n").unwrap();
    fs::write(source.join("staged.txt"), "preserve staged artifact\n").unwrap();
    let source_repo = Repository::open(&source).unwrap();
    let mut index = source_repo.index().unwrap();
    index.add_path(Path::new("staged.txt")).unwrap();
    index.write().unwrap();
    fs::write(source.join("committed.txt"), "preserve unstaged edits\n")
        .unwrap();
    let source_index = fs::read(source_repo.path().join("index")).unwrap();
    let evidence = fixture.run_agent_json(
        &["send", "planner", "Validation: artifact contents checked; full suite blocked. Remaining: port files, validate, commit, and submit.", "--json"],
        &old_environment,
    );
    let report = serde_json::json!({
        "validation_evidence": [{"text": "Artifact contents checked; full suite blocked.", "source": format!("message {}", evidence["data"]["message_id"].as_str().unwrap())}],
        "unfinished_steps": [{"text": "Port files, validate, commit, and submit.", "source": format!("message {}", evidence["data"]["message_id"].as_str().unwrap())}]
    }).to_string();
    let recovery_args = [
        "task",
        "recover",
        "--assignment",
        assignment,
        "--reason",
        "Provider exited before submission.",
        "--report",
        &report,
        "--operation-id",
        "co-01ARZ3NDEKTSV4RRFFQ69G5FB9",
        "--json",
    ];
    rejected(&fixture, &recovery_args, "inactivity");
    fs::write(format!("{}.exit", capture.display()), "exit").unwrap();
    wait_until("observed worker exit", || {
        let status = fixture.run_json(&["status", "--json"]);
        status["data"]["agents"]
            .as_array()
            .unwrap()
            .iter()
            .any(|agent| {
                agent["id"] == spawn["data"]["agent"]["id"]
                    && agent["state"] == "exited"
            })
    });
    let doctor = fixture.run_json(&["doctor", "--json"]);
    assert!(doctor.to_string().contains("task recover"));
    let exited = fixture.run_json(&["prime", "--json"]);
    let blocked = exited["data"]["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == task_id)
        .unwrap();
    assert_eq!(blocked["status"], "in_progress");
    assert_eq!(blocked["assignment"]["session_state"], "exited");
    assert_eq!(blocked["next_action"], "inspect_provider");
    let recovered = fixture.run_json(&recovery_args);
    let context = fixture.run_json(&["prime", "--json"]);
    let reopened = context["data"]["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == task_id)
        .unwrap();
    assert_eq!(reopened["next_action"], "spawn_continuation");
    let full_source =
        super::context::read_detail(&fixture, "assignment", assignment);
    let handoff = &full_source["recovery_handoffs"][0];
    assert_eq!(
        handoff["reported"],
        serde_json::from_str::<Value>(&report).unwrap()
    );
    assert_eq!(handoff["mechanical"]["head_commit"], source_head);
    assert_eq!(
        handoff["mechanical"]["staged_paths"][0]["path"],
        "staged.txt"
    );
    assert_eq!(
        handoff["mechanical"]["unstaged_paths"][0]["path"],
        "committed.txt"
    );
    assert_eq!(
        handoff["mechanical"]["untracked_paths"][0]["path"],
        "untracked.txt"
    );
    assert_eq!(
        handoff["mechanical"]["dirty_paths"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    assert_eq!(handoff["mechanical"]["complete"], true);
    assert_eq!(recovered["data"]["handoff"]["staged_paths"], 1);
    let mut human_command = fixture.command();
    human_command.args(["assignment", "show", assignment, "--limit", "65536"]);
    let human = run(human_command);
    assert!(human.status.success(), "{human:?}");
    assert!(human.stderr.is_empty());
    let human_page: Value = serde_json::from_slice(&human.stdout).unwrap();
    let human_document: Value =
        serde_json::from_str(human_page["text"].as_str().unwrap()).unwrap();
    assert_eq!(human_document, full_source);
    assert_eq!(
        full_source["recoveries"][0],
        recovered["data"]
            .as_object()
            .map(|data| {
                let mut source = data.clone();
                source.remove("operation_id");
                serde_json::Value::Object(source)
            })
            .unwrap()
    );
    assert_eq!(
        fixture.run_json(&["prime", "--json"])["data"]["commit_handoffs"],
        serde_json::json!([])
    );
    assert_eq!(recovered["data"]["assignment_id"], assignment);
    assert_eq!(
        recovered["data"]["workspace_path"],
        source.to_str().unwrap()
    );
    assert!(recovered["data"]["continuation_assignment_id"].is_null());
    assert_eq!(
        fixture.run_json(&["task", "ready", "--json"])["data"]["tasks"][0]["id"],
        task_id
    );
    let mut late_finish = fixture.agent_command(&old_environment);
    late_finish.args([
        "finish",
        "--status",
        "completed",
        "--summary",
        "Late submission.",
        "--json",
    ]);
    let late = run(late_finish);
    assert_eq!(late.status.code(), Some(6), "{late:?}");
    let next =
        fixture.run_json(&["spawn", "builder", "--task", task_id, "--json"]);
    let (next_capture, next_environment, continuation) =
        captured_job(&fixture, &next);
    assert_ne!(source, continuation);
    let launch = fs::read(&next_capture).unwrap();
    let args: Vec<_> = launch
        .split(|byte| *byte == 0)
        .filter_map(|part| part.strip_prefix(b"arg:"))
        .map(|part| String::from_utf8_lossy(part).into_owned())
        .collect();
    assert!(
        args.windows(2)
            .any(|pair| pair == ["--sandbox", "workspace-write"])
    );
    assert!(
        args.windows(2).any(|pair| pair[0] == "--cd"
            && pair[1] == continuation.to_str().unwrap())
    );
    assert!(!args.iter().any(
        |arg| arg == "--add-dir" || arg.contains(source.to_str().unwrap())
    ));
    let prime = fixture.run_agent_json(&["prime", "--json"], &next_environment);
    assert_eq!(
        prime["data"]["recoveries"][0]["continuation_assignment_id"],
        next["data"]["assignment_id"]
    );
    assert_eq!(prime["data"]["active_task"]["id"], task_id);
    assert_eq!(
        prime["data"]["current_task"]["assignment"]["id"],
        next["data"]["assignment_id"]
    );
    assert_eq!(
        prime["data"]["recoveries"][0]["workspace_path"]["text"],
        source.to_str().unwrap()
    );
    let full_continuation = super::context::read_detail(
        &fixture,
        "assignment",
        next["data"]["assignment_id"].as_str().unwrap(),
    );
    assert_eq!(
        full_continuation["recoveries"][0]["assignment_id"],
        assignment
    );
    assert_eq!(full_continuation["recovery_handoffs"][0], *handoff);
    assert_eq!(
        prime["data"]["recoveries"][0]["handoff"],
        recovered["data"]["handoff"]
    );
    let mut client =
        mcp::McpClient::start(fixture.agent_command(&next_environment));
    client.initialize();
    let mut page = client.call(
        "assignment_show",
        serde_json::json!({"assignment_id":assignment,"limit":256}),
    );
    let mut document = String::new();
    loop {
        assert_eq!(page["result"]["isError"], false);
        let data = &page["result"]["structuredContent"]["data"];
        document.push_str(data["text"].as_str().unwrap());
        if data["eof"] == true {
            break;
        }
        // Each new bridge authenticates the continuation, with no source credentials.
        drop(client);
        client =
            mcp::McpClient::start(fixture.agent_command(&next_environment));
        client.initialize();
        page = client.call("assignment_show", serde_json::json!({"assignment_id":assignment,"limit":256,"after":data["next_cursor"],"revision":data["revision"]}));
    }
    assert_eq!(
        serde_json::from_str::<Value>(&document).unwrap()["recovery_handoffs"]
            [0],
        *handoff
    );
    let denied = client.call("logs", serde_json::json!({"agent":spawn["data"]["agent"]["name"],"after":0,"limit":4096,"session_id":null,"tail":false}));
    assert_eq!(
        denied["result"]["structuredContent"]["error"]["code"],
        "permission_denied"
    );
    assert_eq!(
        prime["data"]["commit_handoffs"].as_array().unwrap().len(),
        1
    );
    assert_eq!(
        prime["data"]["commit_handoffs"][0]["assignment_id"],
        next["data"]["assignment_id"]
    );
    assert_eq!(
        prime["data"]["commit_handoffs"][0]["workspace_path"],
        continuation.to_str().unwrap()
    );
    for name in ["committed.txt", "staged.txt", "untracked.txt"] {
        commit_file(
            &continuation,
            Path::new(name),
            &fs::read_to_string(source.join(name)).unwrap(),
            "continue preserved work",
        );
    }
    fixture.run_agent_json(
        &[
            "finish",
            "--status",
            "completed",
            "--summary",
            "Ported and validated recovered work.",
            "--json",
        ],
        &next_environment,
    );
    assert_eq!(
        fixture.run_json(&["task", "ready", "--json"])["data"]["tasks"],
        serde_json::json!([])
    );
    rejected(
        &fixture,
        &[
            "task",
            "close",
            task_id,
            "--summary",
            "Premature closure",
            "--json",
        ],
        "not been integrated",
    );
    fixture.run_json(&[
        "workspace",
        "integrate",
        "--assignment",
        next["data"]["assignment_id"].as_str().unwrap(),
        "--json",
    ]);
    assert_eq!(
        fixture.run_json(&["task", "ready", "--json"])["data"]["tasks"],
        serde_json::json!([])
    );
    assert_eq!(
        fs::read_to_string(fixture.project.join("committed.txt")).unwrap(),
        "preserve unstaged edits\n"
    );
    assert_eq!(
        fs::read_to_string(fixture.project.join("untracked.txt")).unwrap(),
        "preserve dirty work\n"
    );
    assert_eq!(
        fs::read_to_string(fixture.project.join("staged.txt")).unwrap(),
        "preserve staged artifact\n"
    );
    fixture.run_json(&[
        "task",
        "close",
        task_id,
        "--summary",
        "Validated all three recovered files after integration.",
        "--json",
    ]);
    assert_eq!(
        fixture.run_json(&["task", "ready", "--json"])["data"]["tasks"][0]["id"],
        dependent["data"]["task"]["id"]
    );
    assert_eq!(fixture.run_json(&recovery_args), recovered);
    let after_closure = super::context::read_detail(
        &fixture,
        "assignment",
        next["data"]["assignment_id"].as_str().unwrap(),
    );
    assert_eq!(after_closure["recovery_handoffs"][0], *handoff);
    let mut changed_args = recovery_args.to_vec();
    changed_args[7] = "{\"validation_evidence\":[],\"unfinished_steps\":[]}";
    rejected(&fixture, &changed_args, "different request");
    assert_eq!(
        fs::read(source_repo.path().join("index")).unwrap(),
        source_index
    );
    assert_eq!(
        fs::read_to_string(source.join("staged.txt")).unwrap(),
        "preserve staged artifact\n"
    );
    assert_eq!(
        fs::read_to_string(source.join("committed.txt")).unwrap(),
        "preserve unstaged edits\n"
    );
    assert_eq!(
        Repository::open(&source)
            .unwrap()
            .head()
            .unwrap()
            .target()
            .unwrap()
            .to_string(),
        source_head
    );
    assert_eq!(
        fs::read_to_string(source.join("untracked.txt")).unwrap(),
        "preserve dirty work\n"
    );
    fixture.run_json(&["stop", "--json"]);
}

#[test]
fn concurrent_finish_and_recovery_never_accept_both_mutations() {
    for exited in [false, true] {
        let fixture = TestEnvironment::new();
        fs::write(
            fixture.root.join("bin/codex"),
            format!(
                "{}{}",
                FAKE_CODEX.split_once("is_job=false").unwrap().0,
                JOB
            ),
        )
        .unwrap();
        fixture.launch(&[]);
        let task = fixture.run_json(&[
            "task",
            "create",
            "Race submission with recovery",
            "--json",
        ]);
        let task_id = task["data"]["task"]["id"].as_str().unwrap();
        let spawn =
            fixture.run_json(&["spawn", "worker", "--task", task_id, "--json"]);
        let (capture, environment, _) = captured_job(&fixture, &spawn);
        if exited {
            fs::write(format!("{}.exit", capture.display()), "exit").unwrap();
            wait_until("exited assignment before racing requests", || {
                fixture.run_json(&["status", "--json"])["data"]["agents"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|agent| {
                        agent["id"] == spawn["data"]["agent"]["id"]
                            && agent["state"] == "exited"
                    })
            });
        }
        let finish = fixture
            .agent_command(&environment)
            .args([
                "finish",
                "--status",
                "completed",
                "--summary",
                "Validated clean assignment.",
                "--json",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let recover = fixture
            .command()
            .args([
                "task",
                "recover",
                "--assignment",
                spawn["data"]["assignment_id"].as_str().unwrap(),
                "--reason",
                "Continue after exit.",
                "--json",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let finished = finish.wait_with_output().unwrap();
        let recovered = recover.wait_with_output().unwrap();
        assert_eq!(finished.status.success(), !exited, "{finished:?}");
        assert_eq!(recovered.status.success(), exited, "{recovered:?}");
        let prime = fixture.run_json(&["prime", "--json"]);
        assert_eq!(
            prime["data"]["tasks"][0]["status"],
            if exited { "open" } else { "submitted" }
        );
        fixture.run_json(&["stop", "--json"]);
    }
}

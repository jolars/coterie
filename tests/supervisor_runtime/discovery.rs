use super::*;

#[test]
fn retained_tool_discovery_respects_selected_role_and_replays_acceptance() {
    for authorized in [false, true] {
        let fixture = TestEnvironment::new();
        let config = include_str!("../../examples/config/global.toml")
            .replace("coordinator", "planner")
            .replace("builder", "implementer")
            .replace(
                "\"spawn:implementer\", \"send:*\", \"task:*\", \"logs:*\"",
                if authorized {
                    "\"task:read\", \"task:submit-retained\", \"workspace:integrate\", \"task:close\""
                } else {
                    "\"task:read\""
                },
            );
        write_global(&fixture, &config);
        fs::write(
            fixture.root.join("bin/codex"),
            format!(
                "{}{}",
                FAKE_CODEX.split_once("is_job=false").unwrap().0,
                recovery::JOB
            ),
        )
        .unwrap();
        let (mut lead, environment) = live_lead(&fixture);
        let _run = StopRun(&fixture);
        let capture =
            fs::read_to_string(fixture.root.join("mcp-agent-environment"))
                .unwrap();
        assert!(capture.contains("prime.commands"));
        assert!(capture.contains("tool_search"));
        assert_eq!(capture.contains("use task_submit_retained"), authorized);
        if !authorized {
            assert!(capture.contains("Your role lacks task:submit-retained"));
        }
        let mut bridge = McpClient::start(fixture.agent_command(&environment));
        bridge.initialize();
        let catalog = bridge.request("tools/list", json!({}));
        let expected: Value = serde_json::from_str(include_str!(
            "../../schemas/mcp-tools-v1.json"
        ))
        .unwrap();
        assert_eq!(catalog["result"]["tools"], expected["tools"]);
        let tool = catalog["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["name"] == "task_submit_retained")
            .unwrap();
        for prerequisite in ["HEAD", "revoked", "independent", "task_recover"] {
            assert!(
                tool["description"].as_str().unwrap().contains(prerequisite)
            );
        }
        let prime = bridge.call("prime", json!({}));
        assert_eq!(prime["result"]["isError"], false);
        let commands = prime["result"]["structuredContent"]["data"]["commands"]
            .as_array()
            .unwrap();
        assert_eq!(
            commands.contains(&json!("task_submit_retained")),
            authorized
        );
        for command in commands {
            assert!(
                catalog["result"]["tools"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|tool| &tool["name"] == command),
                "authorized command has no callable tool: {command}"
            );
        }

        let task = fixture.run_json(&[
            "task",
            "create",
            "Interrupted implementation",
            "--json",
        ]);
        let task_id = task["data"]["task"]["id"].as_str().unwrap();
        let dependency = fixture.run_json(&[
            "task",
            "create",
            "Wait for acceptance",
            "--after",
            task_id,
            "--json",
        ]);
        let spawn = fixture.run_json(&[
            "spawn",
            "implementer",
            "--task",
            task_id,
            "--json",
        ]);
        let assignment = spawn["data"]["assignment_id"].as_str().unwrap();
        let (capture, worker, workspace) =
            recovery::captured_job(&fixture, &spawn);
        let worker_prime =
            fixture.run_agent_json(&["prime", "--json"], &worker);
        assert!(
            !worker_prime["data"]["commands"]
                .as_array()
                .unwrap()
                .contains(&json!("task submit-retained"))
        );
        let commit = commit_file(
            &workspace,
            Path::new("result.txt"),
            "reviewed\n",
            "Reviewed commit",
        );
        let index = fs::read(
            Repository::open(&workspace).unwrap().path().join("index"),
        )
        .unwrap();
        fs::write(format!("{}.exit", capture.display()), "exit").unwrap();
        wait_until("interrupted worker exit", || {
            fixture.run_json(&["status", "--json"])["data"]["agents"]
                .as_array()
                .unwrap()
                .iter()
                .any(|a| {
                    a["id"] == spawn["data"]["agent"]["id"]
                        && a["state"] == "exited"
                })
        });
        let evidence = fixture.run_json(&["events", "--json"]);
        let event = evidence["data"]["events"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| {
                e["event_type"] == "session.started"
                    && e["subject"] == spawn["data"]["session_id"]
            })
            .unwrap();
        let contract = &event["payload"]["data"]["launch_contract"];
        assert_eq!(
            contract["supervisor"]["version"],
            env!("CARGO_PKG_VERSION")
        );
        assert_eq!(
            contract["supervisor"]["sha256"],
            digest(&fs::read(env!("CARGO_BIN_EXE_coterie")).unwrap())
        );
        assert_eq!(contract["provider"]["version"], "0.153.4");
        assert!(
            contract["provider"]["capabilities"]
                .as_array()
                .unwrap()
                .contains(&json!("authenticated agent supervisor transport"))
        );
        assert_eq!(
            contract["bridge"]["catalog_sha256"],
            digest(&serde_json::to_vec(&expected["tools"]).unwrap())
        );
        assert!(
            contract["bridge"]["tools"]
                .as_array()
                .unwrap()
                .contains(&json!("task_submit_retained"))
        );
        assert_eq!(contract["role"], "implementer");
        assert_eq!(
            contract["permission_profile"]["filesystem"],
            "workspace-write"
        );
        assert!(
            !contract
                .to_string()
                .contains(&environment_value(&worker, "COTERIE_TOKEN"))
        );
        let args = json!({"operation_id":format!("co-{}", ulid::Ulid::generate()), "submission":{
            "assignment_id":assignment, "result_commit":commit,
            "summary":"Verified result.txt and clean worktree; target validation pending.",
            "reason":"Worker exited before finish.",
            "review":{"text":"Independently reviewed this exact commit.", "source":"test review artifact"}
        }});
        let result = bridge.call("task_submit_retained", args.clone());
        if authorized {
            assert_eq!(result["result"]["isError"], false, "{result}");
            assert_eq!(
                result["result"]["structuredContent"]["data"]["task"]["status"],
                "submitted"
            );
            assert_eq!(bridge.call("task_close", json!({"operation_id":format!("co-{}", ulid::Ulid::generate()), "task_id":task_id,"summary":"Premature"}))["result"]["structuredContent"]["error"]["code"], "conflict");
            let integration = bridge.call("workspace_integrate", json!({"operation_id":format!("co-{}", ulid::Ulid::generate()), "assignment_id":assignment}));
            assert_eq!(
                integration["result"]["isError"], false,
                "{integration}"
            );
            assert_eq!(
                fs::read_to_string(fixture.project.join("result.txt")).unwrap(),
                "reviewed\n"
            );
            assert!(
                fixture.run_json(&["task", "ready", "--json"])["data"]["tasks"]
                    .as_array()
                    .unwrap()
                    .is_empty()
            );
            let close = bridge.call("task_close", json!({"operation_id":format!("co-{}", ulid::Ulid::generate()), "task_id":task_id,"summary":"Validated integrated result.txt and clean target."}));
            assert_eq!(close["result"]["isError"], false, "{close}");
            assert_eq!(
                fixture.run_json(&["task", "ready", "--json"])["data"]["tasks"]
                    [0]["id"],
                dependency["data"]["task"]["id"]
            );
            assert_eq!(
                bridge.call("task_submit_retained", args.clone())["result"],
                result["result"]
            );
        } else {
            assert_eq!(
                result["result"]["structuredContent"]["error"]["code"],
                "permission_denied"
            );
            let current = fixture.run_json(&["prime", "--json"]);
            assert_eq!(
                current["data"]["tasks"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|t| t["id"] == task_id)
                    .unwrap()["status"],
                "in_progress"
            );
        }
        assert_eq!(
            fs::read(
                Repository::open(&workspace).unwrap().path().join("index")
            )
            .unwrap(),
            index
        );
        lead.stdin.take().unwrap().write_all(b"done\n").unwrap();
        lead.wait().unwrap();
        assert_eq!(
            bridge.call("task_submit_retained", args)["result"]["structuredContent"]
                ["error"]["code"],
            "unauthenticated"
        );
        fixture.run_json(&["stop", "--json"]);
    }
}

fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[test]
fn interrupted_worker_diagnostics_keep_reports_exits_and_timeouts_distinct() {
    for case in ["capacity", "unknown", "timeout", "partial"] {
        eprintln!("diagnostic case: {case}");
        let fixture = TestEnvironment::new();
        let config = include_str!("../../examples/config/global.toml").replace(
            "job_timeout_seconds = 1800",
            if case == "timeout" {
                "job_timeout_seconds = 8"
            } else {
                "job_timeout_seconds = 1800"
            },
        );
        let config = config.replace(
            "[supervision]",
            "[supervision]\ninterrupt_grace_ms = 20\nshutdown_timeout_ms = 200",
        );
        write_global(&fixture, &config);
        fs::write(
            fixture.root.join("bin/codex"),
            format!(
                "{}{}",
                FAKE_CODEX.split_once("is_job=false").unwrap().0,
                recovery::JOB
            ),
        )
        .unwrap();
        let (mut lead, environment) = live_lead(&fixture);
        let _run = StopRun(&fixture);
        let mut bridge = McpClient::start(fixture.agent_command(&environment));
        bridge.initialize();
        let task = fixture.run_json(&[
            "task",
            "create",
            "Interrupted worker diagnostics",
            "--json",
        ]);
        let task_id = task["data"]["task"]["id"].as_str().unwrap();
        let spawn = fixture
            .run_json(&["spawn", "builder", "--task", task_id, "--json"]);
        let (capture, _, _) = recovery::captured_job(&fixture, &spawn);
        let initial = bridge.call("poll", json!({}));
        assert_eq!(initial["result"]["isError"], false, "{initial}");
        assert!(
            initial["result"]["structuredContent"]["data"]["deadlines"]
                .as_array()
                .unwrap()
                .iter()
                .any(|d| d["session_id"] == spawn["data"]["session_id"])
        );
        let output = match case {
            "capacity" => include_str!("../fixtures/codex-capacity.jsonl"),
            "partial" => "{\"type\":\"error\",\"message\":\"Selected model",
            _ => "",
        };
        if !output.is_empty() {
            fs::write(format!("{}.output", capture.display()), output).unwrap();
            wait_until("provider diagnostic consumed", || {
                !PathBuf::from(format!("{}.output", capture.display())).exists()
            });
        }
        if case != "timeout" {
            fs::write(format!("{}.exit", capture.display()), "exit").unwrap();
        }
        wait_until("diagnostic worker exit", || {
            fixture.run_json(&["status", "--json"])["data"]["agents"]
                .as_array()
                .unwrap()
                .iter()
                .any(|a| {
                    a["id"] == spawn["data"]["agent"]["id"]
                        && a["state"]
                            == if case == "partial" {
                                "quarantined"
                            } else {
                                "exited"
                            }
                })
        });
        let changes = bridge.call(
            "poll",
            json!({"cursor":initial["result"]["structuredContent"]["data"]["cursor"]}),
        );
        assert_eq!(changes["result"]["isError"], false, "{changes}");
        let progress = &changes["result"]["structuredContent"]["data"];
        let exit = progress["changes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| {
                c["kind"] == "session"
                    && c["session_id"] == spawn["data"]["session_id"]
                    && c["state"]
                        == if case == "partial" {
                            "quarantined"
                        } else {
                            "exited"
                        }
            })
            .unwrap();
        if case == "partial" {
            assert!(exit["exit"].is_null());
            let recovery = bridge.call("task_recover", json!({"operation_id":format!("co-{}", ulid::Ulid::generate()), "assignment_id":spawn["data"]["assignment_id"], "reason":"Cannot infer an exit from an incomplete diagnostic."}));
            assert_eq!(
                recovery["result"]["structuredContent"]["error"]["code"],
                "conflict"
            );
        } else {
            assert_eq!(exit["exit"]["reason"], "process");
            if case != "timeout" {
                assert_eq!(exit["exit"]["code"], 23);
            }
        }
        assert_eq!(
            progress["changes"]
                .as_array()
                .unwrap()
                .iter()
                .any(|c| c["kind"] == "session_control"
                    && c["reason"] == "execution_timeout"),
            case == "timeout",
            "{case}: {progress}"
        );
        assert!(!progress.to_string().contains("Selected model"));
        let logs = fixture.run_agent_json(
            &[
                "logs",
                spawn["data"]["agent"]["id"].as_str().unwrap(),
                "--json",
            ],
            &environment,
        );
        let transcript = logs["data"]["transcript"].as_str().unwrap();
        assert_eq!(
            transcript.contains("Selected model is at capacity"),
            case == "capacity"
        );
        assert_eq!(logs["data"]["incomplete_tail"], case == "partial");
        let prime = bridge.call("prime", json!({}));
        assert_eq!(
            prime["result"]["structuredContent"]["data"]["tasks"][0]["status"],
            "in_progress"
        );
        assert_eq!(
            prime["result"]["structuredContent"]["data"]["tasks"][0]["next_action"],
            "inspect_provider"
        );
        assert!(
            fixture.run_json(&["task", "ready", "--json"])["data"]["tasks"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        lead.stdin.take().unwrap().write_all(b"done\n").unwrap();
        lead.wait().unwrap();
        fixture.run_json(&["stop", "--json"]);
    }
}

//! Exercise the validation workflow through existing authenticated messages.
//!
//! Environment denials here are reported fixture observations, not sandbox
//! probes. The opt-in NixOS regression establishes actual provider access.

use super::*;
use serde_json::json;
use sha2::{Digest, Sha256};

#[test]
#[ignore = "validation command invoked only by the handoff fixture"]
fn validation_fixture_child() {
    let expected = std::env::var("COTERIE_VALIDATION_EXPECTED").unwrap();
    assert_eq!(fs::read_to_string("README.md").unwrap(), expected);
}

fn source(workspace: &Path) -> Value {
    let repository = Repository::open(workspace).unwrap();
    json!({
        "head_commit": repository.head().unwrap().target().unwrap().to_string(),
        "inputs": {"README.md": Sha256::digest(fs::read(workspace.join("README.md")).unwrap()).iter().map(|byte| format!("{byte:02x}")).collect::<String>()}
    })
}

fn check(workspace: &Path, expected: &str) -> Value {
    let executable = std::env::current_exe().unwrap();
    let arguments = [
        "--ignored",
        "--exact",
        "validation_handoffs::validation_fixture_child",
        "--nocapture",
    ];
    let output = Command::new(&executable)
        .args(arguments)
        .current_dir(workspace)
        .env("COTERIE_VALIDATION_EXPECTED", expected)
        .output()
        .unwrap();
    json!({
        "command": std::iter::once(executable.to_str().unwrap()).chain(arguments).collect::<Vec<_>>(),
        "working_directory": workspace,
        "source": source(workspace),
        "environment_inputs": {"COTERIE_VALIDATION_EXPECTED": expected},
        "exit_status": output.status.code(),
        "outcome": if output.status.success() { "passed" } else { "failed" },
        "diagnostic": String::from_utf8_lossy(&output.stdout)
    })
}

fn send(
    fixture: &TestEnvironment,
    environment: &[(String, String)],
    recipient: &str,
    record: &Value,
) -> Value {
    fixture.run_agent_json(
        &["send", recipient, &record.to_string(), "--json"],
        environment,
    )
}

fn records(
    fixture: &TestEnvironment,
    environment: &[(String, String)],
) -> Vec<Value> {
    fixture.run_agent_json(&["inbox", "--after", "0", "--json"], environment)
        ["data"]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|message| serde_json::from_str(message["body"].as_str().unwrap()).unwrap())
        .collect()
}

#[test]
fn validation_preflight_routes_pending_checks_without_committing_or_accepting()
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
            super::recovery::JOB
        ),
    )
    .unwrap();
    let capture = fixture.root.join("planner-environment");
    let mut foreground = fixture
        .command()
        .env("COTERIE_FAKE_MODE", "contract")
        .env("COTERIE_FAKE_CAPTURE", &capture)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_until("validation planner", || capture.exists());
    let planner = captured_environment(&capture);
    let task = fixture.run_json(&[
        "task",
        "create",
        "Validate before expensive edits",
        "--json",
    ]);
    let task_id = task["data"]["task"]["id"].as_str().unwrap();
    let dependent = fixture.run_json(&[
        "task",
        "create",
        "Needs accepted validation",
        "--after",
        task_id,
        "--json",
    ]);
    let spawn =
        fixture.run_json(&["spawn", "builder", "--task", task_id, "--json"]);
    let (_, worker, workspace) =
        super::recovery::captured_job(&fixture, &spawn);
    let prime = fixture.run_agent_json(&["prime", "--json"], &worker);
    let assignment = &prime["data"]["commit_handoffs"][0];
    let original_source = source(&workspace);
    let request = json!({
        "kind": "validation_request", "task_id": task_id,
        "assignment_id": spawn["data"]["assignment_id"],
        "session_id": spawn["data"]["session_id"], "generation": assignment["generation"],
        "source": original_source,
        "actual_policy": assignment["permission_profile"],
        "required_commands": [["devenv", "--offline", "--no-tui", "shell", "--", "cargo", "test"]],
        "declared_inputs": {"toolchain": "fixture", "kernel": "fixture", "network": "loopback service", "state_path": workspace.join(".devenv")},
        "validation_owner": null, "proposed_owner": "planner", "pending": ["cargo test"],
        "request_time_ms": 1000,
        "preflight": {"outcome": "blocked", "exit_status": 1, "diagnostic": "fixture: Nix daemon socket: Operation not permitted", "checks_started": false},
        "git_commit_authority": "separate coordinator handoff"
    });
    let body = request.to_string();
    let args = [
        "send",
        "planner",
        &body,
        "--operation-id",
        "co-01ARZ3NDEKTSV4RRFFQ69G5FB1",
        "--json",
    ];
    let sent = fixture.run_agent_json(&args, &worker);
    assert_eq!(fixture.run_agent_json(&args, &worker), sent);
    assert_eq!(records(&fixture, &planner), vec![request.clone()]);
    assert_eq!(source(&workspace), original_source);

    let acknowledgment = json!({
        "kind": "validation_acknowledgment", "request_message_id": sent["data"]["message_id"],
        "validation_owner": "planner", "pending": ["cargo test"],
        "request_time_ms": 1000, "acknowledgment_time_ms": 1250,
        "handoff_wait_ms": 250, "git_commit_pending": true,
        "supported_route": "prepared validation command under coordinator's existing policy"
    });
    send(
        &fixture,
        &planner,
        spawn["data"]["agent"]["name"].as_str().unwrap(),
        &acknowledgment,
    );
    assert_eq!(records(&fixture, &worker), vec![acknowledgment]);

    fs::write(workspace.join("README.md"), "edited source\n").unwrap();
    let failed = check(&workspace, "fixture\n");
    assert_eq!(failed["outcome"], "failed");
    assert_ne!(failed["source"], original_source);
    let result = json!({
        "kind": "validation_result", "validation_owner": "planner",
        "request_message_id": sent["data"]["message_id"],
        "acknowledgment_time_ms": 1250, "result_time_ms": 1750,
        "owner_wait_ms": 400, "execution_time_ms": 100,
        "checks": [failed], "pending": ["cargo test"],
        "summary": "Prepared content check failed; documented Cargo check remains blocked."
    });
    send(
        &fixture,
        &planner,
        spawn["data"]["agent"]["name"].as_str().unwrap(),
        &result,
    );
    let worker_records = records(&fixture, &worker);
    assert_eq!(worker_records[1], result);
    assert_eq!(worker_records[0]["git_commit_pending"], true);
    assert_eq!(source(&fixture.project), original_source);
    let tasks = fixture.run_json(&["status", "--json"]);
    assert_eq!(tasks["data"]["tasks"]["closed"], 0);
    assert!(
        !fixture.run_json(&["task", "ready", "--json"])["data"]["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|task| task["id"] == dependent["data"]["task"]["id"])
    );
    let mut finish = fixture.agent_command(&worker);
    finish.args([
        "finish",
        "--status",
        "completed",
        "--summary",
        "Validation handoff recorded.",
        "--json",
    ]);
    assert!(!run(finish).status.success());
    assert_eq!(
        fs::read_to_string(workspace.join("README.md")).unwrap(),
        "edited source\n"
    );
    drop(foreground.stdin.take());
    assert!(foreground.wait().unwrap().success());
    fixture.run_json(&["stop", "--json"]);
}

#[test]
fn basin_invocations_survive_session_replacement_and_new_base_recovery() {
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
            super::recovery::JOB
        ),
    )
    .unwrap();
    let capture = fixture.root.join("planner-environment");
    let mut foreground = fixture
        .command()
        .env("COTERIE_FAKE_MODE", "contract")
        .env("COTERIE_FAKE_CAPTURE", &capture)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_until("Basin planner", || capture.exists());
    let planner = captured_environment(&capture);
    let task = fixture.run_json(&[
        "task",
        "create",
        "Basin invocation evidence",
        "--json",
    ]);
    let task_id = task["data"]["task"]["id"].as_str().unwrap();
    let spawn =
        fixture.run_json(&["spawn", "builder", "--task", task_id, "--json"]);
    let (worker_capture, worker, workspace) =
        super::recovery::captured_job(&fixture, &spawn);
    let prime = fixture.run_agent_json(&["prime", "--json"], &worker);
    let handoff = &prime["data"]["commit_handoffs"][0];
    let command = json!([
        "devenv",
        "--offline",
        "--no-tui",
        "shell",
        "--",
        "python3",
        "validate.py"
    ]);
    let inputs = json!({"devenv_lock_sha256": "fixture-lock", "devenv_version": "fixture-version", "kernel": "fixture-kernel", "required_environment": ["PATH"], "state_path": workspace.join(".devenv")});
    let initial_source = source(&workspace);
    let plan = json!({
        "kind": "validation_preflight", "task_id": task_id,
        "assignment_id": spawn["data"]["assignment_id"],
        "validation_owner": "planner", "pending": ["documented devenv validation"],
        "required_commands": [command.clone()], "declared_inputs": inputs,
        "actual_policy": handoff["permission_profile"], "source": initial_source,
        "request_time_ms": 1000
    });
    let request = send(&fixture, &worker, "planner", &plan);
    let request_id = &request["data"]["message_id"];
    let initial = json!({
        "kind": "validation_invocation", "request_message_id": request_id,
        "invocation_id": "initial", "assignment_id": spawn["data"]["assignment_id"],
        "session_id": spawn["data"]["session_id"], "generation": handoff["generation"],
        "source": initial_source, "command": command, "working_directory": workspace,
        "declared_inputs": inputs, "actual_policy": handoff["permission_profile"],
        "started_at_ms": 1100, "ended_at_ms": 1200,
        "outcome": "passed", "exit_status": 0, "diagnostic": "fixture: initial documented command passed",
        "prepared_check": check(&workspace, "fixture\n")
    });
    assert_eq!(initial["prepared_check"]["outcome"], "passed");
    send(&fixture, &worker, "planner", &initial);

    fs::write(workspace.join("README.md"), "Basin edits\n").unwrap();
    fs::create_dir(workspace.join(".devenv")).unwrap();
    fs::write(
        workspace.join(".devenv/access-probe"),
        "local writes succeed\n",
    )
    .unwrap();
    let later = json!({
        "kind": "validation_invocation", "request_message_id": request_id,
        "invocation_id": "later-same-worktree", "assignment_id": spawn["data"]["assignment_id"],
        "session_id": spawn["data"]["session_id"], "generation": handoff["generation"],
        "source": source(&workspace), "command": command, "working_directory": workspace,
        "declared_inputs": inputs, "actual_policy": handoff["permission_profile"],
        "started_at_ms": 1300, "ended_at_ms": 1350,
        "outcome": "blocked", "exit_status": 1,
        "diagnostic": "fixture: /nix/var/nix/daemon-socket/socket: Operation not permitted",
        "local_devenv_write": "passed", "checks_started": false,
        "validation_owner": "planner", "pending": ["documented devenv validation"],
        "historical_evidence": "initial invocation validated earlier source only"
    });
    let later_message = send(&fixture, &worker, "planner", &later);
    assert_ne!(initial["source"], later["source"]);
    assert_eq!(initial["actual_policy"], later["actual_policy"]);
    assert_eq!(
        records(&fixture, &planner),
        vec![plan.clone(), initial.clone(), later.clone()]
    );

    drop(foreground.stdin.take());
    assert!(foreground.wait().unwrap().success());
    fs::remove_file(&capture).unwrap();
    let mut replacement = fixture
        .command()
        .env("COTERIE_FAKE_MODE", "contract")
        .env("COTERIE_FAKE_CAPTURE", &capture)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_until("replacement validation planner", || capture.exists());
    let new_planner = captured_environment(&capture);
    assert_ne!(
        environment_value(&planner, "COTERIE_SESSION_ID"),
        environment_value(&new_planner, "COTERIE_SESSION_ID")
    );
    assert_eq!(
        records(&fixture, &new_planner),
        vec![plan, initial.clone(), later.clone()]
    );

    fs::write(format!("{}.exit", worker_capture.display()), "exit").unwrap();
    wait_until("validation worker observed exit", || {
        fixture.run_json(&["status", "--json"])["data"]["agents"]
            .as_array()
            .unwrap()
            .iter()
            .any(|agent| {
                agent["id"] == spawn["data"]["agent"]["id"]
                    && agent["state"] == "exited"
            })
    });
    let report = json!({
        "validation_evidence": [{"text": later.to_string(), "source": format!("message {}", later_message["data"]["message_id"].as_str().unwrap())}],
        "unfinished_steps": [{"text": "Port changes into continuation; record fresh preflight and validation ownership. Earlier pass is historical.", "source": format!("message {}", request_id.as_str().unwrap())}]
    });
    fixture.run_json(&[
        "task",
        "recover",
        "--assignment",
        spawn["data"]["assignment_id"].as_str().unwrap(),
        "--reason",
        "Continue after observed exit.",
        "--report",
        &report.to_string(),
        "--json",
    ]);
    let preserved = super::context::read_detail(
        &fixture,
        "assignment",
        spawn["data"]["assignment_id"].as_str().unwrap(),
    );
    assert_eq!(preserved["recovery_handoffs"][0]["reported"], report);
    let new_base = commit_file(
        &fixture.project,
        Path::new("base.txt"),
        "new target base\n",
        "advance target base",
    );
    let continuation =
        fixture.run_json(&["spawn", "builder", "--task", task_id, "--json"]);
    let (_, next_worker, next_workspace) =
        super::recovery::captured_job(&fixture, &continuation);
    let next_prime = fixture.run_agent_json(&["prime", "--json"], &next_worker);
    let next_handoff = &next_prime["data"]["commit_handoffs"][0];
    assert_eq!(next_handoff["base_commit"], new_base);
    assert_ne!(next_handoff["assignment_id"], handoff["assignment_id"]);
    assert_ne!(source(&next_workspace), initial_source);
    assert_eq!(
        next_handoff["permission_profile"],
        handoff["permission_profile"]
    );
    fs::write(
        next_workspace.join("README.md"),
        fs::read(workspace.join("README.md")).unwrap(),
    )
    .unwrap();
    let fresh = json!({
        "kind": "validation_invocation", "request_message_id": request_id,
        "invocation_id": "fresh-continuation", "assignment_id": continuation["data"]["assignment_id"],
        "session_id": continuation["data"]["session_id"], "generation": next_handoff["generation"],
        "source": source(&next_workspace), "command": command, "working_directory": next_workspace,
        "declared_inputs": {"devenv_lock_sha256": "fixture-lock", "devenv_version": "fixture-version", "kernel": "fixture-kernel", "required_environment": ["PATH"], "state_path": next_workspace.join(".devenv")},
        "actual_policy": next_handoff["permission_profile"], "started_at_ms": 2000, "ended_at_ms": 2010,
        "outcome": "blocked", "exit_status": 1,
        "diagnostic": "fixture: ~/.cache/nix/fetcher-locks: Read-only file system",
        "checks_started": false, "validation_owner": "planner", "pending": ["documented devenv validation"]
    });
    send(&fixture, &next_worker, "planner", &fresh);
    let acknowledgment = json!({
        "kind": "validation_acknowledgment", "request_message_id": request_id,
        "assignment_id": continuation["data"]["assignment_id"],
        "validation_owner": "planner", "pending": ["documented devenv validation"],
        "request_time_ms": 2010, "acknowledgment_time_ms": 2260, "handoff_wait_ms": 250,
        "git_commit_pending": true,
        "summary": "I own the blocked check; initial pass does not validate the continuation."
    });
    send(
        &fixture,
        &new_planner,
        continuation["data"]["agent"]["name"].as_str().unwrap(),
        &acknowledgment,
    );
    assert_eq!(records(&fixture, &next_worker), vec![acknowledgment]);
    let after_recovery = records(&fixture, &new_planner);
    assert_eq!(after_recovery.len(), 4);
    assert_eq!(after_recovery[1], initial);
    assert_eq!(after_recovery[2], later);
    assert_eq!(after_recovery[3], fresh);
    assert_eq!(
        fs::read_to_string(workspace.join("README.md")).unwrap(),
        "Basin edits\n"
    );
    assert_eq!(
        fixture.run_json(&["status", "--json"])["data"]["tasks"]["closed"],
        0
    );
    drop(replacement.stdin.take());
    assert!(replacement.wait().unwrap().success());
    fixture.run_json(&["stop", "--json"]);
}

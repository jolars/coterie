use super::finish::FinishFixture;
use super::*;

const OPERATION: &str = "co-01ARZ3NDEKTSV4RRFFQ69G5FC8";

struct ExternalClosure {
    fixture: FinishFixture,
    task: String,
    assignment: String,
    result: String,
    target: String,
}

impl ExternalClosure {
    fn new() -> Self {
        let fixture =
            FinishFixture::new("worker", "Externally integrated work");
        let result = commit_file(
            &fixture.workspace,
            Path::new("result.txt"),
            "validated result\n",
            "implement result",
        );
        let submitted = fixture.finish("completed");
        let task = submitted["data"]["task"]["id"].as_str().unwrap().to_owned();
        let assignment = submitted["data"]["assignment_id"]
            .as_str()
            .unwrap()
            .to_owned();
        commit_file(
            &fixture.environment.project,
            Path::new("independent.txt"),
            "independent\n",
            "independent change",
        );
        let repository =
            Repository::open(&fixture.environment.project).unwrap();
        let source = repository.find_commit(result.parse().unwrap()).unwrap();
        repository.cherrypick(&source, None).unwrap();
        let target = commit_file(
            &fixture.environment.project,
            Path::new("result.txt"),
            "validated result\n",
            "apply reviewed result externally",
        );
        repository.cleanup_state().unwrap();
        assert_ne!(result, target);
        assert_repository_clean(&fixture.workspace);
        assert_repository_clean(&fixture.environment.project);
        Self {
            fixture,
            task,
            assignment,
            result,
            target,
        }
    }

    fn arguments(&self) -> Vec<&str> {
        vec![
            "task",
            "close",
            &self.task,
            "--override",
            "--assignment",
            &self.assignment,
            "--result-commit",
            &self.result,
            "--target-commit",
            &self.target,
            "--reason",
            "Reviewed cherry-pick outside Coterie.",
            "--summary",
            "Validated result.txt and the full test suite.",
            "--operation-id",
            OPERATION,
            "--json",
        ]
    }

    fn command(&self) -> Command {
        let mut command = self.fixture.environment.command();
        command.args(self.arguments());
        command
    }

    fn assert_submitted(&self) {
        let database = self.fixture.database();
        assert_eq!(
            database
                .query_row(
                    "SELECT status FROM tasks WHERE id = ?1",
                    [&self.task],
                    |row| row.get::<_, String>(0)
                )
                .unwrap(),
            "submitted"
        );
        assert_eq!(
            database
                .query_row(
                    "SELECT count(*) FROM operations WHERE id = ?1",
                    [OPERATION],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
        assert!(database.query_row("SELECT target_commit IS NULL FROM workspaces WHERE assignment_id = ?1", [&self.assignment], |row| row.get::<_, bool>(0)).unwrap());
    }
}

#[test]
fn external_closure_accepts_only_the_current_resubmission() {
    let mut closure = ExternalClosure::new();
    let corrected = commit_file(
        &closure.fixture.workspace,
        Path::new("correction.txt"),
        "reviewed correction\n",
        "correct submitted result",
    );
    let resubmit = [
        "task",
        "resubmit",
        "--assignment",
        &closure.assignment,
        "--expected-result",
        &closure.result,
        "--result",
        &corrected,
        "--summary",
        "Validated the correction.",
        "--reason",
        "Original submission omitted a correction.",
        "--operation-id",
        "co-01ARZ3NDEKTSV4RRFFQ69G5FB9",
        "--json",
    ];
    let response = closure.fixture.environment.run_json(&resubmit);
    assert_eq!(run(closure.command()).status.code(), Some(5));
    closure.assert_submitted();

    let repository =
        Repository::open(&closure.fixture.environment.project).unwrap();
    let source = repository.find_commit(corrected.parse().unwrap()).unwrap();
    repository.cherrypick(&source, None).unwrap();
    closure.target = commit_file(
        &closure.fixture.environment.project,
        Path::new("correction.txt"),
        "reviewed correction\n",
        "apply reviewed correction externally",
    );
    repository.cleanup_state().unwrap();
    assert_ne!(closure.target, corrected);

    let mut arguments = closure.arguments();
    let result_index = arguments
        .iter()
        .position(|arg| *arg == "--result-commit")
        .unwrap()
        + 1;
    arguments[result_index] = &corrected;
    let closed = closure.fixture.environment.run_json(&arguments);
    let result = &closed["data"]["task"]["result"];
    assert_eq!(result["assignment_result"]["result_commit"], corrected);
    assert_eq!(result["operator_override"]["result_commit"], corrected);
    assert!(result.get("integration").is_none());
    assert_eq!(closure.fixture.environment.run_json(&resubmit), response);

    let further = commit_file(
        &closure.fixture.workspace,
        Path::new("further.txt"),
        "preserved later work\n",
        "preserve further work",
    );
    let mut arguments = resubmit.to_vec();
    arguments[5] = &corrected;
    arguments[7] = &further;
    arguments[13] = "co-01ARZ3NDEKTSV4RRFFQ69G5FBA";
    let mut command = closure.fixture.environment.command();
    command.args(arguments);
    assert_eq!(run(command).status.code(), Some(5));
    assert!(closure.fixture.database().query_row(
        "SELECT target_commit IS NULL FROM workspaces WHERE assignment_id = ?1",
        [&closure.assignment],
        |row| row.get::<_, bool>(0),
    ).unwrap());
    assert_eq!(closure.fixture.environment.run_json(&resubmit), response);
    assert_repository_clean(&closure.fixture.workspace);
    closure.fixture.environment.run_json(&["stop", "--json"]);
}

#[test]
fn external_closure_records_override_replays_and_releases_dependencies() {
    let closure = ExternalClosure::new();
    let fixture = &closure.fixture.environment;
    let dependent = fixture.run_json(&[
        "task",
        "create",
        "Dependent work",
        "--after",
        &closure.task,
        "--json",
    ]);
    assert_eq!(dependent["data"]["task"]["ready"], false);
    let output = run(closure.command());
    assert!(output.status.success(), "{output:?}");
    let closed: Value = serde_json::from_slice(&output.stdout).unwrap();
    let result = &closed["data"]["task"]["result"];
    assert_eq!(closed["data"]["task"]["status"], "closed");
    assert!(result.get("integration").is_none());
    assert_eq!(result["assignment_result"]["result_commit"], closure.result);
    let evidence = &result["operator_override"];
    assert_eq!(evidence["assignment_id"], closure.assignment);
    assert_eq!(evidence["base_commit"], closure.fixture.base);
    assert_eq!(evidence["result_commit"], closure.result);
    assert_eq!(evidence["target_commit"], closure.target);
    assert_eq!(evidence["reason"], "Reviewed cherry-pick outside Coterie.");
    assert_eq!(
        evidence["validation_summary"],
        "Validated result.txt and the full test suite."
    );
    let database = closure.fixture.database();
    assert!(database.query_row("SELECT target_commit IS NULL FROM workspaces WHERE assignment_id = ?1", [&closure.assignment], |row| row.get::<_, bool>(0)).unwrap());
    let ready = fixture.run_json(&["task", "ready", "--json"]);
    assert_eq!(
        ready["data"]["tasks"][0]["id"],
        dependent["data"]["task"]["id"]
    );
    let events = fixture.run_json(&["events", "--json"]);
    let events = events["data"]["events"].as_array().unwrap();
    assert!(
        !events
            .iter()
            .any(|event| event["event_type"] == "workspace.integrated")
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event["operation_id"] == OPERATION
                && event["event_type"] == "task.lifecycle_changed"
                && event["actor"] == "operator"
                && event["payload"]["data"]["operator_override"] == *evidence)
            .count(),
        1
    );
    fs::write(
        closure.fixture.workspace.join("preserved.txt"),
        "later work",
    )
    .unwrap();
    fs::write(fixture.project.join("later.txt"), "later target work").unwrap();
    let retry = run(closure.command());
    assert_eq!(retry.stdout, output.stdout);
    assert!(retry.status.success());
    let changed_target = closure.fixture.base.clone();
    let mut arguments = closure.arguments();
    let target_index = arguments
        .iter()
        .position(|arg| *arg == "--target-commit")
        .unwrap()
        + 1;
    arguments[target_index] = &changed_target;
    let changed = {
        let mut command = fixture.command();
        command.args(arguments);
        command
    };
    assert_eq!(run(changed).status.code(), Some(5));
    assert!(closure.fixture.workspace.join("preserved.txt").exists());
    fixture.run_json(&["stop", "--json"]);
    assert!(closure.fixture.workspace.exists());
    let worktree = Repository::open(&closure.fixture.workspace).unwrap();
    let head = worktree.head().unwrap();
    let target = Repository::open(&fixture.project).unwrap();
    assert_eq!(
        target
            .find_reference(head.name().unwrap())
            .unwrap()
            .target()
            .unwrap()
            .to_string(),
        closure.result
    );
    assert_eq!(database.query_row("SELECT count(*) FROM events WHERE operation_id = ?1 AND event_type = 'task.lifecycle_changed'", [OPERATION], |row| row.get::<_, i64>(0)).unwrap(), 1);
}

#[test]
fn external_closure_rejects_agents_dirty_targets_and_moved_tips() {
    let mut closure = ExternalClosure::new();
    let mut agent = closure
        .fixture
        .environment
        .agent_command(&closure.fixture.agent_environment);
    agent.args(closure.arguments());
    assert_eq!(run(agent).status.code(), Some(6));
    closure.assert_submitted();
    fs::write(
        closure.fixture.environment.project.join("dirty.txt"),
        "pending",
    )
    .unwrap();
    let dirty = run(closure.command());
    assert_eq!(dirty.status.code(), Some(5), "{dirty:?}");
    closure.assert_submitted();
    fs::remove_file(closure.fixture.environment.project.join("dirty.txt"))
        .unwrap();
    let target = closure.target.clone();
    closure.target = closure.fixture.base.clone();
    assert_eq!(run(closure.command()).status.code(), Some(5));
    closure.assert_submitted();
    closure.target = target;
    let result = closure.result.clone();
    closure.result = closure.fixture.base.clone();
    assert_eq!(run(closure.command()).status.code(), Some(5));
    closure.assert_submitted();
    closure.result = result;
    fs::write(closure.fixture.workspace.join("dirty.txt"), "preserved")
        .unwrap();
    assert_eq!(run(closure.command()).status.code(), Some(5));
    closure.assert_submitted();
    fs::remove_file(closure.fixture.workspace.join("dirty.txt")).unwrap();
    commit_file(
        &closure.fixture.workspace,
        Path::new("later.txt"),
        "preserved\n",
        "later commit",
    );
    assert_eq!(run(closure.command()).status.code(), Some(5));
    closure.assert_submitted();
    closure.fixture.environment.run_json(&["stop", "--json"]);
}

#[test]
fn external_closure_requires_evidence_and_latest_unintegrated_worktree() {
    let mut closure = ExternalClosure::new();
    for (flag, value, expected) in [
        ("--reason", "  ", 2),
        ("--summary", "  ", 2),
        ("--result-commit", "abc123", 2),
        ("--target-commit", "HEAD", 2),
        (
            "--target-commit",
            "0000000000000000000000000000000000000000",
            5,
        ),
        ("--assignment", "ca-01ARZ3NDEKTSV4RRFFQ69G5FB9", 5),
    ] {
        let mut args = closure.arguments();
        let index = args.iter().position(|arg| *arg == flag).unwrap() + 1;
        args[index] = value;
        let mut command = closure.fixture.environment.command();
        command.args(args);
        let output = run(command);
        assert_eq!(output.status.code(), Some(expected), "{flag}: {output:?}");
        closure.assert_submitted();
    }
    let task = closure.fixture.environment.run_json(&[
        "task",
        "create",
        "Unsubmitted work",
        "--json",
    ]);
    let original_task = closure.task.clone();
    closure.task = task["data"]["task"]["id"].as_str().unwrap().to_owned();
    assert_eq!(run(closure.command()).status.code(), Some(5));
    closure.task = original_task;
    closure.assert_submitted();
    closure.fixture.environment.run_json(&[
        "workspace",
        "integrate",
        "--assignment",
        &closure.assignment,
        "--json",
    ]);
    let output = run(closure.command());
    assert_eq!(output.status.code(), Some(5), "{output:?}");
    closure.fixture.environment.run_json(&[
        "task",
        "close",
        &closure.task,
        "--summary",
        "Verified normal integration.",
        "--json",
    ]);
    closure.fixture.environment.run_json(&["stop", "--json"]);
}

#[test]
fn external_closure_denies_agents_with_task_close_capability_and_operator_replays()
 {
    let closure = ExternalClosure::new();
    let fixture = &closure.fixture.environment;
    let capture = fixture.root.join("closure-lead-environment");
    let mut command = fixture.command();
    command
        .env("COTERIE_FAKE_MODE", "contract")
        .env("COTERIE_FAKE_CAPTURE", &capture)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut foreground = command.spawn().unwrap();
    wait_until("closure lead environment", || capture.exists());
    let lead = captured_environment(&capture);
    // Ordinary closure reaches the integration guard, proving task:close authority.
    let mut normal = fixture.agent_command(&lead);
    normal.args([
        "task",
        "close",
        &closure.task,
        "--summary",
        "Reviewed.",
        "--json",
    ]);
    let output = run(normal);
    assert_eq!(output.status.code(), Some(5), "{output:?}");
    for accepted in [false, true] {
        if accepted {
            assert!(run(closure.command()).status.success());
        }
        let mut agent = fixture.agent_command(&lead);
        agent.args(closure.arguments());
        let output = run(agent);
        assert_eq!(output.status.code(), Some(6), "{output:?}");
        let diagnostic: Value = serde_json::from_slice(&output.stderr).unwrap();
        assert!(
            diagnostic["error"]["message"]
                .as_str()
                .unwrap()
                .contains("only the operator")
        );
    }
    foreground
        .stdin
        .take()
        .unwrap()
        .write_all(b"exit\n")
        .unwrap();
    assert!(foreground.wait_with_output().unwrap().status.success());
    fixture.run_json(&["stop", "--json"]);
}

#[test]
fn closure_override_help_names_authority_evidence_and_preservation() {
    let mut command = Command::new(env!("CARGO_BIN_EXE_coterie"));
    command.args(["task", "close", "--help"]);
    let output = run(command);
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).unwrap();
    for text in [
        "operator only",
        "Preserves",
        "--result-commit",
        "--target-commit",
        "--reason",
        "validation evidence",
    ] {
        assert!(help.contains(text), "{help}");
    }
}

#[test]
fn external_closure_serializes_with_coterie_integration() {
    let closure = ExternalClosure::new();
    let fixture = &closure.fixture.environment;
    let mut override_command = closure.command();
    let child = override_command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut integration = fixture.command();
    integration.args([
        "workspace",
        "integrate",
        "--assignment",
        &closure.assignment,
        "--json",
    ]);
    let integrated = run(integration);
    let accepted = child.wait_with_output().unwrap();
    assert_ne!(
        accepted.status.success(),
        integrated.status.success(),
        "{accepted:?} {integrated:?}"
    );
    let (rejected, expected_status) = if accepted.status.success() {
        (&integrated, "closed")
    } else {
        (&accepted, "submitted")
    };
    assert_eq!(rejected.status.code(), Some(5));
    let database = closure.fixture.database();
    let status: String = database
        .query_row(
            "SELECT status FROM tasks WHERE id = ?1",
            [&closure.task],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(status, expected_status);
    let integration_missing: bool = database.query_row("SELECT target_commit IS NULL FROM workspaces WHERE assignment_id = ?1", [&closure.assignment], |row| row.get(0)).unwrap();
    assert_eq!(integration_missing, accepted.status.success());
    fixture.run_json(&["stop", "--json"]);
}

#[test]
fn external_closure_requires_fresh_validation_after_target_advances() {
    let mut closure = ExternalClosure::new();
    let next = commit_file(
        &closure.fixture.environment.project,
        Path::new("later-target.txt"),
        "additional work\n",
        "advance target",
    );
    let rejected = run(closure.command());
    assert_eq!(rejected.status.code(), Some(5), "{rejected:?}");
    closure.assert_submitted();
    closure.target = next;
    assert!(run(closure.command()).status.success());
    closure.fixture.environment.run_json(&["stop", "--json"]);
}

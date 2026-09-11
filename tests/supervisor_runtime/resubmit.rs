use super::*;

const RESUBMIT: &str = "co-01ARZ3NDEKTSV4RRFFQ69G5FB9";

fn resubmit_command(fixture: &FinishFixture, old: &str, new: &str) -> Command {
    resubmit_operation(fixture, old, new, RESUBMIT)
}

fn resubmit_operation(
    fixture: &FinishFixture,
    old: &str,
    new: &str,
    operation: &str,
) -> Command {
    let assignment: String = fixture
        .database()
        .query_row("SELECT id FROM assignments", [], |row| row.get(0))
        .unwrap();
    let mut command = fixture.environment.command();
    command.args([
        "task",
        "resubmit",
        "--assignment",
        &assignment,
        "--expected-result",
        old,
        "--result",
        new,
        "--summary",
        "Validated the corrected result.",
        "--reason",
        "The original submission omitted the final commit.",
        "--operation-id",
        operation,
        "--json",
    ]);
    command
}

#[test]
fn resubmit_preserves_history_replays_and_integrates_the_correction() {
    let fixture = FinishFixture::new("worker", "Correct a submission");
    let original = fixture.finish("completed");
    let dependent = fixture.environment.run_json(&[
        "task",
        "create",
        "Consume corrected result",
        "--after",
        original["data"]["task"]["id"].as_str().unwrap(),
        "--json",
    ]);
    let old_operation: String = fixture
        .database()
        .query_row(
            "SELECT request_json FROM operations WHERE id = ?1",
            [OPERATION],
            |row| row.get(0),
        )
        .unwrap();
    let corrected = commit_file(
        &fixture.workspace,
        Path::new("fix.txt"),
        "corrected\n",
        "correct submission",
    );
    let output = run(resubmit_command(&fixture, &fixture.base, &corrected));
    assert!(output.status.success(), "{output:?}");
    let response: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response["data"]["task"]["status"], "submitted");
    assert_eq!(
        fixture.environment.run_json(&["task", "ready", "--json"])["data"]["tasks"],
        serde_json::json!([])
    );
    assert_eq!(
        response["data"]["task"]["result"]["result_commit"],
        corrected
    );
    assert_eq!(
        response["data"]["previous_result"],
        original["data"]["task"]["result"]
    );
    assert_eq!(
        fixture.finish("completed"),
        original,
        "a late original finish retry must retain its original submission"
    );
    let assignment = response["data"]["assignment_id"].as_str().unwrap();
    let database = fixture.database();
    assert_eq!(
        database
            .query_row(
                "SELECT request_json FROM operations WHERE id = ?1",
                [OPERATION],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
        old_operation
    );
    let event: String = database.query_row("SELECT payload_json FROM events WHERE event_type = 'task.resubmitted'", [], |row| row.get(0)).unwrap();
    let event: Value = serde_json::from_str(&event).unwrap();
    assert_eq!(
        event["data"]["previous_result"],
        original["data"]["task"]["result"]
    );
    assert_eq!(
        event["data"]["previous_summary"],
        "Validated the assignment."
    );
    assert!(database.query_row("SELECT state = 'completed' AND completed_at IS NOT NULL FROM assignments", [], |row| row.get::<_, bool>(0)).unwrap());
    assert!(
        Repository::open(&fixture.workspace)
            .unwrap()
            .graph_descendant_of(
                corrected.parse().unwrap(),
                fixture.base.parse().unwrap()
            )
            .unwrap()
    );
    let integrated = fixture.environment.run_json(&[
        "workspace",
        "integrate",
        "--assignment",
        assignment,
        "--json",
    ]);
    assert_eq!(
        integrated["data"]["integration"]["result_commit"],
        corrected
    );
    let replay = run(resubmit_command(&fixture, &fixture.base, &corrected));
    assert!(replay.status.success(), "{replay:?}");
    assert_eq!(
        serde_json::from_slice::<Value>(&replay.stdout).unwrap(),
        response
    );
    assert_eq!(database.query_row("SELECT count(*) FROM events WHERE event_type = 'task.resubmitted'", [], |row| row.get::<_, i64>(0)).unwrap(), 1);
    fixture.environment.run_json(&[
        "task",
        "close",
        original["data"]["task"]["id"].as_str().unwrap(),
        "--summary",
        "Validated the integrated correction.",
        "--json",
    ]);
    assert_eq!(
        fixture.environment.run_json(&["task", "ready", "--json"])["data"]["tasks"]
            [0]["id"],
        dependent["data"]["task"]["id"]
    );
    fixture.environment.run_json(&["stop", "--json"]);
}

fn assert_conflict(output: std::process::Output, message: &str) {
    assert_eq!(output.status.code(), Some(5), "{output:?}");
    assert!(output.stdout.is_empty());
    let error: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert!(
        error["error"]["message"]
            .as_str()
            .unwrap()
            .contains(message),
        "{error}"
    );
}

#[test]
fn resubmit_refuses_dirty_mismatched_and_rewritten_work_without_consuming_retries()
 {
    let fixture = FinishFixture::new("worker", "Guard correction ancestry");
    fixture.finish("completed");
    let corrected = commit_file(
        &fixture.workspace,
        Path::new("fix.txt"),
        "correction\n",
        "correction",
    );
    fs::write(fixture.workspace.join("pending.txt"), "unfinished\n").unwrap();
    assert_conflict(
        run(resubmit_command(&fixture, &fixture.base, &corrected)),
        "task resubmit",
    );
    fs::remove_file(fixture.workspace.join("pending.txt")).unwrap();
    assert_conflict(
        run(resubmit_command(&fixture, &"a".repeat(40), &corrected)),
        "--expected-result",
    );
    assert_conflict(
        run(resubmit_command(&fixture, &fixture.base, &"a".repeat(40))),
        "tip",
    );
    assert_eq!(
        fixture
            .database()
            .query_row(
                "SELECT count(*) FROM operations WHERE id = ?1",
                [RESUBMIT],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    let output = run(resubmit_command(&fixture, &fixture.base, &corrected));
    assert!(output.status.success(), "{output:?}");
    let repository = Repository::open(&fixture.workspace).unwrap();
    let old_tree = repository
        .find_commit(fixture.base.parse().unwrap())
        .unwrap()
        .tree()
        .unwrap();
    let signature = Signature::now("Test", "test@example.invalid").unwrap();
    let rewritten = repository
        .commit(None, &signature, &signature, "rewritten", &old_tree, &[])
        .unwrap();
    repository
        .reset(
            &repository.find_object(rewritten, None).unwrap(),
            git2::ResetType::Hard,
            None,
        )
        .unwrap();
    assert_conflict(
        run(resubmit_operation(
            &fixture,
            &corrected,
            &rewritten.to_string(),
            "co-01ARZ3NDEKTSV4RRFFQ69G5FBA",
        )),
        "descend",
    );
    fixture.environment.run_json(&["stop", "--json"]);
}

#[test]
fn resubmit_rejects_unauthorized_workers_and_integrated_results() {
    let fixture = FinishFixture::new("worker", "Enforce correction authority");
    let submitted = fixture.finish("completed");
    let corrected = commit_file(
        &fixture.workspace,
        Path::new("fix.txt"),
        "correction\n",
        "correction",
    );
    let mut denied = resubmit_command(&fixture, &fixture.base, &corrected);
    denied.envs(
        fixture
            .agent_environment
            .iter()
            .map(|(key, value)| (key, value)),
    );
    let output = run(denied);
    assert_eq!(output.status.code(), Some(6), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stderr).contains("task:resubmit"));
    let response = run(resubmit_command(&fixture, &fixture.base, &corrected));
    assert!(response.status.success(), "{response:?}");
    let assignment = submitted["data"]["assignment_id"].as_str().unwrap();
    fixture.environment.run_json(&[
        "workspace",
        "integrate",
        "--assignment",
        assignment,
        "--json",
    ]);
    let later = commit_file(
        &fixture.workspace,
        Path::new("later.txt"),
        "later\n",
        "later correction",
    );
    assert_conflict(
        run(resubmit_operation(
            &fixture,
            &corrected,
            &later,
            "co-01ARZ3NDEKTSV4RRFFQ69G5FBA",
        )),
        "already integrated",
    );
    assert_eq!(
        fixture
            .database()
            .query_row("SELECT result_commit FROM workspaces", [], |row| row
                .get::<_, String>(
                0
            ))
            .unwrap(),
        corrected
    );
    fixture.environment.run_json(&["stop", "--json"]);
}

#[test]
fn resubmit_serializes_with_concurrent_integration_and_competing_retries() {
    let fixture =
        FinishFixture::new("worker", "Serialize correction admission");
    let original = fixture.finish("completed");
    let corrected = commit_file(
        &fixture.workspace,
        Path::new("fix.txt"),
        "correction\n",
        "correction",
    );
    let first = resubmit_command(&fixture, &fixture.base, &corrected);
    let retry = resubmit_command(&fixture, &fixture.base, &corrected);
    let mut integrate = fixture.environment.command();
    integrate.args([
        "workspace",
        "integrate",
        "--assignment",
        original["data"]["assignment_id"].as_str().unwrap(),
        "--json",
    ]);
    let outputs = thread::scope(|scope| {
        let first = scope.spawn(|| run(first));
        let retry = scope.spawn(|| run(retry));
        let integrate = scope.spawn(|| run(integrate));
        (
            first.join().unwrap(),
            retry.join().unwrap(),
            integrate.join().unwrap(),
        )
    });
    assert!(outputs.0.status.success(), "{:?}", outputs.0);
    assert_eq!(outputs.0.stdout, outputs.1.stdout);
    if !outputs.2.status.success() {
        assert_conflict(outputs.2, "task resubmit");
    }
    assert_eq!(fixture.database().query_row("SELECT count(*) FROM events WHERE event_type = 'task.resubmitted'", [], |row| row.get::<_, i64>(0)).unwrap(), 1);
    assert_eq!(
        fixture
            .database()
            .query_row("SELECT result_commit FROM workspaces", [], |row| row
                .get::<_, String>(
                0
            ))
            .unwrap(),
        corrected
    );
    fixture.environment.run_json(&["stop", "--json"]);
}

use super::*;

const COMPLETING_JOB: &str = r#"
if [ -z "${COTERIE_TASK_ID-}" ]; then exit 0; fi
capture="$COTERIE_SOCKET.$COTERIE_AGENT_ID"
{
  for argument in "$@"; do printf 'arg=%s\0' "$argument"; done
  for variable in COTERIE_PROJECT_ROOT COTERIE_PROJECT_ID COTERIE_PRIMARY_PROJECT_ROOT COTERIE_RUN_ID COTERIE_AGENT_ID COTERIE_SESSION_ID COTERIE_ROLE COTERIE_SOCKET COTERIE_TOKEN COTERIE_TASK_ID COTERIE_BIN; do
    eval "value=\${$variable}"
    printf 'env:%s=%s\0' "$variable" "$value"
  done
} > "$capture.pending"
mv "$capture.pending" "$capture"
printf '{"type":"thread.started","thread_id":"workflow-job"}\n'
while [ ! -e "$capture.release" ]; do
  if [ ! -e "/proc/$PPID" ]; then exit 0; fi
  sleep 0.02
done
"$COTERIE_BIN" finish --status completed --summary 'Implementation validated.' --json > /dev/null || exit 22
"$COTERIE_BIN" send planner "$COTERIE_TASK_ID submitted for review" --json > /dev/null || exit 23
printf '{"type":"turn.completed","usage":{}}\n'
"#;

#[test]
fn coordinator_polls_multiple_completions_through_validated_closure() {
    let fixture = TestEnvironment::new();
    write_global(
        &fixture,
        &include_str!("../../examples/config/global.toml")
            .replace("coordinator", "planner")
            .replace("\"logs:*\"]", "\"logs:*\", \"workspace:integrate\"]"),
    );
    fs::write(
        fixture.root.join("bin/codex"),
        format!(
            "{}{}",
            FAKE_CODEX.split_once("is_job=false").unwrap().0,
            COMPLETING_JOB
        ),
    )
    .unwrap();
    let instructions = "Preserve these project conventions.\n";
    commit_file(
        &fixture.project,
        Path::new("AGENTS.md"),
        instructions,
        "record project conventions",
    );
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
    wait_until("planner bootstrap", || capture.exists());
    let planner = captured_environment(&capture);
    let bootstrap = fs::read_to_string(&capture).unwrap();
    assert!(bootstrap.contains("If you are coordinating delegated work"));
    assert!(bootstrap.contains("progress --after <progress_cursor> --wait 5"));
    assert!(bootstrap.contains("workspace integrate --assignment"));
    assert!(bootstrap.contains("task close <task_id> --summary"));
    let request = |args: &[&str]| fixture.run_agent_json(args, &planner);
    let mut jobs = Vec::new();
    for file in ["alpha.txt", "beta.txt"] {
        let task = request(&["task", "create", file, "--json"]);
        let task_id = task["data"]["task"]["id"].as_str().unwrap().to_owned();
        let spawned =
            request(&["spawn", "builder", "--task", &task_id, "--json"]);
        let agent_id =
            spawned["data"]["agent"]["id"].as_str().unwrap().to_owned();
        let capture = PathBuf::from(format!(
            "{}.{}",
            environment_value(&planner, "COTERIE_SOCKET"),
            agent_id
        ));
        wait_until("builder bootstrap", || capture.exists());
        let environment = captured_environment(&capture);
        let workspace = PathBuf::from(environment_value(
            &environment,
            "COTERIE_PROJECT_ROOT",
        ));
        let bootstrap = fs::read_to_string(&capture).unwrap();
        assert!(bootstrap.contains("If you are coordinating delegated work"));
        assert!(
            bootstrap.contains("progress --after <progress_cursor> --wait 5")
        );
        assert!(!bootstrap.contains("workspace integrate --assignment"));
        assert!(!bootstrap.contains("task close <task_id> --summary"));
        assert_eq!(
            fs::read_to_string(workspace.join("AGENTS.md")).unwrap(),
            instructions
        );
        let result = commit_file(
            &workspace,
            Path::new(file),
            file,
            "implement assigned file",
        );
        jobs.push((
            task_id,
            spawned,
            capture,
            workspace,
            result,
            file,
            environment,
        ));
    }
    let dependent = request(&[
        "task",
        "create",
        "Use both results",
        "--after",
        &jobs[0].0,
        "--after",
        &jobs[1].0,
        "--json",
    ]);
    let dependent_id = dependent["data"]["task"]["id"].as_str().unwrap();
    let mut cursor = String::new();
    loop {
        let mut args = vec!["progress", "--limit", "1", "--json"];
        if !cursor.is_empty() {
            args.extend(["--after", &cursor]);
        }
        let page = request(&args);
        cursor = page["data"]["next_cursor"].as_str().unwrap().to_owned();
        if page["data"]["has_more"] == false {
            break;
        }
    }

    // Messages can arrive without a lifecycle change to wake a progress reader.
    fixture.run_agent_json(
        &["send", "planner", "Ready for coordination", "--json"],
        &jobs[0].6,
    );
    let empty =
        request(&["progress", "--after", &cursor, "--wait", "1", "--json"]);
    assert_eq!(empty["data"]["changes"], serde_json::json!([]));
    assert_eq!(empty["data"]["timed_out"], true);
    cursor = empty["data"]["next_cursor"].as_str().unwrap().to_owned();
    let inbox = request(&["inbox", "--after", "0", "--json"]);
    assert_eq!(
        inbox["data"]["messages"][0]["body"],
        "Ready for coordination"
    );
    assert_eq!(inbox["data"]["messages"][0]["acknowledged"], false);
    assert_eq!(request(&["inbox", "--after", "0", "--json"]), inbox);
    let mut inbox_cursor = inbox["data"]["next_cursor"].as_u64().unwrap();
    request(&["inbox", "ack", &inbox_cursor.to_string(), "--json"]);

    let mut handled = Vec::new();
    for (index, (task_id, spawned, capture, workspace, result, file, _)) in
        jobs.iter().enumerate()
    {
        fs::write(format!("{}.release", capture.display()), "complete")
            .unwrap();
        let mut submitted = false;
        let mut exited = false;
        let deadline = Instant::now() + Duration::from_secs(15);
        while !submitted || !exited || !handled.contains(task_id) {
            assert!(
                Instant::now() < deadline,
                "completion must remain observable"
            );
            assert!(foreground.try_wait().unwrap().is_none());
            let page = request(&[
                "progress", "--after", &cursor, "--wait", "5", "--limit", "1",
                "--json",
            ]);
            cursor = page["data"]["next_cursor"].as_str().unwrap().to_owned();
            for change in page["data"]["changes"].as_array().unwrap() {
                submitted |= change["kind"] == "task"
                    && change["task_id"] == *task_id
                    && change["status"] == "submitted";
                exited |= change["kind"] == "agent"
                    && change["agent_id"] == spawned["data"]["agent"]["id"]
                    && change["state"] == "exited";
            }
            let inbox = request(&[
                "inbox",
                "--after",
                &inbox_cursor.to_string(),
                "--json",
            ]);
            for message in inbox["data"]["messages"].as_array().unwrap() {
                assert_eq!(message["acknowledged"], false);
                assert_eq!(
                    message["body"],
                    format!("{task_id} submitted for review")
                );
                let prime = request(&["prime", "--json"]);
                let task = prime["data"]["tasks"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|task| task["id"] == *task_id)
                    .unwrap();
                assert_eq!(task["status"], "submitted");
                assert_eq!(task["result"]["result_commit"], *result);
                // The scripted coordinator reviews the exact submitted tree before acknowledging.
                let repository = Repository::open(workspace).unwrap();
                let commit =
                    repository.find_commit(result.parse().unwrap()).unwrap();
                let tree = commit.tree().unwrap();
                let diff = repository
                    .diff_tree_to_tree(
                        Some(&commit.parent(0).unwrap().tree().unwrap()),
                        Some(&tree),
                        None,
                    )
                    .unwrap();
                assert_eq!(diff.deltas().len(), 1);
                assert_eq!(
                    diff.get_delta(0).unwrap().new_file().path(),
                    Some(Path::new(file))
                );
                let blob = repository
                    .find_blob(tree.get_path(Path::new(file)).unwrap().id())
                    .unwrap();
                assert_eq!(blob.content(), file.as_bytes());
                handled.push(task_id.clone());
            }
            inbox_cursor = inbox["data"]["next_cursor"].as_u64().unwrap();
            request(&["inbox", "ack", &inbox_cursor.to_string(), "--json"]);
        }
        assert!(
            request(&["task", "ready", "--json"])["data"]["tasks"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        let premature = run({
            let mut command = fixture.agent_command(&planner);
            command.args([
                "task",
                "close",
                task_id,
                "--summary",
                "Not integrated",
                "--json",
            ]);
            command
        });
        assert_eq!(premature.status.code(), Some(5));
        let integrated = request(&[
            "workspace",
            "integrate",
            "--assignment",
            spawned["data"]["assignment_id"].as_str().unwrap(),
            "--json",
        ]);
        assert_eq!(
            fs::read_to_string(fixture.project.join(file)).unwrap(),
            *file
        );
        assert_repository_clean(&fixture.project);
        let target = Repository::open(&fixture.project).unwrap();
        let target_commit = target.head().unwrap().target().unwrap();
        assert!(
            target_commit.to_string() == *result
                || target
                    .graph_descendant_of(target_commit, result.parse().unwrap())
                    .unwrap()
        );
        assert_eq!(
            integrated["data"]["integration"]["target_commit"],
            target_commit.to_string()
        );
        let closed = request(&[
            "task",
            "close",
            task_id,
            "--summary",
            "Reviewed submitted diff; verified integrated file and clean target.",
            "--json",
        ]);
        assert_eq!(closed["data"]["task"]["status"], "closed");
        assert_eq!(
            closed["data"]["task"]["result"]["integration"]["result_commit"],
            *result
        );
        assert_eq!(
            closed["data"]["task"]["result"]["integration"]["target_commit"],
            target_commit.to_string()
        );
        assert_eq!(
            closed["data"]["task"]["result"]["validation_summary"],
            "Reviewed submitted diff; verified integrated file and clean target."
        );
        let ready = request(&["task", "ready", "--json"]);
        let tasks = ready["data"]["tasks"].as_array().unwrap();
        assert_eq!(tasks.len(), usize::from(index == 1));
        if index == 1 {
            assert_eq!(tasks[0]["id"], dependent_id);
        }
    }
    assert_eq!(handled.len(), 2);
    assert_eq!(inbox_cursor, 3);
    let inbox = request(&["inbox", "--after", "0", "--json"]);
    assert!(
        inbox["data"]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .all(|message| message["acknowledged"] == true)
    );
    assert!(request(&["inbox", "--after", &inbox_cursor.to_string(), "--json"])["data"]["messages"].as_array().unwrap().is_empty());
    assert_eq!(
        fs::read_to_string(fixture.project.join("AGENTS.md")).unwrap(),
        instructions
    );
    drop(foreground.stdin.take());
    assert!(foreground.wait().unwrap().success());
    fixture.run_json(&["stop", "--json"]);
}

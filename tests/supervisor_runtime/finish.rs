use super::*;

#[path = "resubmit.rs"]
mod resubmit;

const OPERATION: &str = "co-01ARZ3NDEKTSV4RRFFQ69G5FB8";

#[test]
fn dirty_finish_preserves_active_work_and_retries_after_commit() {
    for change in ["staged", "unstaged", "untracked", "hook"] {
        let fixture = FinishFixture::new("worker", "Implement and validate");
        let repository = Repository::open(&fixture.workspace).unwrap();
        let path = if change == "untracked" {
            fs::create_dir(fixture.workspace.join("nested")).unwrap();
            "nested/new.txt"
        } else {
            "README.md"
        };
        fs::write(fixture.workspace.join(path), "unfinished\n").unwrap();
        if matches!(change, "staged" | "hook") {
            let mut index = repository.index().unwrap();
            index.add_path(Path::new(path)).unwrap();
            index.write().unwrap();
        }
        let hook_directory = fixture.environment.root.join("hooks");
        if change == "hook" {
            fs::create_dir(&hook_directory).unwrap();
            let hook = hook_directory.join("pre-commit");
            fs::write(
                &hook,
                "#!/bin/sh\nprintf 'validation failed\\n' >&2\nexit 1\n",
            )
            .unwrap();
            fs::set_permissions(&hook, fs::Permissions::from_mode(0o700))
                .unwrap();
            let output = run({
                let mut command = Command::new("git");
                command.current_dir(&fixture.workspace).args([
                    "-c",
                    "user.name=Coterie Test",
                    "-c",
                    "user.email=test@example.invalid",
                    "-c",
                    &format!("core.hooksPath={}", hook_directory.display()),
                    "commit",
                    "-m",
                    "implement result",
                ]);
                command
            });
            assert!(!output.status.success(), "the real Git hook must fail");
            assert!(
                String::from_utf8_lossy(&output.stderr)
                    .contains("validation failed")
            );
            assert_eq!(
                repository.head().unwrap().target().unwrap().to_string(),
                fixture.base
            );
        }

        for _ in 0..2 {
            let output = run(fixture.finish_command("completed"));
            assert_eq!(output.status.code(), Some(5), "{change}: {output:?}");
            assert!(output.stdout.is_empty());
            let error: Value = serde_json::from_slice(&output.stderr).unwrap();
            assert_eq!(error["error"]["code"], "conflict");
            let message = error["error"]["message"].as_str().unwrap();
            assert!(message.contains(path), "{change}: {message}");
            assert!(
                message.contains("commit") && message.contains("finish"),
                "{message}"
            );
            fixture.assert_active();
            assert_eq!(
                fs::read_to_string(fixture.workspace.join(path)).unwrap(),
                "unfinished\n"
            );
        }

        let result = if change == "hook" {
            fs::write(hook_directory.join("pre-commit"), "#!/bin/sh\nexit 0\n")
                .unwrap();
            let output = run({
                let mut command = Command::new("git");
                command.current_dir(&fixture.workspace).args([
                    "-c",
                    "user.name=Coterie Test",
                    "-c",
                    "user.email=test@example.invalid",
                    "-c",
                    &format!("core.hooksPath={}", hook_directory.display()),
                    "commit",
                    "-m",
                    "implement result",
                ]);
                command
            });
            assert!(output.status.success(), "{output:?}");
            repository.head().unwrap().target().unwrap().to_string()
        } else {
            commit_file(
                &fixture.workspace,
                Path::new(path),
                "validated\n",
                "implement result",
            )
        };
        assert_ne!(result, fixture.base);
        let submitted = fixture.finish("completed");
        assert_eq!(submitted["data"]["task"]["status"], "submitted");
        assert_eq!(
            submitted["data"]["task"]["result"]["result_commit"],
            result
        );
        fs::write(
            fixture.workspace.join("after-submission.txt"),
            "later work\n",
        )
        .unwrap();
        assert_eq!(
            fixture.finish("completed"),
            submitted,
            "a successful retry must replay without rechecking later dirt"
        );
        fixture.environment.run_json(&["stop", "--json"]);
    }
}

#[test]
fn unreadable_finish_preserves_active_work() {
    let fixture = FinishFixture::new("worker", "Preserve unreadable work");
    let directory = fixture.workspace.join("pending");
    fs::create_dir(&directory).unwrap();
    fs::write(directory.join("result.txt"), "unfinished\n").unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o444)).unwrap();
    let output = run(fixture.finish_command("completed"));
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(output.status.code(), Some(5), "{output:?}");
    let error: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["error"]["code"], "conflict");
    assert!(
        error["error"]["message"]
            .as_str()
            .unwrap()
            .contains("pending")
    );
    fixture.assert_active();
    fixture.environment.run_json(&["stop", "--json"]);
}

#[test]
fn large_dirty_finish_returns_bounded_conflict_and_preserves_active_work() {
    let fixture =
        FinishFixture::new("worker", "Report a large unfinished tree");
    for index in 0..5000 {
        fs::write(
            fixture
                .workspace
                .join(format!("{index:04}-{}", "x".repeat(230))),
            "unfinished\n",
        )
        .unwrap();
    }
    let output = run(fixture.finish_command("completed"));
    assert_eq!(output.status.code(), Some(5), "{output:?}");
    assert!(output.stdout.is_empty());
    assert!(output.stderr.len() < 16 * 1024);
    let error: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["error"]["code"], "conflict");
    let message = error["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("0000-")
            && message.contains("additional paths omitted"),
        "{message}"
    );
    fixture.assert_active();
    fixture.environment.run_json(&["stop", "--json"]);
}

#[test]
fn clean_finish_accepts_unchanged_worktrees_reviews_and_non_code_work() {
    for (role, title) in [
        ("worker", "Verify an existing implementation"),
        ("worker", "Report research findings without code changes"),
        ("reviewer", "Review the implementation"),
    ] {
        let fixture = FinishFixture::new(role, title);
        fs::write(
            fixture.environment.project.join(".git/info/exclude"),
            "ignored-output/\n",
        )
        .unwrap();
        fs::create_dir(fixture.workspace.join("ignored-output")).unwrap();
        fs::write(
            fixture.workspace.join("ignored-output/log"),
            "validation output\n",
        )
        .unwrap();
        let submitted = fixture.finish("completed");
        assert_eq!(submitted["data"]["task"]["status"], "submitted");
        if role == "worker" {
            assert_eq!(
                submitted["data"]["task"]["result"]["base_commit"],
                fixture.base
            );
            assert_eq!(
                submitted["data"]["task"]["result"]["result_commit"],
                fixture.base
            );
        } else {
            assert!(
                submitted["data"]["task"]["result"]
                    .get("result_commit")
                    .is_none()
            );
        }
        fixture.environment.run_json(&["stop", "--json"]);
    }
}

#[test]
fn failed_finish_preserves_dirty_work_and_reopens_the_task() {
    let fixture =
        FinishFixture::new("worker", "Investigate an incomplete change");
    fs::write(fixture.workspace.join("unfinished.txt"), "preserve this\n")
        .unwrap();
    let finished = fixture.finish("failed");
    assert_eq!(finished["data"]["task"]["status"], "open");
    assert_eq!(
        fs::read_to_string(fixture.workspace.join("unfinished.txt")).unwrap(),
        "preserve this\n"
    );
    assert!(
        fixture
            .database()
            .query_row(
                "SELECT result_commit IS NULL FROM workspaces",
                [],
                |row| row.get::<_, bool>(0)
            )
            .unwrap()
    );
    fixture.environment.run_json(&["stop", "--json"]);
}

#[test]
fn finish_help_explains_validation_commit_and_retry() {
    let mut command = Command::new(env!("CARGO_BIN_EXE_coterie"));
    command.args(["finish", "--help"]);
    let output = run(command);
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).unwrap();
    for text in [
        "Validate",
        "commit",
        "untracked",
        "active",
        "retry",
        "no new commit",
    ] {
        assert!(help.contains(text), "missing {text}: {help}");
    }
}

pub(super) struct FinishFixture {
    pub(super) environment: TestEnvironment,
    pub(super) agent_environment: Vec<(String, String)>,
    pub(super) workspace: PathBuf,
    run_id: String,
    pub(super) base: String,
}

impl FinishFixture {
    pub(super) fn new(role: &str, title: &str) -> Self {
        let environment = TestEnvironment::new();
        let capture = r#"parent=$PPID
  capture="$COTERIE_SOCKET.$COTERIE_AGENT_ID"
  {
    for variable in COTERIE_PROJECT_ROOT COTERIE_PROJECT_ID COTERIE_PRIMARY_PROJECT_ROOT COTERIE_RUN_ID COTERIE_AGENT_ID COTERIE_SESSION_ID COTERIE_ROLE COTERIE_SOCKET COTERIE_TOKEN COTERIE_TASK_ID; do
      eval "value=\${$variable}"
      printf 'env:%s=%s\0' "$variable" "$value"
    done
  } > "$capture"
  printf ready > "$capture.ready""#;
        fs::write(
            environment.root.join("bin/codex"),
            FAKE_CODEX.replace("parent=$PPID", capture),
        )
        .unwrap();
        environment.launch(&[]);
        let task = environment.run_json(&["task", "create", title, "--json"]);
        let spawn = environment.run_json(&[
            "spawn",
            role,
            "--task",
            task["data"]["task"]["id"].as_str().unwrap(),
            "--json",
        ]);
        let prime = environment.run_json(&["prime", "--json"]);
        let run_id = prime["data"]["identity"]["run_id"]
            .as_str()
            .unwrap()
            .to_owned();
        let agent_id = spawn["data"]["agent"]["id"].as_str().unwrap();
        let capture = environment
            .runtime
            .join("coterie")
            .join(format!("{run_id}.sock.{agent_id}"));
        wait_until("the assignment environment", || {
            capture.with_extension(format!("{agent_id}.ready")).exists()
        });
        let agent_environment = captured_environment(&capture);
        let workspace = PathBuf::from(environment_value(
            &agent_environment,
            "COTERIE_PROJECT_ROOT",
        ));
        let base = Repository::open(&workspace)
            .unwrap()
            .head()
            .unwrap()
            .target()
            .unwrap()
            .to_string();
        Self {
            environment,
            agent_environment,
            workspace,
            run_id,
            base,
        }
    }

    fn finish_command(&self, status: &str) -> Command {
        let mut command =
            self.environment.agent_command(&self.agent_environment);
        command.args([
            "finish",
            "--status",
            status,
            "--summary",
            "Validated the assignment.",
            "--operation-id",
            OPERATION,
            "--json",
        ]);
        command
    }

    pub(super) fn finish(&self, status: &str) -> Value {
        let output = run(self.finish_command(status));
        assert!(output.status.success(), "{output:?}");
        assert!(output.stderr.is_empty(), "{output:?}");
        serde_json::from_slice(&output.stdout).unwrap()
    }

    pub(super) fn database(&self) -> rusqlite::Connection {
        rusqlite::Connection::open_with_flags(
            self.environment
                .state
                .join("coterie/runs")
                .join(&self.run_id)
                .join("state.sqlite3"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap()
    }

    fn assert_active(&self) {
        let database = self.database();
        for query in [
            "SELECT status = 'in_progress' AND result_json IS NULL FROM tasks",
            "SELECT state = 'active' AND released_at IS NULL FROM claims",
            "SELECT state = 'active' AND completed_at IS NULL AND summary IS NULL FROM assignments",
            "SELECT result_commit IS NULL FROM workspaces",
        ] {
            assert!(
                database
                    .query_row(query, [], |row| row.get::<_, bool>(0))
                    .unwrap(),
                "{query}"
            );
        }
        for query in [
            "SELECT count(*) FROM operations WHERE id = ?1",
            "SELECT count(*) FROM events WHERE operation_id = ?1",
        ] {
            assert_eq!(
                database
                    .query_row(query, [OPERATION], |row| row.get::<_, i64>(0))
                    .unwrap(),
                0
            );
        }
        let prime = self
            .environment
            .run_agent_json(&["prime", "--json"], &self.agent_environment);
        assert_eq!(prime["data"]["active_task"]["status"], "in_progress");
    }
}

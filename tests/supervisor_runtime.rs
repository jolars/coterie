use std::fs;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;

const RUN_ID: &str = "cr-01ARZ3NDEKTSV4RRFFQ69G5FAV";
const PROJECT_ID: &str = "cp-01ARZ3NDEKTSV4RRFFQ69G5FAW";

#[test]
fn public_help_lists_the_minimum_delegation_commands() {
    let fixture = TestEnvironment::new();
    let mut command = fixture.command();
    command.arg("--help");

    let output = run(command);

    assert!(output.status.success(), "help failed: {output:?}");
    let stdout =
        String::from_utf8(output.stdout).expect("help should be UTF-8");
    for command in [
        "status", "whoami", "prime", "task", "spawn", "finish", "send",
        "inbox", "logs", "events", "stop",
    ] {
        assert!(
            stdout.contains(command),
            "help should list `{command}`:\n{stdout}"
        );
    }
    assert!(!stdout.contains("__supervisor"));

    let mut command = fixture.command();
    command.args(["task", "--help"]);
    let output = run(command);
    assert!(output.status.success(), "task help failed: {output:?}");
    let stdout =
        String::from_utf8(output.stdout).expect("task help should be UTF-8");
    for command in ["create", "ready", "close"] {
        assert!(
            stdout.contains(command),
            "task help should list `{command}`:\n{stdout}"
        );
    }
}

#[test]
fn invalid_programmatic_arguments_use_the_versioned_error_contract() {
    let fixture = TestEnvironment::new();
    let mut command = fixture.command();
    command.args(["status", "--unknown", "--json"]);

    let output = run(command);

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let error: Value = serde_json::from_slice(&output.stderr)
        .expect("the diagnostic should be one JSON response");
    assert_eq!(error["schema_version"], 1);
    assert_eq!(error["error"]["code"], "invalid_argument");
    assert!(error.get("operation_id").is_none());
}

#[test]
fn mutation_connection_failures_preserve_the_allocated_operation_id() {
    let fixture = TestEnvironment::new();
    let mut command = fixture.command();
    command.args([
        "task",
        "create",
        "Unreachable mutation",
        "--operation-id",
        "co-01ARZ3NDEKTSV4RRFFQ69G5FB4",
        "--json",
    ]);

    let output = run(command);

    assert_eq!(output.status.code(), Some(4));
    assert!(output.stdout.is_empty());
    let error: Value = serde_json::from_slice(&output.stderr)
        .expect("the diagnostic should be one JSON response");
    assert_eq!(error["operation_id"], "co-01ARZ3NDEKTSV4RRFFQ69G5FB4");
    assert_eq!(error["error"]["code"], "not_found");
}

#[test]
fn status_does_not_create_a_run_and_partial_agent_identity_never_becomes_operator()
 {
    let fixture = TestEnvironment::new();
    let mut status = fixture.command();
    status.args(["status", "--json"]);

    let output = run(status);

    assert_eq!(output.status.code(), Some(4));
    assert!(output.stdout.is_empty());
    let error: Value = serde_json::from_slice(&output.stderr)
        .expect("the missing-run diagnostic should be JSON");
    assert_eq!(error["error"]["code"], "not_found");
    assert_eq!(fixture.index_entry_count(), 0);

    fixture.run_json(&["--json"]);
    let mut inbox = fixture.command();
    inbox.args(["inbox", "--json"]);
    let output = run(inbox);
    assert_eq!(output.status.code(), Some(6));
    let error: Value = serde_json::from_slice(&output.stderr)
        .expect("the operator inbox rejection should be JSON");
    assert_eq!(error["error"]["code"], "permission_denied");

    let mut finish = fixture.command();
    finish.args([
        "finish",
        "--status",
        "completed",
        "--summary",
        "No operator assignment.",
        "--json",
    ]);
    let output = run(finish);
    assert_eq!(output.status.code(), Some(6));
    let error: Value = serde_json::from_slice(&output.stderr)
        .expect("the operator finish rejection should be JSON");
    assert_eq!(error["error"]["code"], "permission_denied");
    assert!(error["operation_id"].as_str().is_some());

    let mut whoami = fixture.command();
    whoami
        .args(["whoami", "--json"])
        .env("COTERIE_AGENT_ID", "cg-01ARZ3NDEKTSV4RRFFQ69G5FAX");
    let output = run(whoami);
    assert_eq!(output.status.code(), Some(6));
    assert!(output.stdout.is_empty());
    let error: Value = serde_json::from_slice(&output.stderr)
        .expect("the authentication diagnostic should be JSON");
    assert_eq!(error["error"]["code"], "unauthenticated");

    fixture.run_json(&["stop", "--json"]);
}

#[test]
fn human_success_and_diagnostics_use_separate_streams() {
    let fixture = TestEnvironment::new();
    let launch = run(fixture.command());
    assert!(launch.status.success(), "launch failed: {launch:?}");
    assert!(!launch.stdout.is_empty());
    assert!(launch.stderr.is_empty());
    let data: Value = serde_json::from_slice(&launch.stdout)
        .expect("human output should be readable structured text");
    assert_eq!(data["agent"]["name"], "lead");
    assert!(data.get("schema_version").is_none());

    let mut invalid = fixture.command();
    invalid.args(["task", "close", "not-a-task"]);
    let invalid = run(invalid);
    assert_eq!(invalid.status.code(), Some(2));
    assert!(invalid.stdout.is_empty());
    assert!(!invalid.stderr.is_empty());

    fixture.run_json(&["stop", "--json"]);
}

#[test]
fn operator_commands_drive_the_minimum_delegation_flow() {
    let fixture = TestEnvironment::new();

    let launch = fixture.run_json(&[
        "--operation-id",
        "co-01ARZ3NDEKTSV4RRFFQ69G5FB0",
        "--json",
    ]);
    let run_id = launch["data"]["run_id"]
        .as_str()
        .expect("launch should identify the run")
        .to_owned();
    assert_eq!(launch["data"]["agent"]["name"], "lead");
    assert_eq!(launch["data"]["agent"]["state"], "running");
    assert_eq!(launch["operation_id"], "co-01ARZ3NDEKTSV4RRFFQ69G5FB0");
    let reconnect = fixture.run_json(&[
        "--operation-id",
        "co-01ARZ3NDEKTSV4RRFFQ69G5FB0",
        "--json",
    ]);
    assert_eq!(reconnect["data"]["run_id"], run_id);
    assert_eq!(reconnect["data"]["agent"], launch["data"]["agent"]);
    assert_eq!(
        reconnect["data"]["session_id"],
        launch["data"]["session_id"]
    );

    let identity = fixture.run_json(&["whoami", "--json"]);
    assert_eq!(identity["data"]["run_id"], run_id);
    assert_eq!(identity["data"]["channel"], "operator");
    assert!(identity["data"]["agent"].is_null());

    let task = fixture.run_json(&[
        "task",
        "create",
        "Implement parser",
        "--description",
        "Add parsing tests first.",
        "--operation-id",
        "co-01ARZ3NDEKTSV4RRFFQ69G5FB1",
        "--json",
    ]);
    let task_id = task["data"]["task"]["id"]
        .as_str()
        .expect("task creation should return an ID")
        .to_owned();
    assert!(task["operation_id"].as_str().is_some());
    let retried_task = fixture.run_json(&[
        "task",
        "create",
        "Implement parser",
        "--description",
        "Add parsing tests first.",
        "--operation-id",
        "co-01ARZ3NDEKTSV4RRFFQ69G5FB1",
        "--json",
    ]);
    assert_eq!(retried_task, task);

    let ready = fixture.run_json(&["task", "ready", "--json"]);
    assert_eq!(ready["data"]["tasks"][0]["id"], task_id);
    assert_eq!(ready["data"]["tasks"][0]["project"], "primary");

    let downstream = fixture.run_json(&[
        "task",
        "create",
        "Document parser",
        "--after",
        &task_id,
        "--json",
    ]);
    let downstream_id = downstream["data"]["task"]["id"]
        .as_str()
        .expect("downstream task creation should return an ID");
    let ready = fixture.run_json(&["task", "ready", "--json"]);
    assert_eq!(ready["data"]["tasks"].as_array().map(Vec::len), Some(1));
    assert_eq!(ready["data"]["tasks"][0]["id"], task_id);
    assert_ne!(ready["data"]["tasks"][0]["id"], downstream_id);

    let mut premature_close = fixture.command();
    premature_close.args([
        "task",
        "close",
        &task_id,
        "--summary",
        "Not submitted yet.",
        "--operation-id",
        "co-01ARZ3NDEKTSV4RRFFQ69G5FB5",
        "--json",
    ]);
    let output = run(premature_close);
    assert_eq!(output.status.code(), Some(5));
    assert!(output.stdout.is_empty());
    let error: Value = serde_json::from_slice(&output.stderr)
        .expect("the lifecycle rejection should be JSON");
    assert_eq!(error["operation_id"], "co-01ARZ3NDEKTSV4RRFFQ69G5FB5");
    assert_eq!(error["error"]["code"], "conflict");

    let spawn = fixture.run_json(&[
        "spawn",
        "worker",
        "--task",
        &task_id,
        "--operation-id",
        "co-01ARZ3NDEKTSV4RRFFQ69G5FB2",
        "--json",
    ]);
    assert_eq!(spawn["data"]["agent"]["name"], "worker-1");
    assert_eq!(spawn["data"]["agent"]["state"], "running");
    assert_eq!(spawn["data"]["task_id"], task_id);
    assert!(spawn["data"]["assignment_id"].as_str().is_some());
    let retried_spawn = fixture.run_json(&[
        "spawn",
        "worker",
        "--task",
        &task_id,
        "--operation-id",
        "co-01ARZ3NDEKTSV4RRFFQ69G5FB2",
        "--json",
    ]);
    assert_eq!(retried_spawn, spawn);

    let status = fixture.run_json(&["status", "--json"]);
    assert_eq!(status["data"]["run_id"], run_id);
    assert_eq!(status["data"]["status"], "active");
    assert_eq!(status["data"]["agents"].as_array().map(Vec::len), Some(2));
    assert_eq!(status["data"]["tasks"]["in_progress"], 1);

    let prime = fixture.run_json(&["prime", "--json"]);
    assert_eq!(prime["data"]["identity"]["channel"], "operator");
    assert_eq!(prime["data"]["projects"][0]["alias"], "primary");
    assert_eq!(prime["data"]["peers"].as_array().map(Vec::len), Some(2));
    let tasks = prime["data"]["tasks"]
        .as_array()
        .expect("prime should include durable tasks");
    assert_eq!(tasks.len(), 2);
    let downstream = tasks
        .iter()
        .find(|task| task["id"] == downstream_id)
        .expect("prime should include the blocked downstream task");
    assert_eq!(downstream["ready"], false);
    assert_eq!(downstream["unresolved_dependencies"][0], task_id);

    let sent = fixture.run_json(&[
        "send",
        "worker-1",
        "Check the parser edge cases.",
        "--operation-id",
        "co-01ARZ3NDEKTSV4RRFFQ69G5FB3",
        "--json",
    ]);
    assert_eq!(sent["data"]["recipient"]["name"], "worker-1");
    assert_eq!(sent["data"]["sequence"], 1);
    let retried_send = fixture.run_json(&[
        "send",
        "worker-1",
        "Check the parser edge cases.",
        "--operation-id",
        "co-01ARZ3NDEKTSV4RRFFQ69G5FB3",
        "--json",
    ]);
    assert_eq!(retried_send, sent);

    let logs = fixture.run_json(&["logs", "worker-1", "--json"]);
    assert_eq!(logs["data"]["agent"]["name"], "worker-1");
    assert!(
        logs["data"]["transcript"]
            .as_str()
            .is_some_and(|transcript| transcript.contains("session.ready"))
    );

    let events = fixture.run_json(&["events", "--json"]);
    assert!(events["data"]["events"].is_array());
    assert!(events["data"]["next_cursor"].is_number());

    let stopped = fixture.run_json(&["stop", "--json"]);
    assert_eq!(stopped["data"]["run_id"], run_id);
    assert_eq!(stopped["data"]["status"], "stopped");
}

#[test]
fn private_supervisor_entrypoint_publishes_a_reachable_run() {
    let fixture = TestEnvironment::new();
    let log_path = fixture.root.join("supervisor.log");
    let log = fs::File::create(&log_path).expect("the log should be created");
    let mut command = fixture.command();
    command
        .arg("__supervisor")
        .arg(RUN_ID)
        .arg(PROJECT_ID)
        .arg(&fixture.project)
        .stderr(log);
    let mut child = command.spawn().expect("the supervisor should start");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if fixture.index_entry_count() == 1 {
            break;
        }
        if let Some(status) =
            child.try_wait().expect("status should be readable")
        {
            panic!(
                "supervisor exited with {status}: {}",
                fs::read_to_string(&log_path).unwrap_or_default()
            );
        }
        assert!(Instant::now() < deadline, "supervisor startup timed out");
        thread::sleep(Duration::from_millis(20));
    }

    let foreground = run(fixture.connect_command());
    assert!(
        foreground.status.success(),
        "the indexed supervisor should be reachable: {foreground:?}"
    );
    let mut shutdown = fixture.command();
    shutdown.arg("__supervisor-shutdown");
    let shutdown = run(shutdown);
    assert!(
        shutdown.status.success(),
        "restart shutdown failed: {shutdown:?}"
    );
    assert!(
        child.wait().expect("the supervisor should exit").success(),
        "supervisor failed: {}",
        fs::read_to_string(log_path).unwrap_or_default()
    );
}

#[test]
fn concurrent_launches_share_one_supervisor_and_clean_shutdown_retires_it() {
    let fixture = TestEnvironment::new();
    let first = fixture.connect_command();
    let second = fixture.connect_command();
    let first = thread::spawn(move || run(first));
    let second = thread::spawn(move || run(second));

    let first = first.join().expect("the first launch should not panic");
    let second = second.join().expect("the second launch should not panic");
    assert!(
        first.status.success() && second.status.success(),
        "concurrent launches failed:\nfirst: {first:?}\nsecond: {second:?}\nfiles: {:?}",
        fixture.files()
    );

    let index_path = fixture.only_index_entry();
    assert_eq!(mode(&index_path), 0o600);
    let index: Value = serde_json::from_slice(
        &fs::read(&index_path).expect("the index should be readable"),
    )
    .expect("the index should contain JSON");
    let run_id = index["run_id"]
        .as_str()
        .expect("the index should identify its run");
    let socket = fixture
        .runtime
        .join("coterie")
        .join(format!("{run_id}.sock"));
    let database = fixture
        .state
        .join("coterie/runs")
        .join(run_id)
        .join("state.sqlite3");
    assert!(socket.exists(), "the supervisor socket should be live");
    assert!(
        database.is_file(),
        "the supervisor should own a durable run database"
    );
    assert_eq!(
        fs::read_dir(fixture.state.join("coterie/runs"))
            .expect("the run directory should be readable")
            .count(),
        1,
        "losing startup contenders must not leave run state"
    );

    let mut shutdown_command = fixture.command();
    shutdown_command.arg("__supervisor-shutdown");
    let shutdown = run(shutdown_command);
    assert!(shutdown.status.success(), "shutdown failed: {shutdown:?}");
    wait_until("supervisor retirement", || {
        !index_path.exists() && !socket.exists()
    });
    let connection = rusqlite::Connection::open(database)
        .expect("the stopped run database should open");
    let (status, stopped_at): (String, Option<i64>) = connection
        .query_row(
            "SELECT status, stopped_at FROM runs WHERE id = ?1",
            [run_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("the durable run should be readable");
    assert_eq!(status, "stopped");
    assert!(stopped_at.is_some());
}

#[test]
fn stale_index_and_socket_restart_the_same_durable_run() {
    let fixture = TestEnvironment::new();
    let log_path = fixture.root.join("crashed-supervisor.log");
    let log = fs::File::create(&log_path).expect("the log should be created");
    let mut command = fixture.command();
    command
        .arg("__supervisor")
        .arg(RUN_ID)
        .arg(PROJECT_ID)
        .arg(&fixture.project)
        .stdout(std::process::Stdio::null())
        .stderr(log);
    let mut crashed = command.spawn().expect("the supervisor should start");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if fixture.index_entry_count() == 1 {
            break;
        }
        if let Some(status) = crashed
            .try_wait()
            .expect("the crashed fixture status should be readable")
        {
            panic!(
                "supervisor exited before publication with {status}: {}",
                fs::read_to_string(&log_path).unwrap_or_default()
            );
        }
        assert!(
            Instant::now() < deadline,
            "supervisor publication timed out"
        );
        thread::sleep(Duration::from_millis(20));
    }
    let stale_index = fixture.only_index_entry();
    let stale_contents =
        fs::read(&stale_index).expect("the index should exist");

    crashed
        .kill()
        .expect("the owned fixture process should stop");
    crashed.wait().expect("the killed process should be reaped");

    let restart = run(fixture.connect_command());
    assert!(restart.status.success(), "restart failed: {restart:?}");
    assert_eq!(
        fs::read(&stale_index).expect("the index should be republished"),
        stale_contents,
        "recovery should preserve the indexed run and project IDs"
    );

    let mut shutdown = fixture.command();
    shutdown.arg("__supervisor-shutdown");
    assert!(run(shutdown).status.success());
    wait_until("restarted supervisor retirement", || !stale_index.exists());
}

fn run(mut command: Command) -> std::process::Output {
    command.output().expect("Coterie should execute")
}

fn wait_until(description: &str, predicate: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !predicate() {
        assert!(Instant::now() < deadline, "{description} timed out");
        thread::sleep(Duration::from_millis(20));
    }
}

fn mode(path: &Path) -> u32 {
    fs::metadata(path)
        .expect("metadata should be available")
        .permissions()
        .mode()
        & 0o777
}

struct TestEnvironment {
    root: PathBuf,
    runtime: PathBuf,
    state: PathBuf,
    project: PathBuf,
}

impl TestEnvironment {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "ct-{}-{}",
            std::process::id(),
            ulid::Ulid::generate()
        ));
        let runtime = root.join("runtime");
        let state = root.join("state");
        let project = root.join("project");
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&runtime)
            .expect("the runtime directory should be created");
        fs::create_dir_all(&project)
            .expect("the project directory should be created");
        Self {
            root,
            runtime,
            state,
            project,
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_coterie"));
        command
            .current_dir(&self.project)
            .env("XDG_RUNTIME_DIR", &self.runtime)
            .env("XDG_STATE_HOME", &self.state);
        command
    }

    fn connect_command(&self) -> Command {
        let mut command = self.command();
        command.arg("__supervisor-connect");
        command
    }

    fn run_json(&self, arguments: &[&str]) -> Value {
        let mut command = self.command();
        command.args(arguments);
        let output = run(command);
        assert!(
            output.status.success(),
            "command {arguments:?} failed: {output:?}"
        );
        assert!(
            output.stderr.is_empty(),
            "successful JSON should not write diagnostics: {output:?}"
        );
        serde_json::from_slice(&output.stdout)
            .expect("the command should return one JSON response")
    }

    fn only_index_entry(&self) -> PathBuf {
        let entries = fs::read_dir(self.state.join("coterie/projects"))
            .expect("the project index should exist")
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.extension().is_some_and(|value| value == "json")
            })
            .collect::<Vec<_>>();
        assert_eq!(entries.len(), 1, "exactly one project should be indexed");
        entries[0].clone()
    }

    fn index_entry_count(&self) -> usize {
        fs::read_dir(self.state.join("coterie/projects"))
            .map(|entries| {
                entries
                    .filter_map(Result::ok)
                    .map(|entry| entry.path())
                    .filter(|path| {
                        path.extension().is_some_and(|value| value == "json")
                    })
                    .count()
            })
            .unwrap_or(0)
    }

    fn files(&self) -> Vec<PathBuf> {
        let mut pending = vec![self.root.clone()];
        let mut files = Vec::new();
        while let Some(path) = pending.pop() {
            files.push(path.clone());
            if let Ok(entries) = fs::read_dir(path) {
                pending.extend(
                    entries.filter_map(Result::ok).map(|entry| entry.path()),
                );
            }
        }
        files.sort();
        files
    }
}

impl Drop for TestEnvironment {
    fn drop(&mut self) {
        if self.root.exists() {
            fs::remove_dir_all(&self.root)
                .expect("the test environment should be removable");
        }
    }
}

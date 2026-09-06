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

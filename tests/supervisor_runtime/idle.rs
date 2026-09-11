use super::*;

#[test]
fn idle_shutdown_retires_the_process_and_all_projects_and_preserves_tasks() {
    let fixture = TestEnvironment::new();
    write_global(&fixture, "[supervision]\nidle_timeout_seconds = 1");
    let mut supervisor = fixture
        .command()
        .args([
            "__supervisor",
            RUN_ID,
            PROJECT_ID,
            fixture.project.to_str().unwrap(),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    wait_until("the supervisor socket", || {
        fixture
            .runtime
            .join(format!("coterie/{RUN_ID}.sock"))
            .exists()
    });
    let library = fixture.root.join("library");
    fs::create_dir(&library).unwrap();
    fixture.run_json(&[
        "project",
        "attach",
        library.to_str().unwrap(),
        "--alias",
        "library",
        "--json",
    ]);
    let task = fixture.run_json(&[
        "task",
        "create",
        "Retain unfinished work",
        "--json",
    ]);
    let database = fixture
        .state
        .join(format!("coterie/runs/{RUN_ID}/state.sqlite3"));
    let mut inspections = 0;
    wait_until("idle supervisor exit despite read-only polling", || {
        if supervisor.try_wait().unwrap().is_some() {
            return true;
        }
        if fixture
            .command()
            .args(["status", "--json"])
            .output()
            .unwrap()
            .status
            .success()
        {
            inspections += 1;
        }
        false
    });
    assert!(inspections > 1);
    assert!(supervisor.wait().unwrap().success());
    assert_eq!(fixture.index_entry_count(), 0);
    assert!(
        !fixture
            .runtime
            .join(format!("coterie/{RUN_ID}.sock"))
            .exists()
    );
    let connection = rusqlite::Connection::open_with_flags(
        &database,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    assert_eq!(
        connection
            .query_row("SELECT status FROM runs", [], |row| row
                .get::<_, String>(0))
            .unwrap(),
        "stopped"
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT status FROM tasks WHERE id = ?1",
                [task["data"]["task"]["id"].as_str().unwrap()],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
        "open"
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT count(*) FROM events WHERE event_type = 'run.stopped'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    fixture.launch(&[]);
    assert_ne!(
        fixture.run_json(&["status", "--json"])["data"]["run_id"],
        RUN_ID
    );
    fixture.run_json(&["stop", "--json"]);
}

#[test]
fn live_foreground_and_worker_prevent_idle_shutdown_and_dirty_work_is_retained()
{
    let fixture = TestEnvironment::new();
    write_global(&fixture, "[supervision]\nidle_timeout_seconds = 1");
    let script = FAKE_CODEX.replace(
        "if [ \"$is_job\" = true ]; then",
        r#"if [ "$is_job" = true ]; then
  printf '{"type":"thread.started","thread_id":"idle-worker"}\n'
  while [ ! -e "$COTERIE_SOCKET.release" ]; do sleep 0.02; done
  printf '{"type":"turn.completed","usage":{}}\n'
  exit 0
fi
if [ "$is_job" = true ]; then"#,
    );
    fs::write(fixture.root.join("bin/codex"), script).unwrap();
    let capture = fixture.root.join("lead");
    let mut foreground = fixture
        .command()
        .env("COTERIE_FAKE_MODE", "contract")
        .env("COTERIE_FAKE_CAPTURE", &capture)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    wait_until("foreground readiness", || capture.exists());
    thread::sleep(Duration::from_millis(1200));
    assert!(foreground.try_wait().unwrap().is_none());
    let status = fixture.run_json(&["status", "--json"]);
    let run_id = status["data"]["run_id"].as_str().unwrap();
    let task = fixture.run_json(&[
        "task",
        "create",
        "Unfinished implementation",
        "--json",
    ]);
    fixture.run_json(&[
        "spawn",
        "worker",
        "--task",
        task["data"]["task"]["id"].as_str().unwrap(),
        "--json",
    ]);
    let database = fixture
        .state
        .join(format!("coterie/runs/{run_id}/state.sqlite3"));
    let connection = rusqlite::Connection::open_with_flags(
        &database,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let workspace: Vec<u8> = connection
        .query_row("SELECT path FROM workspaces", [], |row| row.get(0))
        .unwrap();
    let workspace = Path::new(std::ffi::OsStr::from_bytes(&workspace));
    fs::write(workspace.join("unfinished.txt"), "keep this work\n").unwrap();
    foreground
        .stdin
        .take()
        .unwrap()
        .write_all(b"exit\n")
        .unwrap();
    assert!(foreground.wait_with_output().unwrap().status.success());
    thread::sleep(Duration::from_millis(1200));
    let status = fixture.run_json(&["status", "--json"]);
    assert_eq!(status["data"]["status"], "active");
    assert!(
        status["data"]["agents"]
            .as_array()
            .unwrap()
            .iter()
            .any(|agent| agent["name"] == "worker-1"
                && agent["state"] == "running")
    );
    let socket = fixture.runtime.join(format!("coterie/{run_id}.sock"));
    fs::write(socket.with_extension("sock.release"), "").unwrap();
    wait_until("idle shutdown after the worker exits", || !socket.exists());
    wait_until("project retirement", || fixture.index_entry_count() == 0);
    assert_eq!(
        fs::read_to_string(workspace.join("unfinished.txt")).unwrap(),
        "keep this work\n"
    );
    assert_eq!(
        connection
            .query_row("SELECT status FROM tasks", [], |row| row
                .get::<_, String>(0))
            .unwrap(),
        "in_progress"
    );
    assert_eq!(
        connection
            .query_row("SELECT state FROM assignments", [], |row| row
                .get::<_, String>(0))
            .unwrap(),
        "draining"
    );
    let transcript: Vec<u8> = connection.query_row("SELECT transcript_path FROM sessions WHERE process_owner = 'supervisor'", [], |row| row.get(0)).unwrap();
    let transcript = Path::new(std::ffi::OsStr::from_bytes(&transcript));
    assert!(
        fs::metadata(database.parent().unwrap().join(transcript))
            .unwrap()
            .len()
            > 0
    );
}

#[test]
fn disabled_idle_shutdown_remains_available_until_explicit_stop() {
    let fixture = TestEnvironment::new();
    write_global(&fixture, "[supervision]\nidle_timeout_seconds = 0");
    fixture.launch(&[]);
    let status = fixture.run_json(&["status", "--json"]);
    thread::sleep(Duration::from_millis(1200));
    assert_eq!(
        fixture.run_json(&["status", "--json"])["data"]["run_id"],
        status["data"]["run_id"]
    );
    fixture.run_json(&["stop", "--json"]);
}

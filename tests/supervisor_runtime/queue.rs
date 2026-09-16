use super::*;
use nix::pty::{Winsize, openpty};
use nix::unistd::ttyname;
use std::io::Read;
use std::sync::{Arc, Mutex};

const WORKER_MESSAGE: &str = "COTERIE_QUEUE_WORKER_SUBMITTED";
const WITNESS_TITLE: &str = "Coterie queue notification handled";
const THREAD_ID: &str = "01234567-89ab-cdef-0123-456789abcdef";

#[test]
fn fake_queue_foreground() {
    let Ok(ready) = std::env::var("COTERIE_TEST_QUEUE_READY") else {
        return;
    };
    let mut bridge = McpClient::start(Command::new(
        std::env::var_os("COTERIE_BIN").unwrap(),
    ));
    bridge.initialize();
    let prime = bridge.request(
        "tools/call",
        json!({"name":"prime","arguments":{},"_meta":{"threadId":THREAD_ID}}),
    );
    assert_eq!(
        prime["result"]["structuredContent"]["data"]["notifications"],
        "automatic",
        "{prime}"
    );
    assert_eq!(
        prime["result"]["structuredContent"]["data"]["session"]["session_id"],
        std::env::var("COTERIE_SESSION_ID").unwrap()
    );
    let progress = bridge.call(
        "progress",
        json!({"after":null, "limit":50, "wait_seconds":0}),
    );
    fs::write(&ready, serde_json::to_vec(&prime).unwrap()).unwrap();
    if std::env::var_os("COTERIE_TEST_QUEUE_BUSY").is_some() {
        bridge.call("progress", json!({"after":progress["result"]["structuredContent"]["data"]["next_cursor"],"limit":50,"wait_seconds":5}));
        fs::write(format!("{ready}.idle"), "").unwrap();
    }
    let mut input = String::new();
    while std::io::stdin().read_line(&mut input).unwrap() != 0 {
        if input.trim() == "prime" {
            let prime = bridge.call("prime", json!({}));
            fs::write(
                format!("{ready}.reconnected"),
                serde_json::to_vec(&prime).unwrap(),
            )
            .unwrap();
        } else {
            break;
        }
        input.clear();
    }
}

struct QueueForeground<'a> {
    child: Child,
    fixture: &'a TestEnvironment,
}

impl std::ops::Deref for QueueForeground<'_> {
    type Target = Child;
    fn deref(&self) -> &Child {
        &self.child
    }
}

impl std::ops::DerefMut for QueueForeground<'_> {
    fn deref_mut(&mut self) -> &mut Child {
        &mut self.child
    }
}

impl Drop for QueueForeground<'_> {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.fixture.command().args(["stop", "--json"]).output();
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn queue_fixture(fixture: &TestEnvironment) -> (QueueForeground<'_>, PathBuf) {
    queue_fixture_with(fixture, false)
}

fn queue_fixture_with(
    fixture: &TestEnvironment,
    busy: bool,
) -> (QueueForeground<'_>, PathBuf) {
    let capture = fixture.root.join("queue-arguments");
    let ready = fixture.root.join("queue-ready");
    let original = FAKE_CODEX.replace(
        "Usage: codex [OPTIONS] [PROMPT]\\n",
        "Usage: codex [OPTIONS] [PROMPT]\\n  queue Enqueue a message\\n",
    );
    let script = original.replacen("#!/bin/sh\n", r#"#!/bin/sh
if [ "$1" = "queue" ]; then
  if [ "$2" = "--help" ]; then printf '%s\n' '--thread <ID> --message <TEXT>'; exit 0; fi
  printf '%s\0' "$@" >> "$COTERIE_TEST_QUEUE_CAPTURE"
  exit 0
fi
"#, 1).replace("is_job=false", r#"if [ -z "${COTERIE_TASK_ID-}" ]; then
  exec "$COTERIE_TEST_EXECUTABLE" --exact mcp::queue::fake_queue_foreground --nocapture
fi
is_job=false"#);
    fs::write(fixture.root.join("bin/codex"), script).unwrap();
    let mut command = fixture.command();
    if busy {
        command.env("COTERIE_TEST_QUEUE_BUSY", "1");
    }
    let child = command
        .env("COTERIE_TEST_EXECUTABLE", std::env::current_exe().unwrap())
        .env("COTERIE_TEST_QUEUE_CAPTURE", &capture)
        .env("COTERIE_TEST_QUEUE_READY", &ready)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let foreground = QueueForeground { child, fixture };
    wait_until("the bound foreground notification bridge", || {
        ready.exists()
    });
    (foreground, capture)
}

#[test]
fn automatic_queue_delivers_while_the_bridge_is_busy() {
    let fixture = TestEnvironment::new();
    let (mut foreground, capture) = queue_fixture_with(&fixture, true);
    fixture.run_json(&[
        "send",
        "lead",
        "Notify while progress is waiting.",
        "--json",
    ]);
    wait_until("queue delivery during an active tool call", || {
        capture.exists()
    });
    assert!(!fixture.root.join("queue-ready.idle").exists());
    wait_until("the busy tool call completing", || {
        fixture.root.join("queue-ready.idle").exists()
    });
    fixture.run_json(&["stop", "--json"]);
    foreground.wait().unwrap();
}

#[test]
fn automatic_queue_reports_worker_completion_without_an_explicit_message() {
    let fixture = TestEnvironment::new();
    let (mut foreground, capture) = queue_fixture(&fixture);
    let script = fs::read_to_string(fixture.root.join("bin/codex")).unwrap();
    let script = format!(
        "{}{}",
        script.split_once("is_job=false").unwrap().0,
        r#"
printf '{"type":"thread.started","thread_id":"completed-worker"}\n'
"$COTERIE_BIN" finish --status completed --summary 'Completed without an inbox message.' --json >/dev/null || exit 41
printf '{"type":"turn.completed","usage":{}}\n'
"#
    );
    fs::write(fixture.root.join("bin/codex"), script).unwrap();
    let task =
        fixture.run_json(&["task", "create", "Notify completion", "--json"]);
    fixture.run_json(&[
        "spawn",
        "worker",
        "--task",
        task["data"]["task"]["id"].as_str().unwrap(),
        "--json",
    ]);
    let status = fixture.run_json(&["status", "--json"]);
    let database = database(&fixture, &status);
    wait_until(
        "a queued worker submission without an inbox message",
        || {
            database.query_row("SELECT EXISTS(SELECT 1 FROM events AS e JOIN notification_deliveries AS d ON d.event_cursor >= e.sequence WHERE e.event_type = 'task.lifecycle_changed' AND json_extract(e.payload_json, '$.data.status') = 'submitted' AND d.state = 'accepted')", [], |row| row.get::<_, bool>(0)).unwrap()
        },
    );
    assert!(capture.exists());
    assert_eq!(
        database
            .query_row("SELECT COUNT(*) FROM messages", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    fixture.run_json(&["stop", "--json"]);
    foreground.wait().unwrap();
}

#[test]
fn automatic_queue_reconnects_without_replaying_accepted_deliveries() {
    let fixture = TestEnvironment::new();
    let mut supervisor = fixture
        .command()
        .args(["__supervisor", RUN_ID, PROJECT_ID])
        .arg(&fixture.project)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_until("supervisor publication", || {
        fixture.index_entry_count() == 1
    });
    let (mut foreground, capture) = queue_fixture(&fixture);
    fixture.run_json(&["send", "lead", "Before restart.", "--json"]);
    let status = fixture.run_json(&["status", "--json"]);
    let database = database(&fixture, &status);
    wait_until("the first accepted delivery", || {
        database.query_row("SELECT COUNT(*) FROM notification_deliveries WHERE state = 'accepted'", [], |r| r.get::<_, i64>(0)).unwrap() == 1
    });
    let before = fs::read(&capture).unwrap();
    supervisor.kill().unwrap();
    supervisor.wait().unwrap();
    wait_until("foreground owner restoring notification delivery", || {
        let status = fixture
            .command()
            .args(["status", "--json"])
            .output()
            .unwrap();
        status.status.success()
            && serde_json::from_slice::<Value>(&status.stdout).unwrap()["data"]
                ["agents"][0]["state"]
                == "running"
    });
    foreground
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"prime\n")
        .unwrap();
    let reconnected = fixture.root.join("queue-ready.reconnected");
    wait_until("the bridge reauthenticating after reconnect", || {
        reconnected.exists()
    });
    let prime: Value =
        serde_json::from_slice(&fs::read(reconnected).unwrap()).unwrap();
    assert_eq!(
        prime["result"]["structuredContent"]["data"]["notifications"],
        "automatic",
        "{prime}"
    );
    assert_eq!(fs::read(&capture).unwrap(), before);
    fixture.run_json(&["send", "lead", "After restart.", "--json"]);
    wait_until("delivery after supervisor recovery", || {
        database.query_row("SELECT COUNT(*) FROM notification_deliveries WHERE state = 'accepted'", [], |r| r.get::<_, i64>(0)).unwrap() == 2
    });
    fixture.run_json(&["stop", "--json"]);
    foreground.wait().unwrap();
}

#[test]
fn automatic_queue_coalesces_messages_without_promoting_their_contents() {
    let fixture = TestEnvironment::new();
    let (mut foreground, capture) = queue_fixture(&fixture);
    for message in ["WORKER_SECRET_A $(false)", "WORKER_SECRET_B"] {
        fixture.run_json(&["send", "lead", message, "--json"]);
    }
    wait_until("automatic queue delivery", || capture.exists());
    let arguments = fs::read(&capture).unwrap();
    let args: Vec<_> = arguments
        .split(|b| *b == 0)
        .filter(|bytes| !bytes.is_empty())
        .map(|bytes| std::str::from_utf8(bytes).unwrap())
        .collect();
    assert_eq!(&args[..4], &["queue", "--thread", THREAD_ID, "--message"]);
    assert_eq!(args.len(), 5);
    assert!(!args[4].contains("WORKER_SECRET"));
    assert!(args[4].contains("pauses"));
    assert!(args[4].contains("inbox"));
    let status = fixture.run_json(&["status", "--json"]);
    let database = database(&fixture, &status);
    assert_eq!(
        database
            .query_row(
                "SELECT COUNT(*) FROM messages WHERE acknowledged_at IS NULL",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        2
    );
    wait_until("durable delivery observation", || {
        database.query_row("SELECT COUNT(*) FROM notification_deliveries WHERE state = 'accepted'", [], |r| r.get::<_, i64>(0)).unwrap() == 1
    });
    thread::sleep(Duration::from_millis(1200));
    assert_eq!(fs::read(&capture).unwrap(), arguments);
    fixture.run_json(&["stop", "--json"]);
    foreground.wait().unwrap();
    assert_eq!(fs::read(&capture).unwrap(), arguments);
}

fn database(fixture: &TestEnvironment, status: &Value) -> rusqlite::Connection {
    rusqlite::Connection::open_with_flags(
        fixture
            .state
            .join("coterie/runs")
            .join(status["data"]["run_id"].as_str().unwrap())
            .join("state.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap()
}

#[test]
#[ignore = "requires explicit opt-in, Codex authentication, and model access; queues a notification to an idle real Coterie foreground TUI"]
fn installed_codex_queue_wakes_idle_foreground_for_worker_message() {
    let fixture = TestEnvironment::new();
    let home = isolated_authentication(&fixture);
    fs::write(
        home.join("config.toml"),
        format!(
            "check_for_update_on_startup = false\nweb_search = \"disabled\"\n[projects.{}]\ntrust_level = \"trusted\"\n",
            serde_json::to_string(&fixture.project).unwrap()
        ),
    )
    .unwrap();
    let prompt = format!(
        "This is a bounded Coterie notification conformance test. Call Coterie's prime tool, then reply QUEUE_READY and end your turn. Do not poll, wait in a tool, spawn workers, or run shell commands. An authorized test operator will later spawn a worker and send a separate wake-up notification. Only after that notification, call inbox and inspect prime. If your inbox contains {WORKER_MESSAGE} and the task named Queue test worker is submitted, use new_operation_id and task_create to create one task titled {WITNESS_TITLE}, description=Observed a durable worker submission after a queued notification., project=primary, group=null, dependencies=[]. Acknowledge the handled inbox message with a fresh operation ID, then reply QUEUE_HANDLED and end your turn. Do not integrate or close the worker task; the test ends after notification handling. Perform no other work."
    );
    let global = include_str!("../../examples/config/global.toml")
        .replace(
            "roles.coordinator]\nprovider = \"codex\"",
            "roles.coordinator]\nprovider = \"real_codex\"",
        )
        .replace(
            "permission_profile = \"interactive\"",
            "permission_profile = \"inspect\"",
        );
    write_global(
        &fixture,
        &format!(
            "{global}\n[providers.real_codex]\ncommand = [{}, \"--no-alt-screen\"]\n",
            serde_json::to_string(&installed_codex()).unwrap(),
        ),
    );
    let worker = format!(
        r#"
if [ -z "${{COTERIE_TASK_ID-}}" ]; then exit 31; fi
printf '{{"type":"thread.started","thread_id":"queue-test-worker"}}\n'
"$COTERIE_BIN" finish --status completed --summary 'Queue test submission.' --json >/dev/null || exit 32
"$COTERIE_BIN" send coordinator '{WORKER_MESSAGE}' --json >/dev/null || exit 33
printf '{{"type":"turn.completed","usage":{{}}}}\n'
"#
    );
    fs::write(
        fixture.root.join("bin/codex"),
        format!(
            "{}{}",
            FAKE_CODEX.split_once("is_job=false").unwrap().0,
            worker
        ),
    )
    .unwrap();

    let version = provider_command(&fixture, &home)
        .arg("--version")
        .output()
        .unwrap();
    assert!(version.status.success());
    let _daemon = StopDaemon {
        fixture: &fixture,
        home: &home,
    };
    let mut foreground = Foreground::start(&fixture, &home);
    foreground
        .wait_for("foreground startup", || fixture.index_entry_count() == 1);
    thread::sleep(Duration::from_secs(2));
    foreground
        ._terminal
        .write_all(format!("\x1b[200~{prompt}\x1b[201~").as_bytes())
        .unwrap();
    thread::sleep(Duration::from_millis(100));
    foreground._terminal.write_all(b"\r").unwrap();
    let mut command = provider_command(&fixture, &home);
    // The observer reads persisted history without resuming or acquiring the
    // thread owned by the ordinary foreground TUI.
    command.arg("app-server");
    let mut app = McpClient::start_raw(command);
    let initialized = app.request(
        "initialize",
        json!({"clientInfo":{"name":"coterie-queue-conformance","version":"1"},"capabilities":{"experimentalApi":true}}),
    );
    assert!(initialized.get("result").is_some(), "{initialized}");
    app.send(json!({"method":"initialized"}));
    let mut thread_id = String::new();
    foreground.wait_for("the initial completed foreground turn", || {
        let listed = app
            .request("thread/list", json!({"cwd":fixture.project,"limit":10}));
        let threads = listed["result"]["data"].as_array().unwrap();
        if threads.is_empty() {
            return false;
        }
        // Discovery by directory is safe only in this isolated, single-thread
        // fixture. Production delivery needs a generation-bound provider ID.
        assert_eq!(threads.len(), 1, "ambiguous test thread identity");
        thread_id = threads[0]["id"].as_str().unwrap().to_owned();
        completed_turn(&mut app, &thread_id)
    });
    let initial_turn = last_turn_id(&mut app, &thread_id);
    let status = fixture.run_json(&["status", "--json"]);
    let agent_id = status["data"]["agents"][0]["id"].as_str().unwrap();
    let database = rusqlite::Connection::open_with_flags(
        fixture
            .state
            .join("coterie/runs")
            .join(status["data"]["run_id"].as_str().unwrap())
            .join("state.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let task =
        fixture.run_json(&["task", "create", "Queue test worker", "--json"]);
    let task_id = task["data"]["task"]["id"].as_str().unwrap();
    fixture.run_json(&["spawn", "builder", "--task", task_id, "--json"]);
    foreground.wait_for("worker message handling after queue delivery", || {
        message_count(&database, agent_id, true) == 1
            && witness_count(&database) == 1
            && completed_turn(&mut app, &thread_id)
    });
    assert_eq!(
        database
            .query_row(
                "SELECT thread_id FROM foreground_notifications",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
        thread_id
    );
    assert!(database.query_row("SELECT COUNT(*) FROM notification_deliveries WHERE state = 'accepted'", [], |r| r.get::<_, i64>(0)).unwrap() >= 1);
    assert_ne!(last_turn_id(&mut app, &thread_id), initial_turn);
    assert_eq!(
        fixture.run_json(&["status", "--json"])["data"]["agents"][0]["id"],
        agent_id
    );
    eprintln!(
        "{}: queued notification started a new turn in the same idle foreground thread; the agent observed a submitted task, handled its durable worker message, and acknowledged it.",
        String::from_utf8_lossy(&version.stdout).trim()
    );
}

fn completed_turn(app: &mut McpClient, thread_id: &str) -> bool {
    let read = app.request(
        "thread/read",
        json!({"threadId":thread_id,"includeTurns":true}),
    );
    let thread = &read["result"]["thread"];
    thread["turns"]
        .as_array()
        .and_then(|turns| turns.last())
        .is_some_and(|turn| turn["status"] == "completed")
}

fn last_turn_id(app: &mut McpClient, thread_id: &str) -> String {
    app.request(
        "thread/read",
        json!({"threadId":thread_id,"includeTurns":true}),
    )["result"]["thread"]["turns"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn message_count(
    database: &rusqlite::Connection,
    recipient: &str,
    acknowledged: bool,
) -> i64 {
    database
        .query_row(
            "SELECT count(*) FROM messages WHERE recipient_agent_id = ?1 AND body = ?2 AND (acknowledged_at IS NOT NULL) = ?3",
            rusqlite::params![recipient, WORKER_MESSAGE, acknowledged],
            |row| row.get(0),
        )
        .unwrap()
}

fn witness_count(database: &rusqlite::Connection) -> i64 {
    database
        .query_row(
            "SELECT count(*) FROM tasks WHERE title = ?1",
            [WITNESS_TITLE],
            |row| row.get(0),
        )
        .unwrap()
}

fn provider_command(fixture: &TestEnvironment, home: &Path) -> Command {
    let mut command = Command::new(installed_codex());
    for (name, value) in fixture.command().get_envs() {
        match value {
            Some(value) => command.env(name, value),
            None => command.env_remove(name),
        };
    }
    command
        .current_dir(&fixture.project)
        .env("CODEX_HOME", home)
        .env_remove("CODEX_THREAD_ID")
        .env_remove("OPENAI_API_KEY");
    command
}

struct StopDaemon<'a> {
    fixture: &'a TestEnvironment,
    home: &'a Path,
}

impl Drop for StopDaemon<'_> {
    fn drop(&mut self) {
        let _ = provider_command(self.fixture, self.home)
            .args(["app-server", "daemon", "stop"])
            .output();
    }
}

struct Foreground<'a> {
    fixture: &'a TestEnvironment,
    child: Child,
    _terminal: fs::File,
    captured: Arc<Mutex<Vec<u8>>>,
}

impl<'a> Foreground<'a> {
    fn start(fixture: &'a TestEnvironment, home: &Path) -> Self {
        let pty = openpty(
            Some(&Winsize {
                ws_row: 40,
                ws_col: 120,
                ws_xpixel: 0,
                ws_ypixel: 0,
            }),
            None,
        )
        .unwrap();
        let slave = ttyname(&pty.slave).unwrap();
        let mut terminal = fs::File::from(pty.master);
        let mut reader = terminal.try_clone().unwrap();
        let captured = Arc::new(Mutex::new(Vec::new()));
        let capture = captured.clone();
        thread::spawn(move || {
            let mut bytes = [0; 4096];
            while let Ok(count) = reader.read(&mut bytes) {
                if count == 0 {
                    break;
                }
                let mut capture = capture.lock().unwrap();
                capture.extend_from_slice(&bytes[..count]);
                let excess = capture.len().saturating_sub(32 * 1024);
                capture.drain(..excess);
            }
        });
        let mut command = Command::new(std::env::current_exe().unwrap());
        for (name, value) in provider_command(fixture, home).get_envs() {
            match value {
                Some(value) => command.env(name, value),
                None => command.env_remove(name),
            };
        }
        let child = command
            .args([
                "--exact",
                "terminal::foreground_terminal_child",
                "--nocapture",
            ])
            .current_dir(&fixture.project)
            .env("COTERIE_TEST_TERMINAL", slave)
            .env("TERM", "xterm-256color")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        thread::sleep(Duration::from_millis(500));
        terminal.write_all(b"\x1b[1;1R").unwrap();
        Self {
            fixture,
            child,
            _terminal: terminal,
            captured,
        }
    }

    fn wait_for(&mut self, description: &str, mut ready: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(180);
        while !ready() {
            if self.child.try_wait().unwrap().is_some()
                || Instant::now() >= deadline
            {
                panic!(
                    "{description} failed; diagnostic terminal capture: {}",
                    self.terminal_tail()
                );
            }
            thread::sleep(Duration::from_millis(250));
        }
    }

    fn terminal_tail(&self) -> String {
        let captured = self
            .captured
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        String::from_utf8_lossy(
            &captured[captured.len().saturating_sub(4096)..],
        )
        .into_owned()
    }
}

impl Drop for Foreground<'_> {
    fn drop(&mut self) {
        if thread::panicking() {
            eprintln!(
                "Foreground diagnostic capture: {}",
                self.terminal_tail()
            );
        }
        let _ = self.fixture.command().args(["stop", "--json"]).output();
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.child.try_wait().ok().flatten().is_none()
            && Instant::now() < deadline
        {
            thread::sleep(Duration::from_millis(50));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

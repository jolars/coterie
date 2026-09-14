use super::*;
use std::process::Child;

struct Supervisor(Child);

impl Supervisor {
    fn start(fixture: &TestEnvironment) -> Self {
        let child = fixture
            .command()
            .args(["__supervisor", RUN_ID, PROJECT_ID])
            .arg(&fixture.project)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let supervisor = Self(child);
        wait_until("supervisor publication", || {
            fixture.index_entry_count() == 1
        });
        supervisor
    }

    fn stop(&mut self, fixture: &TestEnvironment) {
        fixture.run_json(&["stop", "--json"]);
        wait_until("supervisor exit after foreground cleanup", || {
            self.0.try_wait().unwrap().is_some()
        });
        assert!(self.0.wait().unwrap().success());
        assert_eq!(fixture.index_entry_count(), 0);
        assert!(
            !fixture
                .runtime
                .join("coterie")
                .join(format!("{RUN_ID}.sock"))
                .exists()
        );
    }
}

impl Drop for Supervisor {
    fn drop(&mut self) {
        // A regression in normal stop must not orphan the test's supervisor.
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

fn configure(fixture: &TestEnvironment) {
    write_global(
        fixture,
        &include_str!("../../examples/config/global.toml")
            .replace("providers.codex", "providers.real_codex")
            .replace("provider = \"codex\"", "provider = \"real_codex\"")
            .replace(
                "command = [\"codex\"]",
                &format!(
                    "command = [{}]",
                    serde_json::to_string(&fixture.root.join("bin/codex"))
                        .unwrap()
                ),
            ),
    );
}

#[test]
fn configured_foreground_provider_starts_reconnects_and_stops() {
    let fixture = TestEnvironment::new();
    configure(&fixture);
    let mut supervisor = Supervisor::start(&fixture);
    fixture.launch(&[]);
    let first = fixture.run_json(&["status", "--json"]);
    assert_eq!(first["data"]["agents"][0]["state"], "exited");
    assert_eq!(first["data"]["agents"][0]["role"], "coordinator");

    let capture = fixture.root.join("signals");
    let ready = fixture.root.join("ready");
    let mut foreground = fixture
        .command()
        .env("COTERIE_FAKE_MODE", "stop")
        .env("COTERIE_FAKE_CAPTURE", &capture)
        .env("COTERIE_FAKE_READY", &ready)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_until("configured foreground startup", || {
        ready.exists()
            && fixture.run_json(&["status", "--json"])["data"]["agents"][0]["state"]
                == "running"
    });
    let active = fixture.run_json(&["status", "--json"]);
    assert_eq!(
        active["data"]["agents"][0]["id"],
        first["data"]["agents"][0]["id"]
    );
    // Shutdown and exit observations must keep using the saved binding.
    fs::write(fixture.project.join("coterie.toml"), "invalid = true\n")
        .unwrap();
    supervisor.stop(&fixture);
    wait_until("configured foreground reap", || {
        foreground.try_wait().unwrap().is_some()
    });
    assert!(foreground.wait().unwrap().success());
    assert_eq!(fs::read_to_string(capture).unwrap(), "int\nterm\n");
    let connection = rusqlite::Connection::open_with_flags(
        fixture
            .state
            .join("coterie/runs")
            .join(RUN_ID)
            .join("state.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let observed: i64 = connection.query_row("SELECT count(*) FROM sessions WHERE provider = 'real_codex' AND process_owner = 'foreground' AND state = 'exited' AND ended_at IS NOT NULL", [], |row| row.get(0)).unwrap();
    assert_eq!(observed, 2);
}

#[test]
fn configured_foreground_launch_failure_allows_supervisor_cleanup() {
    let fixture = TestEnvironment::new();
    configure(&fixture);
    let mut supervisor = Supervisor::start(&fixture);
    let provider = fixture.root.join("bin/codex");
    // The final static probe succeeds, but creating the provider process fails.
    fs::write(
        &provider,
        FAKE_CODEX.replace(
            "then printf '%s\\n'",
            "then chmod 600 \"$0\"; printf '%s\\n'",
        ),
    )
    .unwrap();
    let output = run(fixture.command());
    assert_eq!(output.status.code(), Some(7), "{output:?}");
    let status = fixture.run_json(&["status", "--json"]);
    supervisor.stop(&fixture);
    assert_eq!(status["data"]["agents"][0]["state"], "lost");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("Permission denied")
    );
}

struct Foreground<'a> {
    child: Child,
    fixture: &'a TestEnvironment,
}

impl Drop for Foreground<'_> {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.fixture.command().args(["stop", "--json"]).output();
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

#[test]
fn doctor_distinguishes_hidden_and_closed_foreground_terminals_read_only() {
    use nix::pty::{grantpt, posix_openpt, ptsname_r, unlockpt};
    let fixture = TestEnvironment::new();
    configure(&fixture);
    let mut supervisor = Supervisor::start(&fixture);
    let master =
        posix_openpt(nix::fcntl::OFlag::O_RDWR | nix::fcntl::OFlag::O_CLOEXEC)
            .unwrap();
    grantpt(&master).unwrap();
    unlockpt(&master).unwrap();
    let slave = fs::File::options()
        .read(true)
        .write(true)
        .open(ptsname_r(&master).unwrap())
        .unwrap();
    let attributes = nix::sys::termios::tcgetattr(&slave).unwrap();
    let ready = fixture.root.join("ready");
    let capture = fixture.root.join("signals");
    // No controlling terminal sends HUP here, reproducing a deleted editor
    // terminal whose wrapper and provider survive. No viewer reads the master.
    let child = fixture
        .command()
        .env("COTERIE_FAKE_MODE", "stop")
        .env("COTERIE_FAKE_READY", &ready)
        .env("COTERIE_FAKE_CAPTURE", &capture)
        .stdin(slave.try_clone().unwrap())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut foreground = Foreground {
        child,
        fixture: &fixture,
    };
    wait_until("foreground startup observation", || {
        ready.exists()
            && fixture.run_json(&["status", "--json"])["data"]["agents"][0]["state"]
                == "running"
    });
    let database = fixture
        .state
        .join("coterie/runs")
        .join(RUN_ID)
        .join("state.sqlite3");
    let connection = rusqlite::Connection::open_with_flags(
        &database,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let snapshot = || {
        (
            query_rows(
                &connection,
                "SELECT id, state, provider_session_id, ended_at FROM sessions ORDER BY id",
                4,
            ),
            query_rows(
                &connection,
                "SELECT session_id, identity_json FROM foreground_process_identity ORDER BY session_id",
                2,
            ),
            query_rows(
                &connection,
                "SELECT sequence, payload_json FROM events ORDER BY sequence",
                2,
            ),
        )
    };
    let before = snapshot();
    let inspect = |status: &str, message: &str| {
        let report = fixture.run_json(&["doctor", "--json"]);
        let checks = report["data"]["report"]["checks"].as_array().unwrap();
        let terminals: Vec<_> = checks
            .iter()
            .filter(|check| check["check"] == "foreground_terminal")
            .collect();
        assert_eq!(terminals.len(), 1, "{report}");
        let terminal = terminals[0];
        assert_eq!(terminal["status"], status, "{terminal}");
        assert!(terminal["subject"].as_str().unwrap().starts_with("cs-"));
        assert!(
            terminal["message"].as_str().unwrap().contains(message),
            "{terminal}"
        );
        let output = fixture.command().arg("doctor").output().unwrap();
        assert!(output.status.success(), "{output:?}");
        assert!(output.stderr.is_empty());
        let human = String::from_utf8(output.stdout).unwrap();
        assert!(
            human.contains("foreground_terminal") && human.contains(message),
            "{human}"
        );
        if status == "warning" {
            assert!(
                terminal["message"]
                    .as_str()
                    .unwrap()
                    .contains("coterie stop")
            );
            assert!(human.contains("coterie stop"));
        }
        assert_eq!(
            snapshot(),
            before,
            "inspection must not change durable state"
        );
    };
    inspect("ok", "PTY remains linked");
    assert_eq!(nix::sys::termios::tcgetattr(&slave).unwrap(), attributes);
    assert!(foreground.child.try_wait().unwrap().is_none());
    drop(master);
    wait_until("PTY master close", || {
        slave.metadata().unwrap().nlink() == 0
    });
    inspect("warning", "stranded");
    inspect("warning", "stranded");
    assert!(foreground.child.try_wait().unwrap().is_none());
    assert!(!capture.exists(), "doctor must not signal the provider");
    supervisor.stop(&fixture);
    wait_until("stranded foreground cleanup", || {
        foreground.child.try_wait().unwrap().is_some()
    });
    assert!(foreground.child.wait().unwrap().success());
    assert_eq!(fs::read_to_string(capture).unwrap(), "int\nterm\n");
}

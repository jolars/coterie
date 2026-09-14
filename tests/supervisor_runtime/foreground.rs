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

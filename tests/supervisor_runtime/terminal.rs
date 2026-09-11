use std::os::unix::process::CommandExt;

use nix::fcntl::OFlag;
use nix::pty::{grantpt, posix_openpt, ptsname_r, unlockpt};
use nix::unistd::{getpgrp, setsid, tcgetpgrp};

use super::*;

#[test]
fn foreground_terminal_child() {
    let Some(terminal) = std::env::var_os("COTERIE_TEST_TERMINAL") else {
        return;
    };
    // Acquire the controlling terminal in a separate process without pre_exec.
    setsid().unwrap();
    let terminal = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(terminal)
        .unwrap();
    assert_eq!(tcgetpgrp(&terminal).unwrap(), getpgrp());
    let error = Command::new(env!("CARGO_BIN_EXE_coterie"))
        .env_remove("COTERIE_TEST_TERMINAL")
        .stdin(terminal.try_clone().unwrap())
        .stdout(terminal.try_clone().unwrap())
        .stderr(terminal)
        .exec();
    panic!("could not execute the foreground wrapper: {error}");
}

#[test]
fn terminal_hangup_reaps_foreground_and_preserves_workers_on_reconnect() {
    foreground_shutdown(None, false);
}

#[test]
fn terminal_hangup_kills_an_unresponsive_foreground() {
    foreground_shutdown(None, true);
}

#[test]
fn foreground_termination_signal_has_a_bounded_deadline() {
    foreground_shutdown(Some(Signal::SIGTERM), true);
}

#[test]
fn repeated_hangup_does_not_extend_foreground_cleanup() {
    foreground_shutdown(Some(Signal::SIGHUP), true);
}

#[test]
fn foreground_quit_signal_has_a_bounded_deadline() {
    foreground_shutdown(Some(Signal::SIGQUIT), true);
}

fn foreground_shutdown(signal: Option<Signal>, stubborn: bool) {
    let fixture = TestEnvironment::new();
    write_global(
        &fixture,
        "[supervision]\ninterrupt_grace_ms = 100\nshutdown_timeout_ms = 2000\n",
    );
    let capture = fixture.root.join("terminal-signals");
    let ready = fixture.root.join("terminal-ready");
    let handler = if stubborn { ":" } else { "exit 0" };
    let script = FAKE_CODEX.replace(
        "if [ \"${COTERIE_FAKE_MODE-}\" = \"signals\" ]; then",
        &format!(
            r#"if [ "${{COTERIE_FAKE_MODE-}}" = "terminal" ]; then
  trap 'printf "hup\n" >> "$COTERIE_FAKE_CAPTURE"' HUP
  trap 'printf "int\n" >> "$COTERIE_FAKE_CAPTURE"' INT
  # Catch QUIT explicitly so Bash and Dash both exercise shutdown escalation.
  trap 'printf "quit\n" >> "$COTERIE_FAKE_CAPTURE"' QUIT
  trap 'printf "term\n" >> "$COTERIE_FAKE_CAPTURE"; {handler}' TERM
  printf '%s\n' "$$" > "$COTERIE_FAKE_READY"
  while :; do :; done
fi
if [ "${{COTERIE_FAKE_MODE-}}" = "signals" ]; then"#,
        ),
    );
    fs::write(fixture.root.join("bin/codex"), script).unwrap();
    fixture.launch(&[]);
    let task = fixture.run_json(&["task", "create", "Keep working", "--json"]);
    let worker = fixture.run_json(&[
        "spawn",
        "worker",
        "--task",
        task["data"]["task"]["id"].as_str().unwrap(),
        "--json",
    ]);
    let before = fixture.run_json(&["status", "--json"]);
    let terminal = posix_openpt(OFlag::O_RDWR | OFlag::O_CLOEXEC).unwrap();
    grantpt(&terminal).unwrap();
    unlockpt(&terminal).unwrap();
    let template = fixture.command();
    let mut command = Command::new(std::env::current_exe().unwrap());
    for (name, value) in template.get_envs() {
        if let Some(value) = value {
            command.env(name, value);
        }
    }
    let mut foreground = command
        .args([
            "--exact",
            "terminal::foreground_terminal_child",
            "--nocapture",
        ])
        .current_dir(&fixture.project)
        .env("COTERIE_TEST_TERMINAL", ptsname_r(&terminal).unwrap())
        .env("COTERIE_FAKE_MODE", "terminal")
        .env("COTERIE_FAKE_READY", &ready)
        .env("COTERIE_FAKE_CAPTURE", &capture)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut provider_pid = None;
    wait_until("foreground terminal provider PID", || {
        // Shell redirection creates the file before printf writes the PID.
        provider_pid = fs::read_to_string(&ready)
            .ok()
            .and_then(|contents| {
                contents.strip_suffix('\n')?.parse::<i32>().ok()
            })
            .filter(|pid| *pid > 0);
        provider_pid.is_some()
    });
    let provider_pid = provider_pid.unwrap();
    if let Some(signal) = signal {
        kill(Pid::from_raw(foreground.id().try_into().unwrap()), signal)
            .unwrap();
    } else {
        drop(terminal);
    }
    let deadline = Instant::now() + Duration::from_secs(4);
    let exited = loop {
        if foreground.try_wait().unwrap().is_some() {
            break true;
        }
        if Instant::now() >= deadline {
            break false;
        }
        if signal == Some(Signal::SIGHUP) {
            let result = kill(
                Pid::from_raw(foreground.id().try_into().unwrap()),
                Signal::SIGHUP,
            );
            assert!(result.is_ok() || result == Err(nix::errno::Errno::ESRCH));
        }
        thread::sleep(Duration::from_millis(20));
    };
    if !exited {
        // Exercise the failing behavior without leaving the fixture alive.
        fixture.run_json(&["stop", "--json"]);
        foreground.wait().unwrap();
    }
    assert!(
        exited,
        "closing the foreground must have a bounded deadline"
    );
    assert_eq!(
        kill(Pid::from_raw(provider_pid), None),
        Err(nix::errno::Errno::ESRCH),
        "the owned provider must be reaped before the wrapper exits"
    );
    let signals = fs::read_to_string(capture).unwrap();
    if signal == Some(Signal::SIGQUIT) {
        assert!(signals.contains("quit\n"), "{signals}");
    }
    assert!(signals.contains("term\n"), "{signals}");
    let after = fixture.run_json(&["status", "--json"]);
    assert_eq!(after["data"]["run_id"], before["data"]["run_id"]);
    assert_eq!(after["data"]["status"], "active");
    assert_eq!(after["data"]["tasks"]["in_progress"], 1);
    assert!(
        after["data"]["agents"]
            .as_array()
            .unwrap()
            .iter()
            .any(|agent| agent["id"] == worker["data"]["agent"]["id"]
                && agent["state"] == "running")
    );
    fixture.launch(&[]);
    let reconnected = fixture.run_json(&["status", "--json"]);
    assert_eq!(reconnected["data"]["run_id"], before["data"]["run_id"]);
    fixture.run_json(&["stop", "--json"]);
}

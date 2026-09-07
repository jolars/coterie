use std::fs;
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use git2::{Repository, Signature, StatusOptions};
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use serde_json::Value;

const RUN_ID: &str = "cr-01ARZ3NDEKTSV4RRFFQ69G5FAV";
const PROJECT_ID: &str = "cp-01ARZ3NDEKTSV4RRFFQ69G5FAW";
const FAKE_CODEX: &str = r#"#!/bin/sh
if [ "$1" = "--version" ]; then
  printf 'codex-cli 0.151.0\n'
  exit 0
fi
if [ "$1" = "--help" ]; then
  printf 'Usage: codex [OPTIONS] [PROMPT]\n  --config <key=value>\n  --cd <DIR>\n  --sandbox <SANDBOX_MODE>\n  --ask-for-approval <APPROVAL_POLICY>\n'
  exit 0
fi
if [ "$1" = "exec" ] && [ "$2" = "--help" ]; then
  printf 'Usage: codex exec [OPTIONS] [PROMPT]\n  --config <key=value>\n  --cd <DIR>\n  --sandbox <SANDBOX_MODE>\n  --ask-for-approval <APPROVAL_POLICY>\n  --json\n'
  exit 0
fi
if [ "${COTERIE_FAKE_MODE-}" = "contract" ]; then
  {
    printf 'cwd=%s\0' "$PWD"
    for argument in "$@"; do
      printf 'arg=%s\0' "$argument"
    done
    for variable in COTERIE_PROJECT_ROOT COTERIE_PROJECT_ID COTERIE_PRIMARY_PROJECT_ROOT COTERIE_RUN_ID COTERIE_AGENT_ID COTERIE_SESSION_ID COTERIE_ROLE COTERIE_SOCKET COTERIE_TOKEN; do
      eval "value=\${$variable}"
      printf 'env:%s=%s\0' "$variable" "$value"
    done
    if [ "${COTERIE_TASK_ID+x}" = "x" ]; then
      printf 'env:COTERIE_TASK_ID=%s\0' "$COTERIE_TASK_ID"
    fi
  } > "$COTERIE_FAKE_CAPTURE"
  IFS= read -r input
  printf 'stdout:%s\n' "$input"
  printf 'stderr:%s\n' "$input" >&2
  exit 0
fi
if [ "${COTERIE_FAKE_MODE-}" = "signals" ]; then
  trap 'printf "winch\n" >> "$COTERIE_FAKE_CAPTURE"' WINCH
  trap 'printf "int\n" >> "$COTERIE_FAKE_CAPTURE"; exit 0' INT
  printf 'ready\n' > "$COTERIE_FAKE_READY"
  while :; do
    :
  done
fi
if [ "${COTERIE_FAKE_MODE-}" = "stop" ]; then
  trap 'printf "term\n" > "$COTERIE_FAKE_CAPTURE"; exit 0' TERM
  printf 'ready\n' > "$COTERIE_FAKE_READY"
  while :; do
    :
  done
fi
if [ "${COTERIE_FAKE_MODE-}" = "restart" ]; then
  printf 'ready\n' > "$COTERIE_FAKE_READY"
  while [ ! -e "$COTERIE_FAKE_RELEASE" ]; do
    :
  done
  exit 23
fi
is_job=false
for argument in "$@"; do
  if [ "$argument" = "exec" ]; then
    is_job=true
  fi
done
if [ "$is_job" = true ]; then
  parent=$PPID
  printf '{"type":"thread.started","thread_id":"thread-1"}\n'
  while [ ! -e "$COTERIE_SOCKET.release" ]; do
    if [ ! -e "/proc/$parent" ]; then
      exit 0
    fi
  done
  coterie="${0%/*}/coterie"
  "$coterie" inbox --json > /dev/null || exit 20
  "$coterie" inbox ack 1 --json > /dev/null || exit 21
  "$coterie" finish --status completed --summary "Implemented and tested." --json > /dev/null || exit 22
  printf '{"type":"turn.completed","usage":{}}\n'
  exit 0
fi
exit 0
"#;

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
        "status",
        "whoami",
        "prime",
        "task",
        "spawn",
        "finish",
        "send",
        "inbox",
        "logs",
        "workspace",
        "events",
        "stop",
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

    let mut command = fixture.command();
    command.args(["workspace", "--help"]);
    let output = run(command);
    assert!(output.status.success(), "workspace help failed: {output:?}");
    let stdout = String::from_utf8(output.stdout)
        .expect("workspace help should be UTF-8");
    assert!(
        stdout.contains("integrate"),
        "workspace help should list `integrate`:\n{stdout}"
    );
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
fn interactive_foreground_rejects_json_before_starting_a_run() {
    let fixture = TestEnvironment::new();
    let mut command = fixture.command();
    command.arg("--json");

    let output = run(command);

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let error: Value = serde_json::from_slice(&output.stderr)
        .expect("the diagnostic should be one JSON response");
    assert_eq!(error["error"]["code"], "invalid_argument");
    assert!(error["operation_id"].as_str().is_some());
    assert_eq!(fixture.index_entry_count(), 0);
}

#[test]
fn missing_codex_is_reported_as_unavailable() {
    let fixture = TestEnvironment::new();
    let empty_path = fixture.root.join("empty-path");
    fs::create_dir(&empty_path).expect("the empty PATH should be created");
    let mut command = fixture.command();
    command.env("PATH", empty_path);

    let output = run(command);

    assert_eq!(output.status.code(), Some(7));
    assert!(output.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("could not execute a Codex probe")
    );
    fixture.run_json(&["stop", "--json"]);
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

    fixture.launch(&[]);
    let mut inbox = fixture.command();
    inbox.args(["inbox", "--json"]);
    let output = run(inbox);
    assert_eq!(output.status.code(), Some(6));
    let error: Value = serde_json::from_slice(&output.stderr)
        .expect("the operator inbox rejection should be JSON");
    assert_eq!(error["error"]["code"], "permission_denied");

    let mut acknowledge = fixture.command();
    acknowledge.args([
        "inbox",
        "ack",
        "1",
        "--operation-id",
        "co-01ARZ3NDEKTSV4RRFFQ69G5FB9",
        "--json",
    ]);
    let output = run(acknowledge);
    assert_eq!(output.status.code(), Some(6));
    let error: Value = serde_json::from_slice(&output.stderr)
        .expect("the acknowledgement rejection should be JSON");
    assert_eq!(error["operation_id"], "co-01ARZ3NDEKTSV4RRFFQ69G5FB9");
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
    assert!(launch.stdout.is_empty());
    assert!(launch.stderr.is_empty());
    let status = fixture.run_json(&["status", "--json"]);
    assert_eq!(status["data"]["agents"][0]["name"], "lead");
    assert_eq!(status["data"]["agents"][0]["state"], "exited");

    let mut invalid = fixture.command();
    invalid.args(["task", "close", "not-a-task"]);
    let invalid = run(invalid);
    assert_eq!(invalid.status.code(), Some(2));
    assert!(invalid.stdout.is_empty());
    assert!(!invalid.stderr.is_empty());

    fixture.run_json(&["stop", "--json"]);
}

#[test]
fn clean_git_launch_starts_and_reconnects_to_silent_foreground_leads() {
    let fixture = TestEnvironment::new();
    assert_repository_clean(&fixture.project);

    fixture.launch(&[]);
    let initial_status = fixture.run_json(&["status", "--json"]);
    let run_id = initial_status["data"]["run_id"]
        .as_str()
        .expect("status should identify the started run")
        .to_owned();
    let socket = fixture
        .runtime
        .join("coterie")
        .join(format!("{run_id}.sock"));
    let socket_inode = fs::metadata(&socket)
        .expect("the started supervisor should publish its socket")
        .ino();

    assert_eq!(initial_status["data"]["agents"][0]["name"], "lead");
    assert_eq!(initial_status["data"]["agents"][0]["state"], "exited");

    fixture.launch(&[]);
    let reconnected_status = fixture.run_json(&["status", "--json"]);

    assert_eq!(reconnected_status["data"]["run_id"], run_id);
    assert_eq!(
        fs::metadata(&socket)
            .expect("the reconnected supervisor socket should remain live")
            .ino(),
        socket_inode
    );
    assert_eq!(reconnected_status["data"]["agents"][0]["name"], "lead");
    assert_eq!(reconnected_status["data"]["agents"][0]["state"], "exited");
    assert_repository_clean(&fixture.project);

    fixture.run_json(&["stop", "--json"]);
}

#[test]
fn foreground_codex_inherits_streams_directory_identity_and_agents_discovery() {
    let fixture = TestEnvironment::new();
    let capture = fixture.root.join("codex-contract");
    let mut command = fixture.command();
    command
        .env("COTERIE_FAKE_MODE", "contract")
        .env("COTERIE_FAKE_CAPTURE", &capture)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().expect("Coterie should start");
    child
        .stdin
        .take()
        .expect("the foreground stdin should be piped")
        .write_all(b"terminal input\n")
        .expect("the terminal input should be writable");

    let output = child
        .wait_with_output()
        .expect("the foreground process should finish");

    assert!(output.status.success(), "foreground failed: {output:?}");
    assert_eq!(output.stdout, b"stdout:terminal input\n");
    assert_eq!(output.stderr, b"stderr:terminal input\n");
    let records = fs::read(&capture)
        .expect("the Codex contract should be captured")
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
        .map(|record| String::from_utf8(record.to_vec()).expect("UTF-8 record"))
        .collect::<Vec<_>>();
    assert!(records.contains(&format!("cwd={}", fixture.project.display())));
    let arguments = records
        .iter()
        .filter_map(|record| record.strip_prefix("arg="))
        .collect::<Vec<_>>();
    assert_eq!(arguments.len(), 10, "the bootstrap must not be a prompt");
    assert_eq!(arguments[0], "--sandbox");
    assert_eq!(arguments[1], "workspace-write");
    assert_eq!(arguments[2], "--ask-for-approval");
    assert_eq!(arguments[3], "on-request");
    assert_eq!(arguments[4], "--config");
    assert_eq!(arguments[5], "approvals_reviewer=\"user\"");
    assert_eq!(arguments[6], "--cd");
    assert_eq!(arguments[7], fixture.project.to_string_lossy());
    assert_eq!(arguments[8], "--config");
    assert!(arguments[9].starts_with("developer_instructions=\""));
    assert!(arguments[9].contains("Run `coterie prime`"));
    assert!(arguments[9].contains("AGENTS.md"));
    for variable in [
        "COTERIE_PROJECT_ROOT",
        "COTERIE_PROJECT_ID",
        "COTERIE_PRIMARY_PROJECT_ROOT",
        "COTERIE_RUN_ID",
        "COTERIE_AGENT_ID",
        "COTERIE_SESSION_ID",
        "COTERIE_ROLE",
        "COTERIE_SOCKET",
        "COTERIE_TOKEN",
    ] {
        assert!(
            records.iter().any(|record| {
                record
                    .strip_prefix(&format!("env:{variable}="))
                    .is_some_and(|value| !value.is_empty())
            }),
            "Codex should receive {variable}"
        );
    }
    assert!(
        !records
            .iter()
            .any(|record| record.starts_with("env:COTERIE_TASK_ID=")),
        "a foreground lead must not inherit a stale task identity"
    );
    fixture.run_json(&["stop", "--json"]);
}

#[test]
fn foreground_signals_sent_to_coterie_reach_codex() {
    let fixture = TestEnvironment::new();
    let capture = fixture.root.join("codex-signals");
    let ready = fixture.root.join("codex-ready");
    let mut command = fixture.command();
    command
        .env("COTERIE_FAKE_MODE", "signals")
        .env("COTERIE_FAKE_CAPTURE", &capture)
        .env("COTERIE_FAKE_READY", &ready)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().expect("Coterie should start");
    let _open_stdin = child
        .stdin
        .take()
        .expect("the foreground stdin should stay open");
    wait_until("the fake Codex process", || ready.exists());
    let coterie_pid =
        Pid::from_raw(i32::try_from(child.id()).expect("PID should fit"));

    kill(coterie_pid, Signal::SIGWINCH).expect("SIGWINCH should be sent");
    wait_until("the resize signal", || {
        fs::read_to_string(&capture)
            .unwrap_or_default()
            .contains("winch")
    });
    kill(coterie_pid, Signal::SIGINT).expect("SIGINT should be sent");
    wait_until("the interrupted foreground", || {
        child
            .try_wait()
            .expect("status should be readable")
            .is_some()
    });
    let status = child.wait().expect("the foreground should be reaped");

    assert!(status.success());
    let signals = fs::read_to_string(&capture)
        .expect("the delivered signals should be recorded");
    assert!(signals.contains("winch\n"));
    assert!(signals.contains("int\n"));
    assert_eq!(fixture.index_entry_count(), 1);
    fixture.run_json(&["stop", "--json"]);
}

#[test]
fn stop_terminates_and_reaps_the_foreground_codex_process() {
    let fixture = TestEnvironment::new();
    let capture = fixture.root.join("codex-stop");
    let ready = fixture.root.join("codex-ready");
    let mut command = fixture.command();
    command
        .env("COTERIE_FAKE_MODE", "stop")
        .env("COTERIE_FAKE_CAPTURE", &capture)
        .env("COTERIE_FAKE_READY", &ready)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut foreground = command.spawn().expect("Coterie should start");
    wait_until("the fake Codex process", || ready.exists());

    let stopped = fixture.run_json(&["stop", "--json"]);

    assert_eq!(stopped["data"]["status"], "stopped");
    wait_until("the foreground wrapper", || {
        foreground
            .try_wait()
            .expect("the wrapper status should be readable")
            .is_some()
    });
    assert!(
        foreground
            .wait()
            .expect("the wrapper should be reaped")
            .success()
    );
    assert_eq!(
        fs::read_to_string(&capture).expect("Codex should record termination"),
        "term\n"
    );
    assert_eq!(fixture.index_entry_count(), 0);
}

#[test]
fn foreground_exit_is_recorded_after_the_supervisor_restarts() {
    let fixture = TestEnvironment::new();
    let mut supervisor = fixture.command();
    supervisor
        .arg("__supervisor")
        .arg(RUN_ID)
        .arg(PROJECT_ID)
        .arg(&fixture.project)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut crashed = supervisor.spawn().expect("the supervisor should start");
    wait_until("supervisor publication", || {
        fixture.index_entry_count() == 1
    });

    let ready = fixture.root.join("codex-ready");
    let release = fixture.root.join("codex-release");
    let mut command = fixture.command();
    command
        .env("COTERIE_FAKE_MODE", "restart")
        .env("COTERIE_FAKE_READY", &ready)
        .env("COTERIE_FAKE_RELEASE", &release)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let foreground = command.spawn().expect("Coterie should start");
    wait_until("the fake Codex process", || ready.exists());

    crashed.kill().expect("the supervisor should crash");
    crashed
        .wait()
        .expect("the crashed supervisor should be reaped");
    let restarted = run(fixture.connect_command());
    assert!(restarted.status.success(), "restart failed: {restarted:?}");
    let database = fixture
        .state
        .join("coterie/runs")
        .join(RUN_ID)
        .join("state.sqlite3");
    let connection = rusqlite::Connection::open(&database)
        .expect("the restarted run database should open");
    let (state, reconciliation, owner, active_credentials):
        (String, String, String, i64) = connection
        .query_row(
            "SELECT sessions.state, sessions.reconciliation_state, \
                    sessions.process_owner, \
                    COUNT(session_credentials.session_id) \
             FROM sessions \
             JOIN session_credentials ON session_credentials.session_id = sessions.id \
             WHERE sessions.provider_session_id LIKE 'process:%' \
               AND session_credentials.revoked_at IS NULL",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("the live foreground session should remain durable");
    assert_eq!(state, "unknown");
    assert_eq!(reconciliation, "unknown");
    assert_eq!(owner, "foreground");
    assert_eq!(active_credentials, 1);
    drop(connection);

    let mut contender = fixture.command();
    contender.args(["--operation-id", "co-01ARZ3NDEKTSV4RRFFQ69G5FBE"]);
    let contender = run(contender);
    assert_eq!(contender.status.code(), Some(5));
    assert!(
        String::from_utf8_lossy(&contender.stderr).contains("already active")
    );

    fs::write(&release, b"exit").expect("Codex should be released");
    let output = foreground
        .wait_with_output()
        .expect("the foreground wrapper should finish");

    assert_eq!(
        output.status.code(),
        Some(7),
        "foreground output: {output:?}"
    );
    let connection = rusqlite::Connection::open(database)
        .expect("the restarted run database should open");
    let (state, reconciliation): (String, String) = connection
        .query_row(
            "SELECT state, reconciliation_state FROM sessions \
             WHERE provider_session_id LIKE 'process:%'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("the foreground exit should be durable");
    assert_eq!(state, "exited");
    assert_eq!(reconciliation, "observed");
    let exit_code: i64 = connection
        .query_row(
            "SELECT json_extract(payload_json, '$.data.details.code') \
             FROM events \
             WHERE event_type = 'session.lifecycle_changed' \
               AND actor = 'foreground' \
             ORDER BY sequence DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .expect("the definitive foreground exit code should be durable");
    assert_eq!(exit_code, 23);
    fixture.run_json(&["stop", "--json"]);
}

#[test]
fn operator_commands_drive_the_minimum_delegation_flow() {
    let fixture = TestEnvironment::new();

    const FIRST_LAUNCH: &str = "co-01ARZ3NDEKTSV4RRFFQ69G5FB0";
    fixture.launch(&["--operation-id", FIRST_LAUNCH]);
    let initial_status = fixture.run_json(&["status", "--json"]);
    let run_id = initial_status["data"]["run_id"]
        .as_str()
        .expect("status should identify the run")
        .to_owned();
    assert_eq!(initial_status["data"]["agents"][0]["name"], "lead");
    assert_eq!(initial_status["data"]["agents"][0]["state"], "exited");
    let mut replay = fixture.command();
    replay.args(["--operation-id", FIRST_LAUNCH]);
    let replay = run(replay);
    assert_eq!(replay.status.code(), Some(5));
    assert!(replay.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&replay.stderr)
            .contains("has already attempted its provider launch")
    );
    fixture.launch(&["--operation-id", "co-01ARZ3NDEKTSV4RRFFQ69G5FBD"]);
    let reconnected = fixture.run_json(&["status", "--json"]);
    assert_eq!(reconnected["data"]["run_id"], run_id);
    assert_eq!(reconnected["data"]["agents"][0]["name"], "lead");
    assert_eq!(reconnected["data"]["agents"][0]["state"], "exited");

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
    let session_id = spawn["data"]["session_id"]
        .as_str()
        .expect("spawn should return a session ID");
    let assignment_id = spawn["data"]["assignment_id"]
        .as_str()
        .expect("spawn should return an assignment ID");
    let database = fixture
        .state
        .join("coterie/runs")
        .join(&run_id)
        .join("state.sqlite3");
    let connection = rusqlite::Connection::open(&database)
        .expect("the active run database should be readable");
    let (provider_session_id, session_reconciliation): (
        Option<String>,
        String,
    ) = connection
        .query_row(
            "SELECT provider_session_id, reconciliation_state \
                 FROM sessions WHERE id = ?1",
            [session_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("the session observation should be durable");
    let worker_process_id = provider_session_id
        .as_deref()
        .and_then(|provider_id| provider_id.strip_prefix("process:"))
        .and_then(|process_id| process_id.parse::<u32>().ok())
        .expect("the Codex worker should have a process identity");
    assert_eq!(session_reconciliation, "observed");
    let (workspace_kind, workspace_path, workspace_reconciliation, base_commit): (
        String,
        Vec<u8>,
        String,
        String,
    ) = connection
        .query_row(
            "SELECT kind, path, state, base_commit FROM workspaces WHERE assignment_id = ?1",
            [assignment_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("the workspace observation should be durable");
    assert_eq!(workspace_kind, "worktree");
    assert_eq!(workspace_reconciliation, "observed");
    let workspace_path =
        Path::new(std::ffi::OsStr::from_bytes(&workspace_path));
    assert!(
        workspace_path
            .starts_with(fixture.state.join("coterie/runs").join(&run_id)),
        "the assignment workspace should be rooted in private run state"
    );
    let project_repository = Repository::open(&fixture.project)
        .expect("the target repository should open");
    assert_eq!(
        project_repository
            .head()
            .and_then(|head| head.peel_to_commit())
            .expect("the target HEAD should resolve")
            .id()
            .to_string(),
        base_commit
    );
    let reference_name = format!("refs/heads/coterie/{run_id}/{assignment_id}");
    assert_eq!(
        project_repository
            .find_reference(&reference_name)
            .expect("the assignment reference should exist")
            .target()
            .map(|oid| oid.to_string())
            .as_deref(),
        Some(base_commit.as_str())
    );
    let workspace_repository = Repository::open(workspace_path)
        .expect("the assignment worktree should open");
    assert_eq!(
        workspace_repository
            .head()
            .expect("the assignment HEAD should resolve")
            .name()
            .expect("the assignment HEAD should be UTF-8"),
        reference_name
    );
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

    let mut logs = None;
    wait_until("the worker transcript", || {
        let observed = fixture.run_json(&["logs", "worker-1", "--json"]);
        let ready = observed["data"]["transcript"]
            .as_str()
            .is_some_and(|transcript| transcript.contains("thread.started"));
        if ready {
            logs = Some(observed);
        }
        ready
    });
    let logs = logs.expect("the worker transcript should be observed");
    assert_eq!(logs["data"]["agent"]["name"], "worker-1");
    assert!(
        logs["data"]["transcript"]
            .as_str()
            .is_some_and(|transcript| transcript.contains("thread.started"))
    );

    let events = fixture.run_json(&["events", "--json"]);
    let events = events["data"]["events"]
        .as_array()
        .expect("the typed event stream should be an array");
    assert!(!events.is_empty());
    assert!(events.windows(2).all(|events| {
        events[0]["sequence"].as_u64() < events[1]["sequence"].as_u64()
    }));
    assert!(events.iter().all(|event| {
        event["run_id"] == run_id && event["payload"]["schema_version"] == 1
    }));
    for event_type in [
        "run.started",
        "project.attached",
        "agent.created",
        "session.started",
        "session.reconciliation_changed",
        "session.lifecycle_changed",
        "task.created",
        "task.claimed",
        "assignment.created",
        "workspace.desired",
        "workspace.reconciliation_changed",
        "message.sent",
    ] {
        assert!(
            events.iter().any(|event| event["event_type"] == event_type),
            "the runtime should normalize `{event_type}`"
        );
    }

    fs::write(workspace_path.join("uncommitted.txt"), "recoverable work\n")
        .expect("the assignment worktree should become dirty");
    let stopped = fixture.run_json(&["stop", "--json"]);
    assert_eq!(stopped["data"]["run_id"], run_id);
    assert_eq!(stopped["data"]["status"], "stopped");
    assert!(
        workspace_path.exists(),
        "stopping must preserve a dirty, unintegrated worker workspace"
    );
    assert_eq!(
        fs::read_to_string(workspace_path.join("uncommitted.txt"))
            .expect("recoverable work should remain readable"),
        "recoverable work\n"
    );
    assert!(
        project_repository.find_reference(&reference_name).is_ok(),
        "stopping must preserve the owned reference while work is recoverable"
    );
    assert!(
        !Path::new("/proc")
            .join(worker_process_id.to_string())
            .exists(),
        "stopping must terminate and reap the active Codex worker"
    );
}

#[test]
fn operator_completes_the_codex_worker_loop_through_validation() {
    let fixture = TestEnvironment::new();
    fixture.launch(&[]);
    let status = fixture.run_json(&["status", "--json"]);
    let run_id = status["data"]["run_id"]
        .as_str()
        .expect("status should identify the run")
        .to_owned();
    let task = fixture.run_json(&[
        "task",
        "create",
        "Implement the worker result",
        "--json",
    ]);
    let task_id = task["data"]["task"]["id"]
        .as_str()
        .expect("task creation should return an ID")
        .to_owned();
    let spawn =
        fixture.run_json(&["spawn", "worker", "--task", &task_id, "--json"]);
    let assignment_id = spawn["data"]["assignment_id"]
        .as_str()
        .expect("spawn should return an assignment ID")
        .to_owned();

    fixture.run_json(&[
        "send",
        "worker-1",
        "Include the requested result and tests.",
        "--json",
    ]);
    let database = fixture
        .state
        .join("coterie/runs")
        .join(&run_id)
        .join("state.sqlite3");
    let connection = rusqlite::Connection::open(&database)
        .expect("the active run database should open");
    let workspace_bytes: Vec<u8> = connection
        .query_row(
            "SELECT path FROM workspaces WHERE assignment_id = ?1",
            [&assignment_id],
            |row| row.get(0),
        )
        .expect("the assignment workspace should be durable");
    drop(connection);
    let workspace = Path::new(std::ffi::OsStr::from_bytes(&workspace_bytes));
    let result_commit = commit_file(
        workspace,
        Path::new("result.txt"),
        "worker result\n",
        "implement worker result",
    );
    let socket = fixture
        .runtime
        .join("coterie")
        .join(format!("{run_id}.sock"));
    fs::write(socket.with_extension("sock.release"), b"finish")
        .expect("the scripted worker should be released");

    wait_until("the submitted assignment", || {
        fixture.run_json(&["status", "--json"])["data"]["tasks"]["submitted"]
            == 1
    });
    let mut premature_close = fixture.command();
    premature_close.args([
        "task",
        "close",
        &task_id,
        "--summary",
        "Validated before integration.",
        "--json",
    ]);
    let premature_close = run(premature_close);
    assert_eq!(premature_close.status.code(), Some(5));
    let error: Value = serde_json::from_slice(&premature_close.stderr)
        .expect("the integration requirement should be a JSON diagnostic");
    assert_eq!(error["error"]["code"], "conflict");
    assert!(
        error["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("not been integrated"))
    );

    let integrated = fixture.run_json(&[
        "workspace",
        "integrate",
        "--assignment",
        &assignment_id,
        "--json",
    ]);
    assert_eq!(
        integrated["data"]["integration"]["result_commit"],
        result_commit
    );
    let target_commit = integrated["data"]["integration"]["target_commit"]
        .as_str()
        .expect("integration should return the target commit")
        .to_owned();
    assert_eq!(
        fs::read_to_string(fixture.project.join("result.txt"))
            .expect("the integrated result should be readable"),
        "worker result\n"
    );

    let closed = fixture.run_json(&[
        "task",
        "close",
        &task_id,
        "--summary",
        "Integrated result and validated its tests.",
        "--json",
    ]);
    assert_eq!(closed["data"]["task"]["status"], "closed");
    assert_eq!(
        closed["data"]["task"]["result"]["integration"]["target_commit"],
        target_commit
    );
    assert_eq!(
        closed["data"]["task"]["result"]["validation_summary"],
        "Integrated result and validated its tests."
    );

    let mut logs = None;
    wait_until("the complete worker transcript", || {
        let observed = fixture.run_json(&["logs", "worker-1", "--json"]);
        let complete = observed["data"]["transcript"]
            .as_str()
            .is_some_and(|transcript| transcript.contains("turn.completed"));
        if complete {
            logs = Some(observed);
        }
        complete
    });
    let logs = logs.expect("the complete worker transcript should be observed");
    let transcript = logs["data"]["transcript"]
        .as_str()
        .expect("the worker transcript should be returned");
    assert!(transcript.contains("thread.started"));
    assert!(transcript.contains("turn.completed"));
    let events = fixture.run_json(&["events", "--json"]);
    let events = events["data"]["events"]
        .as_array()
        .expect("events should be returned");
    for event_type in [
        "message.sent",
        "message.acknowledged",
        "workspace.integrated",
        "task.lifecycle_changed",
    ] {
        assert!(
            events.iter().any(|event| event["event_type"] == event_type),
            "the completed loop should contain `{event_type}`"
        );
    }
    fixture.run_json(&["stop", "--json"]);
}

#[test]
fn worktree_workers_fail_closed_for_non_git_projects() {
    let fixture = TestEnvironment::new_plain();
    fixture.launch(&[]);
    let task = fixture.run_json(&[
        "task",
        "create",
        "Git-only work",
        "--operation-id",
        "co-01ARZ3NDEKTSV4RRFFQ69G5FBA",
        "--json",
    ]);
    let task_id = task["data"]["task"]["id"]
        .as_str()
        .expect("the task ID should be returned");
    let mut spawn = fixture.command();
    spawn.args([
        "spawn",
        "worker",
        "--task",
        task_id,
        "--operation-id",
        "co-01ARZ3NDEKTSV4RRFFQ69G5FBB",
        "--json",
    ]);

    let output = run(spawn);

    assert_eq!(output.status.code(), Some(7));
    assert!(output.stdout.is_empty());
    let error: Value = serde_json::from_slice(&output.stderr)
        .expect("the workspace rejection should be JSON");
    assert_eq!(error["error"]["code"], "unavailable");
    assert!(
        error["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("not Git-backed"))
    );
    let status = fixture.run_json(&["status", "--json"]);
    assert_eq!(status["data"]["tasks"]["open"], 1);
    assert_eq!(status["data"]["agents"].as_array().map(Vec::len), Some(1));
    fixture.run_json(&["stop", "--json"]);
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

#[test]
fn restart_marks_a_vanished_codex_worker_lost() {
    let fixture = TestEnvironment::new();
    let log_path = fixture.root.join("reconciliation-supervisor.log");
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
    wait_until("supervisor publication", || {
        fixture.index_entry_count() == 1
    });

    fixture.launch(&["--operation-id", "co-01ARZ3NDEKTSV4RRFFQ69G5FB8"]);
    let task = fixture.run_json(&[
        "task",
        "create",
        "Implement parser",
        "--operation-id",
        "co-01ARZ3NDEKTSV4RRFFQ69G5FBA",
        "--json",
    ]);
    let task_id = task["data"]["task"]["id"]
        .as_str()
        .expect("task creation should return an ID")
        .to_owned();
    let downstream = fixture.run_json(&[
        "task",
        "create",
        "Document parser",
        "--after",
        &task_id,
        "--operation-id",
        "co-01ARZ3NDEKTSV4RRFFQ69G5FBB",
        "--json",
    ]);
    let downstream_id = downstream["data"]["task"]["id"]
        .as_str()
        .expect("dependent task creation should return an ID")
        .to_owned();
    let spawn = fixture.run_json(&[
        "spawn",
        "worker",
        "--task",
        &task_id,
        "--operation-id",
        "co-01ARZ3NDEKTSV4RRFFQ69G5FBC",
        "--json",
    ]);
    let worker_session_id = spawn["data"]["session_id"]
        .as_str()
        .expect("spawn should return a session ID")
        .to_owned();
    let tasks_before =
        fixture.run_json(&["prime", "--json"])["data"]["tasks"].clone();
    assert!(
        tasks_before
            .as_array()
            .is_some_and(|tasks| tasks.len() == 2)
    );
    assert!(tasks_before.as_array().is_some_and(|tasks| {
        tasks.iter().any(|task| {
            task["id"] == downstream_id
                && task["unresolved_dependencies"][0] == task_id
        })
    }));
    let transcript_before =
        fixture.run_json(&["logs", "worker-1", "--json"])["data"]["transcript"]
            .clone();
    let database = fixture
        .state
        .join("coterie/runs")
        .join(RUN_ID)
        .join("state.sqlite3");
    let connection = rusqlite::Connection::open(&database)
        .expect("the active run database should open");
    let session_id: String = connection
        .query_row(
            "SELECT id FROM sessions WHERE process_owner = 'foreground' \
             ORDER BY created_at DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .expect("the foreground session should be durable");
    let worker_process_id: u32 = connection
        .query_row(
            "SELECT provider_session_id FROM sessions WHERE id = ?1",
            [&worker_session_id],
            |row| row.get::<_, String>(0),
        )
        .expect("the worker process identity should be durable")
        .strip_prefix("process:")
        .and_then(|process_id| process_id.parse().ok())
        .expect("the worker should have a process identity");
    let durable_before = durable_restart_snapshot(&connection);
    assert_eq!(durable_before.tasks.len(), 2);
    assert_eq!(durable_before.dependencies.len(), 1);
    assert_eq!(durable_before.transcript_references.len(), 2);
    assert_eq!(durable_before.operations.len(), 4);
    assert_eq!(durable_before.workspaces.len(), 1);
    drop(connection);

    crashed
        .kill()
        .expect("the owned fixture process should stop");
    crashed.wait().expect("the killed process should be reaped");
    wait_until("the orphaned Codex worker to exit", || {
        !Path::new("/proc")
            .join(worker_process_id.to_string())
            .exists()
    });
    let restart = run(fixture.connect_command());
    assert!(restart.status.success(), "restart failed: {restart:?}");

    let connection = rusqlite::Connection::open(&database)
        .expect("the restarted run database should open");
    assert_eq!(durable_restart_snapshot(&connection), durable_before);
    let lost_sessions: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM sessions \
             WHERE id IN (?1, ?2) AND state = 'lost' \
               AND reconciliation_state = 'lost'",
            [&session_id, &worker_session_id],
            |row| row.get(0),
        )
        .expect("the reconciled sessions should remain durable");
    assert_eq!(lost_sessions, 1);
    let observed_workspaces: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM workspaces WHERE state = 'observed'",
            [],
            |row| row.get(0),
        )
        .expect("the reconciled workspace should remain durable");
    assert_eq!(observed_workspaces, 1);
    let lost_events: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM events \
             WHERE event_type = 'session.reconciliation_changed' \
               AND subject = ?1 \
               AND json_extract(payload_json, '$.data.state') = 'lost'",
            [&worker_session_id],
            |row| row.get(0),
        )
        .expect("the reconciliation event should be queryable");
    assert_eq!(lost_events, 1);
    drop(connection);

    let tasks_after =
        fixture.run_json(&["prime", "--json"])["data"]["tasks"].clone();
    assert_eq!(tasks_after, tasks_before);
    let transcript_after =
        fixture.run_json(&["logs", "worker-1", "--json"])["data"]["transcript"]
            .clone();
    assert_eq!(transcript_after, transcript_before);

    let mut replay = fixture.command();
    replay.args(["--operation-id", "co-01ARZ3NDEKTSV4RRFFQ69G5FB8"]);
    let replay = run(replay);
    assert_eq!(replay.status.code(), Some(5));
    assert!(replay.stdout.is_empty());
    assert!(String::from_utf8_lossy(&replay.stderr).contains("operation"));

    let mut shutdown = fixture.command();
    shutdown.arg("__supervisor-shutdown");
    assert!(run(shutdown).status.success());
    wait_until("restarted supervisor retirement", || {
        fixture.index_entry_count() == 0
    });
}

#[derive(Debug, PartialEq)]
struct DurableRestartSnapshot {
    tasks: Vec<Vec<rusqlite::types::Value>>,
    dependencies: Vec<Vec<rusqlite::types::Value>>,
    transcript_references: Vec<Vec<rusqlite::types::Value>>,
    operations: Vec<Vec<rusqlite::types::Value>>,
    workspaces: Vec<Vec<rusqlite::types::Value>>,
}

fn durable_restart_snapshot(
    connection: &rusqlite::Connection,
) -> DurableRestartSnapshot {
    DurableRestartSnapshot {
        tasks: query_rows(
            connection,
            "SELECT id, run_id, project_id, group_id, title, description, \
                    status, result_json, created_at, updated_at \
             FROM tasks ORDER BY id",
            10,
        ),
        dependencies: query_rows(
            connection,
            "SELECT run_id, task_id, dependency_task_id, created_at \
             FROM task_dependencies ORDER BY task_id, dependency_task_id",
            4,
        ),
        transcript_references: query_rows(
            connection,
            "SELECT id, run_id, agent_id, generation, provider, \
                    provider_session_id, transcript_path, created_at \
             FROM sessions ORDER BY id",
            8,
        ),
        operations: query_rows(
            connection,
            "SELECT id, run_id, kind, actor_agent_id, status, request_json, \
                    result_json, attempt_count, created_at, updated_at \
             FROM operations ORDER BY id",
            10,
        ),
        workspaces: query_rows(
            connection,
            "SELECT assignment_id, run_id, project_id, kind, path, \
                    base_commit, result_commit, target_commit, created_at \
             FROM workspaces ORDER BY assignment_id",
            9,
        ),
    }
}

fn query_rows(
    connection: &rusqlite::Connection,
    sql: &str,
    column_count: usize,
) -> Vec<Vec<rusqlite::types::Value>> {
    let mut statement = connection
        .prepare(sql)
        .expect("the durable snapshot query should prepare");
    statement
        .query_map([], |row| {
            (0..column_count)
                .map(|column| row.get(column))
                .collect::<rusqlite::Result<Vec<_>>>()
        })
        .expect("the durable snapshot query should execute")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("the durable snapshot rows should decode")
}

fn run(mut command: Command) -> std::process::Output {
    command.output().expect("Coterie should execute")
}

fn wait_until(description: &str, mut predicate: impl FnMut() -> bool) {
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

fn commit_file(
    worktree: &Path,
    relative_path: &Path,
    contents: &str,
    message: &str,
) -> String {
    fs::write(worktree.join(relative_path), contents)
        .expect("the worker result should be written");
    let repository =
        Repository::open(worktree).expect("the worker repository should open");
    let mut index = repository.index().expect("the index should open");
    index
        .add_path(relative_path)
        .expect("the worker result should enter the index");
    index.write().expect("the index should be persisted");
    let tree_id = index.write_tree().expect("the tree should be written");
    let tree = repository
        .find_tree(tree_id)
        .expect("the worker result tree should resolve");
    let parent = repository
        .head()
        .and_then(|head| head.peel_to_commit())
        .expect("the worker base commit should resolve");
    let signature = Signature::now("Coterie Worker", "worker@example.invalid")
        .expect("the worker signature should be valid");
    repository
        .commit(
            Some("HEAD"),
            &signature,
            &signature,
            message,
            &tree,
            &[&parent],
        )
        .expect("the worker result commit should be created")
        .to_string()
}

fn assert_repository_clean(project: &Path) {
    let repository =
        Repository::open(project).expect("the fixture repository should open");
    let mut options = StatusOptions::new();
    options.include_untracked(true).recurse_untracked_dirs(true);
    let statuses = repository
        .statuses(Some(&mut options))
        .expect("the fixture repository status should be readable");
    assert!(
        statuses.is_empty(),
        "the fixture repository should be clean, but had {} status entries",
        statuses.len()
    );
}

struct TestEnvironment {
    root: PathBuf,
    runtime: PathBuf,
    state: PathBuf,
    project: PathBuf,
    path: std::ffi::OsString,
}

impl TestEnvironment {
    fn new() -> Self {
        Self::new_with_repository(true)
    }

    fn new_plain() -> Self {
        Self::new_with_repository(false)
    }

    fn new_with_repository(initialize_repository: bool) -> Self {
        let fixture_id =
            format!("{}-{}", std::process::id(), ulid::Ulid::generate());
        let root = std::env::temp_dir().join(format!("ct-{fixture_id}"));
        // The kernel bounds Unix socket paths, so keep this fixture independent
        // of an arbitrarily long `TMPDIR` used for its other files.
        let runtime =
            Path::new("/tmp").join(format!("ct-runtime-{fixture_id}"));
        let state = root.join("state");
        let project = root.join("project");
        let bin = root.join("bin");
        let socket = runtime.join("coterie").join(format!("{RUN_ID}.sock"));
        assert!(
            socket.as_os_str().as_bytes().len() <= 107,
            "the test runtime must support Coterie's Unix socket path: {socket:?}"
        );
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&runtime)
            .expect("the runtime directory should be created");
        fs::create_dir_all(&project)
            .expect("the project directory should be created");
        if initialize_repository {
            let repository = Repository::init(&project)
                .expect("the fixture repository should initialize");
            fs::write(project.join("README.md"), "fixture\n")
                .expect("the fixture file should be written");
            let mut index = repository.index().expect("the index should open");
            index
                .add_path(Path::new("README.md"))
                .expect("the fixture file should enter the index");
            index.write().expect("the index should be persisted");
            let tree_id =
                index.write_tree().expect("the tree should be written");
            let tree = repository
                .find_tree(tree_id)
                .expect("the fixture tree should resolve");
            let signature =
                Signature::now("Coterie Test", "test@example.invalid")
                    .expect("the fixture signature should be valid");
            repository
                .commit(
                    Some("HEAD"),
                    &signature,
                    &signature,
                    "initial",
                    &tree,
                    &[],
                )
                .expect("the initial fixture commit should be created");
            drop(tree);
            drop(index);
            drop(repository);
        }
        fs::create_dir(&bin).expect("the fixture bin directory should exist");
        let codex = bin.join("codex");
        fs::write(&codex, FAKE_CODEX)
            .expect("the fake Codex executable should be written");
        fs::set_permissions(&codex, fs::Permissions::from_mode(0o700))
            .expect("the fake Codex executable should be private");
        std::os::unix::fs::symlink(
            env!("CARGO_BIN_EXE_coterie"),
            bin.join("coterie"),
        )
        .expect("the scripted worker should find the Coterie binary");
        let path = std::env::join_paths(std::iter::once(bin).chain(
            std::env::split_paths(
                &std::env::var_os("PATH").unwrap_or_default(),
            ),
        ))
        .expect("the fixture PATH should be representable");
        Self {
            root,
            runtime,
            state,
            project,
            path,
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_coterie"));
        command
            .current_dir(&self.project)
            .env("XDG_RUNTIME_DIR", &self.runtime)
            .env("XDG_STATE_HOME", &self.state)
            .env("PATH", &self.path);
        command
    }

    fn launch(&self, arguments: &[&str]) -> std::process::Output {
        let mut command = self.command();
        command.args(arguments);
        let output = run(command);
        assert!(
            output.status.success(),
            "foreground launch {arguments:?} failed: {output:?}"
        );
        assert!(
            output.stdout.is_empty() && output.stderr.is_empty(),
            "the foreground wrapper should not write around the TUI: {output:?}"
        );
        output
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
        let mut pending = vec![self.root.clone(), self.runtime.clone()];
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
        for path in [&self.root, &self.runtime] {
            if path.exists() {
                fs::remove_dir_all(path)
                    .expect("the test environment should be removable");
            }
        }
    }
}

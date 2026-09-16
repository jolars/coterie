//! Every traced occurrence is crashed in a separate process, without unwinding.

use super::*;
use crate::fault::injection;
use crate::workspace::GitWorkspace;
use git2::{Repository, Signature};
use std::os::unix::fs::DirBuilderExt;

#[path = "resubmit_tests.rs"]
mod resubmit_tests;

#[path = "recovery_tests.rs"]
mod recovery_tests;

#[path = "git_publication_tests.rs"]
mod git_publication_tests;

const CHILD: &str = "supervisor::crash_tests::crash_child";
const RUN: &str = "cr-01ARZ3NDEKTSV4RRFFQ69G5FAV";
const PROJECT: &str = "cp-01ARZ3NDEKTSV4RRFFQ69G5FAW";
const TASK: &str = "ct-01ARZ3NDEKTSV4RRFFQ69G5FAX";
const OPERATION: &str = "co-01ARZ3NDEKTSV4RRFFQ69G5FAY";
const FIXTURE_TIME_MS: i64 = 1_700_000_000_000;

#[test]
fn crash_matrix_attached_run_publication_and_retirement() {
    matrix("runtime-attachment");
}

#[test]
fn crash_matrix_offline_stop_recovery() {
    matrix("runtime-stop");
}

#[test]
fn crash_matrix_project_attachment() {
    matrix("attachment");
}

#[test]
fn crash_matrix_project_attachment_recovery() {
    recovery_matrix("attachment", "project.attach.record.after");
}

#[test]
fn crash_matrix_initialization() {
    matrix("initialize");
}

#[test]
fn crash_matrix_task_mutation() {
    matrix("task");
}

#[test]
fn crash_matrix_spawn() {
    matrix("spawn");
}

#[test]
fn crash_matrix_finish() {
    matrix("finish");
}

#[test]
fn crash_matrix_resubmit() {
    matrix("resubmit");
}

#[test]
fn crash_matrix_recover_assignment() {
    matrix("recover-assignment");
}

#[test]
fn crash_matrix_continue_assignment() {
    matrix("continue-assignment");
}

#[test]
fn crash_matrix_fast_forward() {
    matrix("integrate");
}

#[test]
fn crash_matrix_merge() {
    matrix("merge");
}

#[test]
fn crash_matrix_rebase() {
    matrix("rebase");
}

#[test]
fn crash_matrix_transcript() {
    matrix("transcript");
}

#[test]
fn crash_matrix_shutdown() {
    matrix("shutdown");
}

#[test]
fn shutdown_trace_does_not_depend_on_wall_clock_delays() {
    let _clock = test_clock::FrozenClock::new(FIXTURE_TIME_MS);
    let traces = [false, true].map(|delayed| {
        let directory = Directory::new();
        prepare_project(&directory.0);
        let mut fixture = Fixture::open(directory.0.clone());
        fixture.prepare("shutdown");
        let policy = fixture
            .store
            .configuration(RUN.parse().unwrap())
            .unwrap()
            .supervision;
        let trace = directory.0.join("trace");
        injection::arm(&trace, None);
        if delayed {
            // Force descheduling between durable intent and control delivery,
            // beyond the real interrupt deadline, without relying on host load.
            injection::delay_once(
                "db.transaction.committed",
                Duration::from_millis(policy.interrupt_grace_ms as u64 + 50),
            );
        }
        fixture.shutdown();
        injection::disarm();
        fs::read_to_string(trace).unwrap()
    });
    assert_eq!(traces[0], traces[1]);
}

#[test]
fn crash_matrix_idle_shutdown() {
    matrix("idle-shutdown");
}

#[test]
fn crash_matrix_message() {
    matrix("message");
}

#[test]
fn crash_matrix_acknowledgment() {
    matrix("acknowledge");
}

#[test]
fn crash_matrix_task_closure() {
    matrix("close");
}

#[test]
fn crash_matrix_external_closure() {
    matrix("external-close");
}

#[test]
fn crash_matrix_runtime_publication_and_retirement() {
    matrix("runtime");
}

#[test]
fn runtime_operator_failure_does_not_strand_supervisor() {
    let fixture = Directory::new();
    child(&fixture.0, "runtime-operator-error", "exercise", None, 101);
    assert!(
        fs::read_to_string(fixture.0.join("child.log"))
            .unwrap()
            .contains("operator failed")
    );
}

#[test]
fn crash_matrix_real_process_spawn() {
    matrix("process");
}

#[test]
fn crash_matrix_process_interrupt() {
    matrix("interrupt-process");
}

#[test]
fn crash_matrix_process_terminate() {
    matrix("terminate-process");
}

#[test]
fn crash_matrix_process_kill() {
    matrix("kill-process");
}

#[test]
fn crash_matrix_process_exit() {
    matrix("exit-process");
}

#[test]
fn crash_matrix_malformed_process_output() {
    matrix("malformed-process");
}

#[test]
fn crash_matrix_foreground_launch() {
    matrix("foreground");
}

#[test]
fn crash_matrix_foreground_exit() {
    matrix("foreground-exit");
}

#[test]
fn foreground_exit_barrier_waits_for_process_absence() {
    let fixture = Directory::new();
    let executable = fixture.0.join("codex");
    fs::write(&executable, PROCESS_PROVIDER).unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700))
        .unwrap();
    let scope = SessionScope {
        run_id: RUN.parse().unwrap(),
        agent_id: AgentId::generate(),
        session_id: SessionId::generate(),
        generation: 0,
    };
    let mut process = Command::new(&executable)
        .arg("fixture")
        .env("COTERIE_SESSION_ID", scope.session_id.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_for_file(&fixture.0.join("codex.launches"));
    let provider_id = format!("process:{}", process.id());
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (finished_tx, finished_rx) = std::sync::mpsc::channel();
    std::thread::scope(|threads| {
        threads.spawn(|| {
            started_tx.send(()).unwrap();
            wait_for_foreground_process_absence(&provider_id, scope);
            finished_tx.send(()).unwrap();
        });
        started_rx.recv().unwrap();
        let live = finished_rx.recv_timeout(Duration::from_millis(100));
        fs::write(fixture.0.join("codex.release"), "").unwrap();
        wait_for_file(&fixture.0.join("codex.exited"));
        let unreaped = finished_rx.recv_timeout(Duration::from_millis(100));
        // The exit marker is insufficient: the adapter still sees an unreaped
        // child. Reap it even if the barrier incorrectly returns early.
        process.wait().unwrap();
        assert!(matches!(
            live,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ));
        assert!(matches!(
            unreaped,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ));
        finished_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    });
}

#[test]
fn crash_matrix_foreground_notification() {
    matrix("foreground-notification");
}

#[test]
fn crash_matrix_workspace_recovery() {
    recovery_matrix("spawn", "workspace.reference.after");
}

#[test]
fn crash_matrix_session_recovery() {
    recovery_matrix("spawn", "session.launch.after");
}

#[test]
fn crash_matrix_integration_recovery() {
    recovery_matrix("merge", "integration.checkout.after");
}

#[test]
fn crash_matrix_rebase_recovery() {
    recovery_matrix("rebase", "integration.rebase.commit.after");
}

#[test]
fn crash_matrix_runtime_recovery() {
    recovery_matrix("runtime", "socket.permissions.after");
}

#[test]
fn crash_matrix_covers_all_declared_boundaries() {
    let mut declared = std::collections::BTreeSet::new();
    for source in [
        include_str!("../state.rs"),
        include_str!("../state/recovery.rs"),
        include_str!("../supervisor.rs"),
        include_str!("session.rs"),
        include_str!("projects.rs"),
        include_str!("idle.rs"),
        include_str!("notifications.rs"),
        include_str!("recovery.rs"),
        include_str!("../providers.rs"),
        include_str!("../providers/notifications.rs"),
        include_str!("../project.rs"),
        include_str!("../private_fs.rs"),
        include_str!("../workspace.rs"),
        include_str!("../transcript.rs"),
    ] {
        for suffix in source.split("crate::fault::point(").skip(1) {
            declared.insert(
                suffix
                    .trim_start()
                    .strip_prefix('"')
                    .unwrap()
                    .split('"')
                    .next()
                    .unwrap()
                    .to_owned(),
            );
        }
    }
    let mut covered = std::collections::BTreeSet::new();
    for case in [
        "initialize",
        "attachment",
        "task",
        "spawn",
        "finish",
        "resubmit",
        "recover-assignment",
        "continue-assignment",
        "integrate",
        "merge",
        "rebase",
        "transcript",
        "shutdown",
        "idle-shutdown",
        "message",
        "acknowledge",
        "close",
        "external-close",
        "runtime",
        "runtime-attachment",
        "runtime-stop",
        "process",
        "interrupt-process",
        "terminate-process",
        "kill-process",
        "exit-process",
        "malformed-process",
        "foreground",
        "foreground-exit",
        "foreground-notification",
    ] {
        let fixture = Directory::new();
        child(&fixture.0, case, "exercise", None, 0);
        let trace = fs::read_to_string(fixture.0.join("trace")).unwrap();
        covered.extend(trace.lines().map(str::to_owned));
        if case == "runtime" {
            let index = trace
                .lines()
                .position(|point| point == "socket.permissions.after")
                .unwrap();
            let fixture = Directory::new();
            child(
                &fixture.0,
                case,
                "exercise",
                Some(index),
                injection::CRASH_EXIT,
            );
            child(&fixture.0, case, "recover-trace", None, 0);
            covered.extend(
                fs::read_to_string(fixture.0.join("recovery.trace"))
                    .unwrap()
                    .lines()
                    .map(str::to_owned),
            );
        }
    }
    assert_eq!(
        covered, declared,
        "every declared boundary needs a crash scenario"
    );
}

fn recovery_matrix(case: &str, seed: &str) {
    let baseline = Directory::new();
    child(&baseline.0, case, "exercise", None, 0);
    let trace = fs::read_to_string(baseline.0.join("trace")).unwrap();
    let seed_index = trace
        .lines()
        .position(|point| point == seed)
        .expect("the seed boundary must be exercised");
    let baseline = Directory::new();
    child(
        &baseline.0,
        case,
        "exercise",
        Some(seed_index),
        injection::CRASH_EXIT,
    );
    child(&baseline.0, case, "recover-trace", None, 0);
    let trace = fs::read_to_string(baseline.0.join("recovery.trace")).unwrap();
    let boundaries: Vec<_> = trace.lines().collect();
    assert!(!boundaries.is_empty());
    for (index, name) in boundaries.iter().enumerate() {
        let fixture = Directory::new();
        child(
            &fixture.0,
            case,
            "exercise",
            Some(seed_index),
            injection::CRASH_EXIT,
        );
        child(
            &fixture.0,
            case,
            "recover-trace",
            Some(index),
            injection::CRASH_EXIT,
        );
        assert_eq!(
            fs::read_to_string(fixture.0.join("recovery.trace"))
                .unwrap()
                .lines()
                .collect::<Vec<_>>(),
            boundaries[..=index]
        );
        child(&fixture.0, case, "recover", None, 0);
        let before = snapshot(&fixture.0);
        child(&fixture.0, case, "recover", None, 0);
        assert!(
            snapshot(&fixture.0) == before,
            "{case} recovery diverged after {seed}, then {index}: {name}"
        );
    }
    eprintln!(
        "{case} recovery after {seed}: {} crash boundaries",
        boundaries.len()
    );
}

fn matrix(case: &str) {
    let baseline = Directory::new();
    child(&baseline.0, case, "exercise", None, 0);
    let trace = fs::read_to_string(baseline.0.join("trace")).unwrap();
    let boundaries: Vec<_> = trace.lines().collect();
    assert!(
        !boundaries.is_empty(),
        "{case} must exercise fault boundaries"
    );
    for (index, name) in boundaries.iter().enumerate() {
        let fixture = Directory::new();
        child(
            &fixture.0,
            case,
            "exercise",
            Some(index),
            injection::CRASH_EXIT,
        );
        let reached = fs::read_to_string(fixture.0.join("trace")).unwrap();
        assert_eq!(
            reached.lines().collect::<Vec<_>>(),
            boundaries[..=index],
            "{case} must reach the same boundary {index}: {name}"
        );
        child(&fixture.0, case, "recover", None, 0);
        let before = snapshot(&fixture.0);
        child(&fixture.0, case, "recover", None, 0);
        let after = snapshot(&fixture.0);
        for (table, rows) in &before.database {
            if after.database.get(table) != Some(rows) {
                let new_rows = after.database.get(table).unwrap();
                let changed: Vec<_> = rows
                    .iter()
                    .zip(new_rows)
                    .enumerate()
                    .filter(|(_, (a, b))| a != b)
                    .collect();
                panic!(
                    "{case} boundary {index} ({name}): table {table}, rows {} -> {}, changes {changed:?}",
                    rows.len(),
                    new_rows.len()
                );
            }
        }
        let changed_files: Vec<_> = before
            .files
            .keys()
            .chain(after.files.keys())
            .filter(|path| before.files.get(*path) != after.files.get(*path))
            .collect();
        assert!(
            changed_files.is_empty(),
            "{case} boundary {index} ({name}): files changed: {changed_files:?}"
        );
    }
    eprintln!(
        "{case}: {} crash boundaries recovered twice",
        boundaries.len()
    );
}

fn child(root: &Path, case: &str, mode: &str, index: Option<usize>, code: i32) {
    let log = fs::File::create(root.join("child.log")).unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", CHILD, "--nocapture"])
        .env("COTERIE_CRASH_TEST_ROOT", root)
        .env("COTERIE_CRASH_TEST_CASE", case)
        .env("COTERIE_CRASH_TEST_MODE", mode)
        .env("XDG_RUNTIME_DIR", root.join("rt"))
        .env("XDG_STATE_HOME", root.join("state"))
        .env(
            "COTERIE_CRASH_TEST_INDEX",
            index.map_or_else(String::new, |i| i.to_string()),
        )
        .stdout(log.try_clone().unwrap())
        .stderr(log);
    let mut child = command.spawn().unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if std::time::Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!(
                "{case} {mode} {index:?} timed out: {}\nFault trace:\n{}",
                fs::read_to_string(root.join("child.log")).unwrap(),
                fs::read_to_string(root.join("trace")).unwrap_or_default()
            );
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    assert_eq!(
        status.code(),
        Some(code),
        "{case} {mode} {index:?}: {}",
        fs::read_to_string(root.join("child.log")).unwrap()
    );
}

#[test]
fn crash_child() {
    let Some(root) = std::env::var_os("COTERIE_CRASH_TEST_ROOT") else {
        return;
    };
    let _clock = test_clock::FrozenClock::new(FIXTURE_TIME_MS);
    let root = PathBuf::from(root);
    let case = std::env::var("COTERIE_CRASH_TEST_CASE").unwrap();
    let mode = std::env::var("COTERIE_CRASH_TEST_MODE").unwrap();
    if mode == "recover-trace" {
        injection::arm(
            &root.join("recovery.trace"),
            std::env::var("COTERIE_CRASH_TEST_INDEX")
                .unwrap()
                .parse()
                .ok(),
        );
    }
    if case.starts_with("foreground") {
        foreground_child(root, &mode, &case);
        return;
    }
    if case == "process" || case.ends_with("-process") {
        process_child(root, &mode, &case);
        return;
    }
    if case == "attachment" {
        attachment_child(&root, &mode);
        return;
    }
    if case.starts_with("runtime") {
        runtime_child(&root, &mode, &case);
        return;
    }
    if mode.starts_with("recover") {
        let mut fixture = Fixture::open(root);
        fixture.recover(&case);
        injection::disarm();
        fixture.verify(&case);
    } else {
        let index = std::env::var("COTERIE_CRASH_TEST_INDEX")
            .unwrap()
            .parse()
            .ok();
        if case == "initialize" {
            prepare_project(&root);
            injection::arm(&root.join("trace"), index);
            let mut fixture = Fixture::open(root);
            injection::disarm();
            fixture.verify(&case);
        } else {
            prepare_project(&root);
            let mut fixture = Fixture::open(root.clone());
            fixture.prepare(&case);
            injection::arm(&root.join("trace"), index);
            fixture.exercise(&case);
            injection::disarm();
        }
    }
}

fn foreground_child(root: PathBuf, mode: &str, case: &str) {
    if mode == "exercise" {
        prepare_project(&root);
        fs::write(root.join("codex"), PROCESS_PROVIDER).unwrap();
        fs::set_permissions(
            root.join("codex"),
            fs::Permissions::from_mode(0o700),
        )
        .unwrap();
    }
    let mut fixture = Fixture::open(root.clone());
    if mode == "exercise" {
        let boundary = std::env::var("COTERIE_CRASH_TEST_INDEX")
            .unwrap()
            .parse()
            .ok();
        let token = AgentToken::generate().unwrap();
        if case == "foreground" {
            injection::arm(&root.join("trace"), boundary);
        }
        let RpcResponse::ForegroundPrepared {
            run_id,
            agent,
            session_id,
            generation,
            bootstrap_instruction,
        } = launch_foreground(
            &mut fixture.store,
            RUN.parse().unwrap(),
            &AuthenticatedCaller::Operator,
            OPERATION.parse().unwrap(),
            &token,
        )
        .unwrap()
        else {
            panic!("expected a foreground launch")
        };
        let scope = SessionScope {
            run_id,
            agent_id: agent.id,
            session_id,
            generation,
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let mut provider = crate::providers::CodexProvider::new([root
                .join("codex")
                .into_os_string()]);
            let specification = LaunchSpecification {
                scope,
                working_directory: root.join("project"),
                permission_profile: crate::config::builtin_standard()
                    .permission_profiles["interactive"],
                bootstrap_instruction,
            };
            let environment = InteractiveEnvironment {
                project_id: PROJECT.parse().unwrap(),
                primary_project_root: root.join("project"),
                role: agent.role,
                socket_path: fixture.socket.clone(),
                token,
            };
            let handle = provider
                .launch_interactive(&specification, Some(&environment))
                .unwrap();
            observe_foreground_started(
                &mut fixture.store,
                run_id,
                &AuthenticatedCaller::Operator,
                scope,
                provider.foreground_process_id(&handle).unwrap(),
                provider.foreground_process_identity(&handle).as_ref(),
            )
            .unwrap();
            if case == "foreground-exit" {
                wait_for_file(&root.join("codex.launches"));
                fs::write(root.join("codex.release"), "").unwrap();
                injection::arm(&root.join("trace"), boundary);
                let (status, _) = provider
                    .wait_foreground_until_termination(
                        &handle,
                        std::future::pending(),
                    )
                    .await
                    .unwrap();
                observe_foreground_ended(
                    &mut fixture.store,
                    run_id,
                    &AuthenticatedCaller::Operator,
                    scope,
                    ForegroundEnd::Exited {
                        code: status.code(),
                        signal: status.signal(),
                    },
                )
                .unwrap();
            }
            if case == "foreground-notification" {
                let queue = crate::providers::notifications::CodexQueue::new(
                    &[root.join("codex").to_str().unwrap().into()],
                    &root.join("project"),
                    provider.foreground_process_identity(&handle).unwrap(),
                );
                fixture
                    .store
                    .transaction(|r| {
                        r.enable_notifications(scope)?;
                        assert!(r.bind_notifications(
                            scope,
                            "01234567-89ab-cdef-0123-456789abcdef"
                        )?);
                        Ok(())
                    })
                    .unwrap();
                send_message(
                    &mut fixture.store,
                    run_id,
                    &AuthenticatedCaller::Operator,
                    OperationId::generate(),
                    agent.id.to_string(),
                    "Private report.".into(),
                    None,
                )
                .unwrap();
                injection::arm(&root.join("trace"), boundary);
                let RpcResponse::ForegroundNotificationClaimed {
                    claim: Some(claim),
                } = notifications::execute(
                    &mut fixture.store,
                    run_id,
                    &AuthenticatedCaller::Operator,
                    RpcRequest::ClaimForegroundNotification {
                        operation_id: OperationId::generate(),
                        scope,
                    },
                    None,
                )
                .unwrap()
                else {
                    panic!("expected a notification claim");
                };
                let observed =
                    notifications::attempt(&queue, scope, &claim).await;
                notifications::execute(
                    &mut fixture.store,
                    run_id,
                    &AuthenticatedCaller::Operator,
                    observed,
                    None,
                )
                .unwrap();
            }
            injection::disarm();
        });
    } else {
        let now = unix_timestamp().unwrap();
        if case == "foreground-exit" {
            // A crash before wait() leaves the released provider exiting
            // asynchronously. Compare recovery passes only after the adapter
            // can prove the same external state for both of them.
            let sessions = fixture
                .store
                .transaction(|r| r.sessions(RUN.parse().unwrap()))
                .unwrap();
            for session in sessions
                .into_iter()
                .filter(|session| !session.state.is_terminal())
            {
                wait_for_foreground_process_absence(
                    session
                        .provider_session_id
                        .as_deref()
                        .expect("the exit fixture recorded startup"),
                    SessionScope {
                        run_id: session.run_id,
                        agent_id: session.agent_id,
                        session_id: session.id,
                        generation: session.generation,
                    },
                );
            }
        }
        let mut foreground_sessions = AgentSessionSupervisor::new(
            crate::providers::CodexProvider::new([root
                .join("codex")
                .into_os_string()]),
            &fixture.run,
        );
        foreground_sessions
            .reconcile_after_restart(
                &mut fixture.store,
                RUN.parse().unwrap(),
                now,
            )
            .unwrap();
        reconcile_operations(&mut fixture.runtime(), now).unwrap();
        let sessions = fixture
            .store
            .transaction(|r| r.sessions(RUN.parse().unwrap()))
            .unwrap();
        assert!(sessions.len() <= 1);
        for session in sessions {
            assert!(matches!(
                session.state,
                LifecycleState::Unknown
                    | LifecycleState::Exited
                    | LifecycleState::Lost
            ));
            if session.state == LifecycleState::Lost {
                assert_eq!(
                    session.reconciliation_state,
                    ExternalResourceState::Lost
                );
            }
            assert_eq!(session.process_owner, SessionProcessOwner::Foreground);
            if case == "foreground-notification" {
                let scope = SessionScope {
                    run_id: session.run_id,
                    agent_id: session.agent_id,
                    session_id: session.id,
                    generation: session.generation,
                };
                assert!(
                    !fixture
                        .store
                        .transaction(|r| r.notification_pending(scope, true))
                        .unwrap()
                );
                assert!(
                    fixture
                        .store
                        .transaction(|r| r.claim_notification(
                            scope,
                            OperationId::generate(),
                            true,
                            now
                        ))
                        .unwrap()
                        .is_none()
                );
            }
        }
        let before = fixture
            .store
            .transaction(|r| r.sessions(RUN.parse().unwrap()))
            .unwrap();
        if !before.is_empty() {
            assert!(
                launch_foreground(
                    &mut fixture.store,
                    RUN.parse().unwrap(),
                    &AuthenticatedCaller::Operator,
                    OPERATION.parse().unwrap(),
                    &AgentToken::generate().unwrap()
                )
                .is_err()
            );
            assert_eq!(
                fixture
                    .store
                    .transaction(|r| r.sessions(RUN.parse().unwrap()))
                    .unwrap(),
                before
            );
        }
    }
    let trace = fs::read_to_string(root.join("trace")).unwrap();
    if trace.contains("process.foreground.spawn.after")
        || case == "foreground-exit"
    {
        wait_for_file(&root.join("codex.launches"));
        assert_eq!(
            fs::read_to_string(root.join("codex.launches"))
                .unwrap()
                .lines()
                .count(),
            1
        );
    }
    assert_eq!(
        fs::read_to_string(root.join("project/AGENTS.md")).unwrap(),
        "Preserve project instructions.\n"
    );
    if case == "foreground-notification" {
        if trace.contains("notification.queue.spawned") {
            wait_for_file(&root.join("codex.queued"));
        }
        let queued =
            fs::read_to_string(root.join("codex.queued")).unwrap_or_default();
        assert!(
            queued.lines().count() <= 1,
            "recovery duplicated the provider effect"
        );
    }
}

// This executable exercises the actual Codex process adapter without a model,
// network, or credentials. Its independent ledger detects repeated launches.
const PROCESS_PROVIDER: &str = r#"#!/bin/sh
if [ "$1" = "queue" ]; then printf 'queued\n' >> "$0.queued"; exit 0; fi
if [ "${3-}" = "mcp" ] && [ "${4-}" = "get" ]; then printf '%s\n' '{"enabled":true,"transport":{"type":"stdio","command":"coterie","args":["__mcp"],"env_vars":["COTERIE_TOKEN"]}}'; exit 0; fi
if [ "$1" = "--version" ]; then printf 'codex-cli 0.153.4\n'; exit 0; fi
if [ "$1" = "--help" ] || [ "${2-}" = "--help" ]; then
  printf '%s\n' 'Usage: codex exec [OPTIONS] [PROMPT]' '--config <key=value>' '--cd <DIR>' '--sandbox <SANDBOX_MODE>' '--ask-for-approval <APPROVAL_POLICY>' '--json'
  exit 0
fi
printf '%s %s\n' "$$" "$COTERIE_SESSION_ID" >> "$0.launches"
trap 'printf "INT\n" >> "$0.signals"' INT
trap 'printf "TERM\n" >> "$0.signals"' TERM
while [ ! -e "$0.release" ]; do
  if [ -e "$0.malformed" ]; then
    printf 'not-json\n'
    while [ ! -e "$0.release" ]; do sleep 0.01; done
  fi
  sleep 0.01
done
printf 'done\n' > "$0.exited"
"#;

fn process_child(root: PathBuf, mode: &str, case: &str) {
    if mode == "exercise" {
        prepare_project(&root);
        fs::write(root.join("codex"), PROCESS_PROVIDER).unwrap();
        fs::set_permissions(
            root.join("codex"),
            fs::Permissions::from_mode(0o700),
        )
        .unwrap();
    }
    let mut fixture = Fixture::open(root.clone());
    let mut sessions = AgentSessionSupervisor::new(
        crate::providers::CodexProvider::new([root
            .join("codex")
            .into_os_string()]),
        &fixture.run,
    );
    if mode == "exercise" {
        fixture.prepare("spawn");
        let boundary = std::env::var("COTERIE_CRASH_TEST_INDEX")
            .unwrap()
            .parse()
            .ok();
        if case == "process" {
            injection::arm(&root.join("trace"), boundary);
        }
        spawn_agent(
            SpawnRuntime {
                store: &mut fixture.store,
                sessions: &mut sessions,
                workspaces: &mut fixture.workspaces,
                run_state_directory: &fixture.run,
                socket_path: &fixture.socket,
                run_id: RUN.parse().unwrap(),
            },
            &AuthenticatedCaller::Operator,
            OPERATION.parse().unwrap(),
            "worker".to_owned(),
            TASK.parse().unwrap(),
        )
        .unwrap();
        if matches!(case, "exit-process" | "malformed-process") {
            wait_for_file(&root.join("codex.launches"));
            let scope = fixture.scope();
            let trigger = if case == "exit-process" {
                "codex.release"
            } else {
                "codex.malformed"
            };
            fs::write(root.join(trigger), "").unwrap();
            injection::arm(&root.join("trace"), boundary);
            injection::ignore_empty_poll_boundary();
            let deadline = std::time::Instant::now() + Duration::from_secs(2);
            loop {
                assert!(std::time::Instant::now() < deadline);
                if let Some(event) = sessions
                    .advance(
                        &mut fixture.store,
                        scope.session_id,
                        unix_timestamp().unwrap(),
                    )
                    .unwrap()
                    && (matches!(event.kind, crate::providers::ProviderEventKind::Observation(observation) if observation.lifecycle.is_terminal())
                        || matches!(event.kind, crate::providers::ProviderEventKind::MalformedOutput { .. }))
                {
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        } else if case != "process" {
            wait_for_file(&root.join("codex.launches"));
            let scope = fixture.scope();
            let now = unix_timestamp_ms().unwrap();
            fixture.store.transaction(|r| r.request_session_control(scope, crate::state::supervision::ControlReason::ExecutionTimeout, now, 250, 5000)).unwrap();
            injection::arm(&root.join("trace"), boundary);
            let offset = match case {
                "interrupt-process" => 0,
                "terminate-process" => 250,
                "kill-process" => 2500,
                _ => unreachable!(),
            };
            sessions
                .drive_controls(
                    &mut fixture.store,
                    RUN.parse().unwrap(),
                    now + offset,
                )
                .unwrap();
        }
        injection::disarm();
    } else {
        let now = unix_timestamp().unwrap();
        fixture
            .workspaces
            .reconcile_after_restart(
                &mut fixture.store,
                RUN.parse().unwrap(),
                now,
            )
            .unwrap();
        sessions
            .reconcile_after_restart(
                &mut fixture.store,
                RUN.parse().unwrap(),
                now,
            )
            .unwrap();
        let absent = fixture
            .store
            .transaction(|r| r.operation(OPERATION.parse().unwrap()))
            .unwrap()
            .is_none();
        let mut runtime = SpawnRuntime {
            store: &mut fixture.store,
            sessions: &mut sessions,
            workspaces: &mut fixture.workspaces,
            run_state_directory: &fixture.run,
            socket_path: &fixture.socket,
            run_id: RUN.parse().unwrap(),
        };
        reconcile_operations(&mut runtime, now).unwrap();
        if absent {
            spawn_agent(
                runtime,
                &AuthenticatedCaller::Operator,
                OPERATION.parse().unwrap(),
                "worker".to_owned(),
                TASK.parse().unwrap(),
            )
            .unwrap();
        }
        // The recovery subprocess also goes away. Classify the still-running
        // process using a fresh adapter that cannot assert ownership of it.
        let mut replacement = AgentSessionSupervisor::new(
            crate::providers::CodexProvider::new([root
                .join("codex")
                .into_os_string()]),
            &fixture.run,
        );
        replacement
            .reconcile_after_restart(
                &mut fixture.store,
                RUN.parse().unwrap(),
                now,
            )
            .unwrap();
        reconcile_operations(
            &mut SpawnRuntime {
                store: &mut fixture.store,
                sessions: &mut replacement,
                workspaces: &mut fixture.workspaces,
                run_state_directory: &fixture.run,
                socket_path: &fixture.socket,
                run_id: RUN.parse().unwrap(),
            },
            now,
        )
        .unwrap();
        fixture.verify("process");
    }
    let session = fixture
        .store
        .transaction(|r| Ok(r.sessions(RUN.parse().unwrap())?.remove(0)))
        .unwrap();
    if session.provider_session_id.is_some()
        || root.join("trace").exists()
            && fs::read_to_string(root.join("trace"))
                .unwrap()
                .contains("process.job.spawn.after")
    {
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while !root.join("codex.launches").exists() {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        let ledger = fs::read_to_string(root.join("codex.launches")).unwrap();
        assert_eq!(
            ledger.lines().count(),
            1,
            "one durable intent must execute the provider at most once"
        );
        assert!(ledger.contains(&session.id.to_string()));
    }
    if fs::read_to_string(root.join("trace"))
        .unwrap()
        .contains("process.signal.after")
    {
        wait_for_file(&root.join("codex.signals"));
        assert_eq!(
            fs::read_to_string(root.join("codex.signals"))
                .unwrap()
                .lines()
                .count(),
            1
        );
    }
}

fn wait_for_foreground_process_absence(provider_id: &str, scope: SessionScope) {
    let provider = crate::providers::CodexProvider::new(["codex"]);
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        if matches!(
            provider.recover(provider_id, scope).unwrap(),
            crate::providers::ProviderRecovery::Lost
        ) {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for foreground fixture {provider_id} to disappear"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn wait_for_file(path: &Path) {
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while fs::metadata(path).map_or(true, |metadata| metadata.len() == 0) {
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for {}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn attachment_child(root: &Path, mode: &str) {
    if mode == "exercise" {
        prepare_project(root);
        Repository::init(root.join("library")).unwrap();
    }
    crate::private_fs::directory(&root.join("rt")).unwrap();
    let mut fixture = Fixture::open(root.to_owned());
    let project = DiscoveredProject::discover(root.join("project")).unwrap();
    let active = ActiveRunEntry::new(
        RUN.parse().unwrap(),
        PROJECT.parse().unwrap(),
        project.identity.clone(),
    );
    let directories = CoterieDirectories::from_environment().unwrap();
    directories.prepare().unwrap();
    let LeaseAttempt::Acquired(lease) = ProjectLease::try_acquire(
        &directories,
        &project.identity,
        active.run_id,
    )
    .unwrap() else {
        panic!("primary lease unavailable");
    };
    let mut projects = projects::AttachedProjects::new(
        directories.clone(),
        active.clone(),
        lease,
    );
    if mode == "exercise" {
        injection::arm(
            &root.join("trace"),
            std::env::var("COTERIE_CRASH_TEST_INDEX")
                .unwrap()
                .parse()
                .ok(),
        );
    } else {
        projects
            .recover(&mut fixture.store, active.run_id, false)
            .unwrap();
    }
    projects
        .attach(
            &mut fixture.store,
            active.run_id,
            &AuthenticatedCaller::Operator,
            RpcRequest::ProjectAttach {
                operation_id: OPERATION.parse().unwrap(),
                path: root.join("library"),
                alias: Some("library".into()),
            },
        )
        .unwrap();
    injection::disarm();
    let stored = fixture
        .store
        .transaction(|repositories| repositories.projects(active.run_id))
        .unwrap();
    assert_eq!(stored.len(), 2);
    let library = DiscoveredProject::discover(root.join("library")).unwrap();
    let entry = ActiveRunIndex::new(&directories)
        .lookup(&library.identity)
        .unwrap()
        .unwrap();
    assert_eq!(entry.run_id, active.run_id);
    assert!(stored.iter().any(|project| project.id == entry.project_id
        && project.identity == entry.project_identity));
}

async fn coordinate_runtime(
    server: impl std::future::Future<Output = Result<(), SupervisorError>>,
    operator: impl std::future::Future<Output = Result<(), SupervisorError>>
    + Send
    + 'static,
) -> Result<(), SupervisorError> {
    let (cancel, canceled) = oneshot::channel();
    // Blocking work inhibits Tokio's automatic clock advancement while the
    // separate operator thread keeps its filesystem reads off the fault plan.
    let mut operator = tokio::task::spawn_blocking(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                tokio::select! {
                    biased;
                    _ = canceled => Ok(()),
                    result = operator => result,
                }
            })
    });
    tokio::pin!(server);
    let mut operator_finished = false;
    let result = tokio::select! {
        biased;
        result = &mut server => result,
        result = &mut operator => {
            operator_finished = true;
            result.expect("operator panicked").expect("operator failed");
            server.await
        }
    };
    // Recovery can retire an already stopped run before the operator connects.
    // Cancel in-flight RPCs as well as startup retries, then join the thread.
    let _disconnected = cancel.send(());
    if !operator_finished {
        operator
            .await
            .expect("operator panicked")
            .expect("operator failed");
    }
    result
}

#[tokio::test(start_paused = true)]
async fn runtime_retirement_cancels_a_waiting_operator() {
    let (started, ready) = oneshot::channel();
    let result = coordinate_runtime(
        async {
            ready.await.unwrap();
            Err(SupervisorError::InvalidProof)
        },
        async move {
            started.send(()).unwrap();
            std::future::pending().await
        },
    )
    .await;
    assert!(matches!(result, Err(SupervisorError::InvalidProof)));
}

#[tokio::test(start_paused = true)]
async fn runtime_clock_waits_for_operator_io() {
    let (sent, received) = oneshot::channel();
    let (release, released) = oneshot::channel();
    let start = Instant::now();
    coordinate_runtime(
        async {
            tokio::time::timeout(Duration::from_millis(1), received)
                .await
                .expect("operator I/O must not advance the supervisor clock")
                .unwrap();
            assert_eq!(Instant::now(), start);
            release.send(()).unwrap();
            Ok(())
        },
        async move {
            std::thread::sleep(Duration::from_millis(20));
            sent.send(()).unwrap();
            let _retired = released.await;
            Ok(())
        },
    )
    .await
    .unwrap();
}

#[tokio::test(start_paused = true)]
async fn runtime_operator_completion_still_waits_for_retirement() {
    let (sent, received) = oneshot::channel();
    let result = coordinate_runtime(
        async {
            received.await.unwrap();
            tokio::task::yield_now().await;
            Err(SupervisorError::InvalidProof)
        },
        async move {
            sent.send(()).unwrap();
            Ok(())
        },
    )
    .await;
    assert!(matches!(result, Err(SupervisorError::InvalidProof)));
}

fn runtime_child(root: &Path, mode: &str, case: &str) {
    let attach = case != "runtime" && case != "runtime-stop";
    let stopping = case == "runtime-stop";
    if mode == "exercise" {
        prepare_project(root);
        if case == "runtime-attachment" {
            Repository::init(root.join("library")).unwrap();
        }
    }
    crate::private_fs::directory(&root.join("rt")).unwrap();
    let project = DiscoveredProject::discover(root.join("project")).unwrap();
    let active = ActiveRunEntry::new(
        RUN.parse().unwrap(),
        PROJECT.parse().unwrap(),
        project.identity.clone(),
    );
    let directories = CoterieDirectories::from_environment().unwrap();
    if stopping {
        if mode == "exercise" {
            let run = directories.prepare_run(active.run_id).unwrap();
            drop(initialize_store(&run.state, &active, &project).unwrap());
            let LeaseAttempt::Acquired(lease) = ProjectLease::try_acquire(
                &directories,
                &project.identity,
                active.run_id,
            )
            .unwrap() else {
                panic!("fixture lease")
            };
            ActiveRunIndex::new(&directories)
                .publish(&active, &lease)
                .unwrap();
        } else if ActiveRunIndex::new(&directories)
            .lookup(&project.identity)
            .unwrap()
            .is_none()
        {
            let mut store =
                open_configuration_store(&directories, active.run_id).unwrap();
            assert_eq!(
                store
                    .transaction(|r| r.run(active.run_id))
                    .unwrap()
                    .unwrap()
                    .status,
                "stopped"
            );
            return;
        }
    }
    if mode == "exercise" {
        let index = std::env::var("COTERIE_CRASH_TEST_INDEX")
            .unwrap()
            .parse()
            .ok();
        injection::arm(&root.join("trace"), index);
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .start_paused(true)
        .build()
        .unwrap();
    let socket = directories.socket_path(active.run_id);
    let operator_entry = active.clone();
    let library = root.join("library");
    let operator = async move {
        // The typed handshake establishes readiness. The parent watchdog bounds
        // the whole scenario, including retries and RPCs, under host load.
        let mut client = loop {
            if let Ok(client) = SupervisorClient::connect_with_unbounded(
                &socket,
                &operator_entry,
                ConnectionChannel::Operator,
                RequestAuthentication::Operator,
            )
            .await
            {
                break client;
            }
            sleep(Duration::from_millis(5)).await;
        };
        if attach {
            client
                .request_unbounded(RpcRequest::ProjectAttach {
                    operation_id: "co-01ARZ3NDEKTSV4RRFFQ69G5FAZ"
                        .parse()
                        .unwrap(),
                    path: library,
                    alias: Some("library".into()),
                })
                .await?;
        }
        client
            .request_unbounded(RpcRequest::Shutdown {
                operation_id: OPERATION.parse().unwrap(),
            })
            .await?;
        Ok(())
    };
    let result = runtime.block_on(coordinate_runtime(
        async {
            if stopping {
                serve_with_overrides(
                    active.clone(),
                    project.clone(),
                    directories.clone(),
                    &crate::cli::config::Overrides::default(),
                    Some(OPERATION.parse().unwrap()),
                )
                .await
            } else {
                serve(active.clone(), project.clone(), directories.clone())
                    .await
            }
        },
        operator,
    ));
    injection::disarm();
    if let Err(SupervisorError::SocketIo {
        action: "validate stale",
        path,
        source,
    }) = &result
    {
        assert!(mode.starts_with("recover"));
        assert_eq!(source.kind(), io::ErrorKind::PermissionDenied);
        assert!(fs::symlink_metadata(path).unwrap().file_type().is_socket());
        assert!(source.to_string().contains("0600"));
        // A crash before chmod leaves an unverifiable socket. Recovery must
        // report it and preserve it, rather than weakening private-file checks.
        return;
    }
    result.unwrap();
    assert!(
        ActiveRunIndex::new(&directories)
            .lookup(&project.identity)
            .unwrap()
            .is_none()
    );
    assert!(!directories.socket_path(active.run_id).exists());
    assert!(matches!(
        ProjectLease::try_acquire(
            &directories,
            &project.identity,
            active.run_id
        )
        .unwrap(),
        LeaseAttempt::Acquired(_)
    ));
    let run = directories.runs.join(RUN);
    let mut store = initialize_store(&run, &active, &project).unwrap();
    assert_eq!(
        store
            .transaction(|r| r.run(active.run_id))
            .unwrap()
            .unwrap()
            .status,
        "stopped"
    );
}

struct Fixture {
    root: PathBuf,
    run: PathBuf,
    socket: PathBuf,
    store: Store,
    sessions: AgentSessionSupervisor<FakeProvider>,
    workspaces: WorkspaceSupervisor<GitWorkspace>,
}

impl Fixture {
    fn open(root: PathBuf) -> Self {
        let project =
            DiscoveredProject::discover(root.join("project")).unwrap();
        let active = ActiveRunEntry::new(
            RUN.parse().unwrap(),
            PROJECT.parse().unwrap(),
            project.identity.clone(),
        );
        let run = root.join("run");
        crate::private_fs::directory(&run).unwrap();
        let mut configuration = crate::config::resolve(
            &Default::default(),
            &Default::default(),
            &Default::default(),
        )
        .unwrap();
        if root.join("codex").exists() {
            configuration.providers.get_mut("codex").unwrap().command =
                vec![root.join("codex").to_str().unwrap().into()];
        }
        let store = initialize_store_with_configuration(
            &run,
            &active,
            &project,
            &configuration,
        )
        .unwrap();
        let sessions = runtime_sessions(&run);
        let workspaces = WorkspaceSupervisor::new(GitWorkspace::new(&run));
        Self {
            socket: root.join("supervisor.sock"),
            root,
            run,
            store,
            sessions,
            workspaces,
        }
    }

    fn runtime(&mut self) -> SpawnRuntime<'_, FakeProvider, GitWorkspace> {
        SpawnRuntime {
            store: &mut self.store,
            sessions: &mut self.sessions,
            workspaces: &mut self.workspaces,
            run_state_directory: &self.run,
            socket_path: &self.socket,
            run_id: RUN.parse().unwrap(),
        }
    }

    fn spawn(&mut self) -> Result<RpcResponse, RpcFailure> {
        spawn_agent(
            self.runtime(),
            &AuthenticatedCaller::Operator,
            OPERATION.parse().unwrap(),
            "worker".to_owned(),
            TASK.parse().unwrap(),
        )
    }

    fn scope(&mut self) -> SessionScope {
        let session = self
            .store
            .transaction(|r| Ok(r.sessions(RUN.parse().unwrap())?.remove(0)))
            .unwrap();
        SessionScope {
            run_id: session.run_id,
            agent_id: session.agent_id,
            session_id: session.id,
            generation: session.generation,
        }
    }

    fn workspace(&mut self) -> crate::state::WorkspaceRecord {
        self.store
            .transaction(|r| Ok(r.workspaces(RUN.parse().unwrap())?.remove(0)))
            .unwrap()
    }

    fn prepare(&mut self, case: &str) {
        self.store
            .transaction(|r| {
                r.insert_task(&TaskRecord {
                    id: TASK.parse().unwrap(),
                    run_id: RUN.parse().unwrap(),
                    project_id: PROJECT.parse().unwrap(),
                    group_id: None,
                    title: "Preserve this task".to_owned(),
                    description: "Recover without repeating work".to_owned(),
                    status: TaskStatus::Open,
                    result: None,
                    created_at: 1,
                    updated_at: 1,
                })
            })
            .unwrap();
        if matches!(case, "task" | "spawn" | "idle-shutdown") {
            return;
        }
        self.spawn().unwrap();
        let scope = self.scope();
        if case != "transcript" {
            self.sessions
                .advance(
                    &mut self.store,
                    scope.session_id,
                    unix_timestamp().unwrap(),
                )
                .unwrap();
        }
        if matches!(case, "recover-assignment" | "continue-assignment") {
            recovery_tests::prepare(self, case);
            return;
        }
        if matches!(
            case,
            "finish"
                | "integrate"
                | "merge"
                | "rebase"
                | "close"
                | "resubmit"
                | "external-close"
        ) {
            let workspace = self.workspace();
            commit(
                &workspace.path,
                "result.txt",
                "recoverable worker result\n",
            );
            commit(
                &workspace.path,
                "second.txt",
                "another recoverable change\n",
            );
        }
        if matches!(
            case,
            "integrate"
                | "merge"
                | "rebase"
                | "close"
                | "resubmit"
                | "external-close"
        ) {
            self.finish();
        }
        if case == "resubmit" {
            let workspace = self.workspace();
            fs::write(
                self.root.join("original-result"),
                workspace.result_commit.unwrap(),
            )
            .unwrap();
            commit(&workspace.path, "correction.txt", "validated correction\n");
        }
        if case == "external-close" {
            commit(
                &self.root.join("project"),
                "result.txt",
                "recoverable worker result\n",
            );
            commit(
                &self.root.join("project"),
                "second.txt",
                "another recoverable change\n",
            );
        }
        if case == "close" {
            self.integrate(case);
        }
        if matches!(case, "merge" | "rebase") {
            commit(
                &self.root.join("project"),
                "concurrent.txt",
                "independent target work\n",
            );
        }
        if case == "acknowledge" {
            self.message();
        }
    }

    fn exercise(&mut self, case: &str) {
        match case {
            "task" => self.task(),
            "spawn" => {
                self.spawn().unwrap();
            }
            "finish" => self.finish(),
            "resubmit" => self.resubmit(),
            "recover-assignment" | "continue-assignment" => {
                recovery_tests::exercise(self, case)
            }
            "integrate" | "merge" | "rebase" => self.integrate(case),
            "message" => self.message(),
            "acknowledge" => self.acknowledge(),
            "close" => self.close(false),
            "external-close" => self.close(true),
            "transcript" => {
                let scope = self.scope();
                TranscriptStore::new(&self.run)
                    .append(scope.session_id, b"{\"type\":\"partial")
                    .unwrap();
            }
            "shutdown" => self.shutdown(),
            "idle-shutdown" => self.idle_shutdown(),
            _ => panic!("unknown crash case: {case}"),
        }
    }

    fn task(&mut self) {
        create_task(
            &mut self.store,
            RUN.parse().unwrap(),
            &AuthenticatedCaller::Operator,
            OPERATION.parse().unwrap(),
            "Dependent task".to_owned(),
            "Retain dependency and group".to_owned(),
            "primary".to_owned(),
            Some("crash matrix".to_owned()),
            vec![TASK.parse().unwrap()],
            None,
        )
        .unwrap();
    }

    fn close(&mut self, external: bool) {
        let operator_override = external.then(|| {
            let workspace = self.workspace();
            crate::protocol::ClosureOverrideRequest {
                assignment_id: workspace.assignment_id,
                result_commit: workspace.result_commit.unwrap(),
                target_commit: Repository::open(self.root.join("project"))
                    .unwrap()
                    .head()
                    .unwrap()
                    .target()
                    .unwrap()
                    .to_string(),
                reason: "Applied and validated externally.".to_owned(),
            }
        });
        close_task(
            &mut self.store,
            &self.workspaces,
            RUN.parse().unwrap(),
            &AuthenticatedCaller::Operator,
            "co-01ARZ3NDEKTSV4RRFFQ69G5FB4".parse().unwrap(),
            TASK.parse().unwrap(),
            "Validated the integrated result.".to_owned(),
            operator_override,
            None,
        )
        .unwrap();
    }

    fn finish(&mut self) {
        let caller = AuthenticatedCaller::Agent(self.scope());
        finish_assignment(
            &mut self.store,
            &self.workspaces,
            RUN.parse().unwrap(),
            &caller,
            "co-01ARZ3NDEKTSV4RRFFQ69G5FAZ".parse().unwrap(),
            FinishStatus::Completed,
            "Preserve the implementation and its tests.".to_owned(),
            None,
        )
        .unwrap();
    }

    fn integrate(&mut self, case: &str) {
        let assignment = self.workspace().assignment_id;
        integrate_workspace(
            &mut self.store,
            &mut self.workspaces,
            RUN.parse().unwrap(),
            &AuthenticatedCaller::Operator,
            "co-01ARZ3NDEKTSV4RRFFQ69G5FB0".parse().unwrap(),
            assignment,
            (case == "merge")
                .then_some(crate::workspace::IntegrationStrategy::Merge),
        )
        .unwrap();
    }

    fn correction(&mut self) -> crate::state::resubmit::Resubmission {
        let workspace = self.workspace();
        crate::state::resubmit::Resubmission {
            assignment_id: workspace.assignment_id,
            expected_result: fs::read_to_string(
                self.root.join("original-result"),
            )
            .unwrap(),
            result_commit: Repository::open(&workspace.path)
                .unwrap()
                .head()
                .unwrap()
                .target()
                .unwrap()
                .to_string(),
            summary: "Validated the corrected implementation.".to_owned(),
            reason: "The original submission omitted a correction.".to_owned(),
        }
    }

    fn resubmit(&mut self) {
        let correction = self.correction();
        resubmit::resubmit_task(
            &mut self.store,
            &self.workspaces,
            RUN.parse().unwrap(),
            &AuthenticatedCaller::Operator,
            "co-01ARZ3NDEKTSV4RRFFQ69G5FB5".parse().unwrap(),
            correction,
            None,
        )
        .unwrap();
    }

    fn message(&mut self) {
        let recipient = self.scope().agent_id.to_string();
        send_message(
            &mut self.store,
            RUN.parse().unwrap(),
            &AuthenticatedCaller::Operator,
            "co-01ARZ3NDEKTSV4RRFFQ69G5FB1".parse().unwrap(),
            recipient,
            "Durable handoff".to_owned(),
            None,
        )
        .unwrap();
    }

    fn acknowledge(&mut self) {
        let caller = AuthenticatedCaller::Agent(self.scope());
        acknowledge_inbox(
            &mut self.store,
            RUN.parse().unwrap(),
            &caller,
            "co-01ARZ3NDEKTSV4RRFFQ69G5FB2".parse().unwrap(),
            1,
        )
        .unwrap();
    }

    fn shutdown(&mut self) {
        let (sender, mut receiver) = oneshot::channel();
        let mut foreground = ForegroundCoordination::default();
        begin_shutdown(
            &mut self.store,
            &mut self.sessions,
            &mut self.workspaces,
            RUN.parse().unwrap(),
            "co-01ARZ3NDEKTSV4RRFFQ69G5FB3".parse().unwrap(),
            sender,
            &mut foreground,
        );
        assert!(receiver.try_recv().unwrap().is_ok());
    }

    fn idle_shutdown(&mut self) {
        let run_id = RUN.parse().unwrap();
        let policy = self.store.configuration(run_id).unwrap().supervision;
        let mut idle = idle::IdleShutdown::new(policy);
        let now = Instant::now();
        idle.begin_if_due(&mut self.store, run_id, now).unwrap();
        idle.begin_if_due(
            &mut self.store,
            run_id,
            now + Duration::from_secs(policy.idle_timeout_seconds as u64),
        )
        .unwrap();
        let mut foreground = ForegroundCoordination {
            pending_shutdown: Some(PendingShutdown {
                responses: Vec::new(),
            }),
            ..Default::default()
        };
        progress_shutdown_inner(
            &mut self.store,
            &mut self.sessions,
            &mut self.workspaces,
            run_id,
            &mut foreground,
        )
        .unwrap();
    }

    fn recover(&mut self, case: &str) {
        let run_id = RUN.parse().unwrap();
        let now = unix_timestamp().unwrap();
        if matches!(case, "recover-assignment" | "continue-assignment") {
            recovery_tests::recover(self, case);
            return;
        }
        if case == "idle-shutdown" {
            self.idle_shutdown();
            return;
        }
        if case == "shutdown" {
            self.sessions
                .reconcile_after_restart(&mut self.store, run_id, now)
                .unwrap();
            self.shutdown();
            return;
        }
        self.workspaces
            .reconcile_after_restart(&mut self.store, run_id, now)
            .unwrap();
        self.sessions
            .reconcile_after_restart(&mut self.store, run_id, now)
            .unwrap();
        reconcile_operations(&mut self.runtime(), now).unwrap();
        match case {
            "task" => self.task(),
            "spawn" => {
                // A retry can report an uncertain launched process. It must
                // retain the claim and never launch a replacement implicitly.
                if self
                    .store
                    .transaction(|r| r.operation(OPERATION.parse().unwrap()))
                    .unwrap()
                    .is_none()
                {
                    self.spawn().unwrap();
                }
                self.sessions = runtime_sessions(&self.run);
                self.sessions
                    .reconcile_after_restart(&mut self.store, run_id, now)
                    .unwrap();
                reconcile_operations(&mut self.runtime(), now).unwrap();
            }
            "integrate" | "merge" | "rebase" => {
                if self
                    .store
                    .transaction(|r| {
                        r.operation(
                            "co-01ARZ3NDEKTSV4RRFFQ69G5FB0".parse().unwrap(),
                        )
                    })
                    .unwrap()
                    .is_none()
                {
                    self.integrate(case);
                }
            }
            "message" => self.message(),
            "acknowledge" => self.acknowledge(),
            "close" => self.close(false),
            "external-close" => self.close(true),
            "initialize" | "finish" | "transcript" => {}
            "resubmit" => self.resubmit(),
            _ => panic!("unknown recovery case: {case}"),
        }
    }

    fn verify(&mut self, case: &str) {
        if matches!(case, "recover-assignment" | "continue-assignment") {
            recovery_tests::verify(self, case);
            return;
        }
        let run_id = RUN.parse().unwrap();
        assert_eq!(
            fs::read_to_string(self.root.join("project/AGENTS.md")).unwrap(),
            "Preserve project instructions.\n"
        );
        self.store
            .transaction(|r| {
                assert_eq!(r.projects(run_id)?.len(), 1);
                assert_eq!(
                    r.run(run_id)?.unwrap().status,
                    if matches!(case, "shutdown" | "idle-shutdown") {
                        "stopped"
                    } else {
                        "active"
                    }
                );
                let events = r.events_after(run_id, 0, 1000)?;
                assert_eq!(
                    events
                        .iter()
                        .filter(|e| e.event_type == "run.started")
                        .count(),
                    1
                );
                let tasks = r.tasks(run_id)?;
                assert_eq!(
                    tasks.len(),
                    match case {
                        "initialize" => 0,
                        "task" => 2,
                        _ => 1,
                    }
                );
                if case != "initialize" {
                    let task = r.task(TASK.parse().unwrap())?.unwrap();
                    assert_eq!(
                        task.description,
                        "Recover without repeating work"
                    );
                    if case == "task" {
                        let dependent = tasks
                            .iter()
                            .find(|task| task.id != TASK.parse().unwrap())
                            .unwrap();
                        assert!(dependent.group_id.is_some());
                        let readiness =
                            r.task_readiness(dependent.id)?.unwrap();
                        assert!(!readiness.is_ready());
                        assert_eq!(dependent.status, TaskStatus::Open);
                    }
                    if !matches!(case, "task" | "idle-shutdown") {
                        let assignments = r
                            .workspaces(run_id)?
                            .into_iter()
                            .map(|w| {
                                r.assignment(w.assignment_id)
                                    .map(Option::unwrap)
                            })
                            .collect::<Result<Vec<_>, _>>()?;
                        assert_eq!(assignments.len(), 1);
                        for kind in [
                            "task.claimed",
                            "assignment.created",
                            "session.started",
                        ] {
                            assert_eq!(
                                events
                                    .iter()
                                    .filter(|event| event.event_type == kind)
                                    .count(),
                                1
                            );
                        }
                        assert_eq!(r.sessions(run_id)?.len(), 1);
                        assert!(r.sessions(run_id)?.iter().all(|s| matches!(
                            s.state,
                            LifecycleState::Unknown
                                | LifecycleState::Lost
                                | LifecycleState::Exited
                                | LifecycleState::Quarantined
                        )));
                        assert_eq!(r.workspaces(run_id)?.len(), 1);
                        assert!(matches!(
                            task.status,
                            TaskStatus::InProgress
                                | TaskStatus::Submitted
                                | TaskStatus::Closed
                        ));
                        assert_eq!(
                            matches!(
                                task.status,
                                TaskStatus::Submitted | TaskStatus::Closed
                            ),
                            assignments[0].state == "completed"
                        );
                        if matches!(case, "close" | "external-close") {
                            assert_eq!(task.status, TaskStatus::Closed);
                        }
                        if case == "external-close" {
                            let result = task.result.as_ref().unwrap();
                            assert!(result.get("integration").is_none());
                            assert_eq!(result["operator_override"]["assignment_id"], json!(assignments[0].id));
                            assert!(r.workspaces(run_id)?[0].target_commit.is_none());
                            assert_eq!(events.iter().filter(|event| {
                                event.event_type == "task.lifecycle_changed"
                                    && event.payload["data"]["operator_override"] == result["operator_override"]
                            }).count(), 1);
                        }
                    }
                }
                for kind in [
                    "task.claimed",
                    "assignment.created",
                    "session.started",
                    "workspace.integrated",
                    "message.sent",
                    "message.acknowledged",
                    "run.stopped",
                ] {
                    assert!(
                        events.iter().filter(|e| e.event_type == kind).count()
                            <= 1,
                        "duplicate {kind}"
                    );
                }
                for (kind, expected) in [
                    ("task.resubmitted", case == "resubmit"),
                    ("message.sent", matches!(case, "message" | "acknowledge")),
                    ("message.acknowledged", case == "acknowledge"),
                    ("run.stopped", matches!(case, "shutdown" | "idle-shutdown")),
                ] {
                    assert_eq!(
                        events
                            .iter()
                            .filter(|event| event.event_type == kind)
                            .count(),
                        usize::from(expected)
                    );
                }
                Ok(())
            })
            .unwrap();
        if !matches!(case, "initialize" | "task" | "idle-shutdown") {
            let workspace = self.workspace();
            assert!(workspace.path.is_dir());
            assert_eq!(
                fs::read_to_string(workspace.path.join("AGENTS.md")).unwrap(),
                "Preserve project instructions.\n"
            );
            let repository =
                Repository::open(self.root.join("project")).unwrap();
            assert_eq!(repository.worktrees().unwrap().len(), 1);
            assert_eq!(
                repository
                    .references_glob("refs/heads/coterie/*")
                    .unwrap()
                    .count(),
                1
            );
            if matches!(
                case,
                "finish"
                    | "integrate"
                    | "merge"
                    | "rebase"
                    | "close"
                    | "resubmit"
                    | "external-close"
            ) {
                assert_eq!(
                    fs::read_to_string(workspace.path.join("result.txt"))
                        .unwrap(),
                    "recoverable worker result\n"
                );
                assert_eq!(
                    fs::read_to_string(workspace.path.join("second.txt"))
                        .unwrap(),
                    "another recoverable change\n"
                );
            }
            if case == "resubmit" {
                let correction = self.correction();
                assert_eq!(
                    workspace.result_commit.as_deref(),
                    Some(correction.result_commit.as_str())
                );
                assert_eq!(workspace.target_commit, None);
                let repository = Repository::open(&workspace.path).unwrap();
                assert!(
                    repository
                        .graph_descendant_of(
                            correction.result_commit.parse().unwrap(),
                            correction.expected_result.parse().unwrap()
                        )
                        .unwrap()
                );
                self.store.transaction(|r| {
                    let event = r.events_after(run_id, 0, 1000)?.into_iter().find(|event| event.event_type == "task.resubmitted").unwrap();
                    assert_eq!(event.payload["data"]["previous_result"]["result_commit"], correction.expected_result);
                    assert_eq!(event.payload["data"]["result"]["result_commit"], correction.result_commit);
                    assert_eq!(r.task(TASK.parse().unwrap())?.unwrap().status, TaskStatus::Submitted);
                    Ok(())
                }).unwrap();
            }
            if case == "transcript" {
                let session_id = self.scope().session_id;
                let page = TranscriptStore::new(&self.run)
                    .read(session_id, 0, 65536)
                    .unwrap();
                let prefix = b"{\"type\":\"session.ready\"}\n";
                assert!(page.bytes.starts_with(prefix));
                assert!(
                    page.bytes == prefix
                        || page.bytes
                            == [prefix.as_slice(), b"{\"type\":\"partial"]
                                .concat()
                );
                assert_eq!(
                    page.incomplete_tail,
                    page.bytes.len() > prefix.len()
                );
            }
            if matches!(case, "integrate" | "merge" | "rebase")
                && workspace.target_commit.is_none()
            {
                let operation = self
                    .store
                    .transaction(|r| {
                        r.operation(
                            "co-01ARZ3NDEKTSV4RRFFQ69G5FB0".parse().unwrap(),
                        )
                    })
                    .unwrap()
                    .unwrap();
                assert_eq!(
                    operation.reconciliation_state,
                    Some(ExternalResourceState::Unknown)
                );
                assert!(operation.reconciliation_error.is_some());
                assert!(["trace", "recovery.trace"].iter().any(|name| {
                    fs::read_to_string(self.root.join(name)).is_ok_and(
                        |trace| {
                            trace.lines().last()
                                == Some("integration.checkout.progress")
                        },
                    )
                }));
                return;
            }
            if matches!(case, "integrate" | "merge" | "rebase" | "close") {
                assert_eq!(
                    workspace.target_commit.as_deref(),
                    Some(
                        repository
                            .head()
                            .unwrap()
                            .target()
                            .unwrap()
                            .to_string()
                            .as_str()
                    )
                );
                assert_eq!(
                    fs::read_to_string(self.root.join("project/result.txt"))
                        .unwrap(),
                    "recoverable worker result\n"
                );
                let tip = repository.head().unwrap().peel_to_commit().unwrap();
                assert_eq!(
                    tip.parent_count(),
                    if case == "merge" { 2 } else { 1 }
                );
                if case == "rebase" {
                    assert_eq!(tip.message().unwrap(), "second.txt");
                    let first = tip.parent(0).unwrap();
                    assert_eq!(first.parent_count(), 1);
                    assert_eq!(first.message().unwrap(), "result.txt");
                    assert_eq!(
                        first.parent(0).unwrap().message().unwrap(),
                        "concurrent.txt"
                    );
                    assert_eq!(
                        Repository::open(&workspace.path)
                            .unwrap()
                            .head()
                            .unwrap()
                            .target()
                            .unwrap()
                            .to_string(),
                        workspace.result_commit.as_deref().unwrap()
                    );
                }
                assert!(repository.statuses(None).unwrap().is_empty());
                assert_eq!(
                    repository
                        .reflog(repository.head().unwrap().name().unwrap())
                        .unwrap()
                        .len(),
                    if matches!(case, "merge" | "rebase") {
                        3
                    } else {
                        2
                    }
                );
            }
        }
    }
}

fn prepare_project(root: &Path) {
    let project = root.join("project");
    Repository::init(&project).unwrap();
    commit(&project, "AGENTS.md", "Preserve project instructions.\n");
}

fn commit(path: &Path, name: &str, contents: &str) {
    fs::write(path.join(name), contents).unwrap();
    let repository = Repository::open(path).unwrap();
    let mut index = repository.index().unwrap();
    index.add_path(Path::new(name)).unwrap();
    index.write().unwrap();
    let tree_id = index.write_tree().unwrap();
    let tree = repository.find_tree(tree_id).unwrap();
    let parent = repository
        .head()
        .ok()
        .map(|head| head.peel_to_commit().unwrap());
    let parents = parent.iter().collect::<Vec<_>>();
    let signature = Signature::new(
        "Crash Matrix",
        "crash@example.invalid",
        &git2::Time::new(10, 0),
    )
    .unwrap();
    repository
        .commit(Some("HEAD"), &signature, &signature, name, &tree, &parents)
        .unwrap();
}

#[derive(Debug, PartialEq)]
struct Snapshot {
    database: BTreeMap<String, Vec<Vec<rusqlite::types::Value>>>,
    files: BTreeMap<PathBuf, Vec<u8>>,
}

fn snapshot(root: &Path) -> Snapshot {
    let database_path = if root.join("run").exists() {
        root.join("run/state.sqlite3")
    } else {
        root.join("state/coterie/runs")
            .join(RUN)
            .join("state.sqlite3")
    };
    let connection = rusqlite::Connection::open_with_flags(
        database_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    assert_eq!(
        connection
            .query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    assert!(
        !connection
            .prepare("PRAGMA foreign_key_check")
            .unwrap()
            .exists([])
            .unwrap()
    );
    let tables = connection
        .prepare(
            "SELECT name FROM sqlite_schema WHERE type = 'table' ORDER BY name",
        )
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let mut database = BTreeMap::new();
    for table in tables {
        let mut statement = connection
            .prepare(&format!(
                "SELECT * FROM \"{}\" ORDER BY rowid",
                table.replace('"', "\"\"")
            ))
            .unwrap();
        let columns = statement.column_count();
        let rows = statement
            .query_map([], |r| {
                (0..columns)
                    .map(|i| r.get(i))
                    .collect::<Result<Vec<_>, _>>()
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        database.insert(table, rows);
    }
    let mut files = BTreeMap::new();
    collect_files(root, Path::new("project"), &mut files);
    for directory in [
        "run/workspaces",
        "run/transcripts",
        "rt",
        "state/coterie/projects",
    ] {
        if root.join(directory).exists() {
            collect_files(root, Path::new(directory), &mut files);
        }
    }
    if root.join("codex.launches").exists() {
        files.insert(
            PathBuf::from("codex.launches"),
            fs::read(root.join("codex.launches")).unwrap(),
        );
    }
    if root.join("codex.signals").exists() {
        files.insert(
            PathBuf::from("codex.signals"),
            fs::read(root.join("codex.signals")).unwrap(),
        );
    }
    Snapshot { database, files }
}

fn collect_files(
    root: &Path,
    relative: &Path,
    files: &mut BTreeMap<PathBuf, Vec<u8>>,
) {
    for entry in fs::read_dir(root.join(relative)).unwrap() {
        let entry = entry.unwrap();
        let path = relative.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            collect_files(root, &path, files);
        } else if entry.file_type().unwrap().is_socket() {
            use std::os::unix::fs::MetadataExt;
            let metadata = entry.metadata().unwrap();
            files.insert(
                path,
                format!("socket:{}:{}", metadata.ino(), metadata.mode())
                    .into_bytes(),
            );
        } else {
            files.insert(path, fs::read(entry.path()).unwrap());
        }
    }
}

struct Directory(PathBuf);

impl Directory {
    fn new() -> Self {
        // Crash snapshots include the runtime socket, whose Linux path limit
        // cannot accommodate an arbitrarily long TMPDIR such as CI's.
        let root = Path::new("/tmp")
            .join(format!("coterie-crash-{}", RunId::generate()));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        Self(root)
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        if self.0.join("codex").exists() {
            fs::write(self.0.join("codex.release"), "").unwrap();
            let trace =
                fs::read_to_string(self.0.join("trace")).unwrap_or_default();
            let launched = self.0.join("codex.launches").exists()
                || trace.contains("process.job.spawn.after")
                || trace.contains("process.foreground.spawn.after");
            // Keep the release marker until the fixture has acknowledged it or
            // /proc proves the recorded process cannot write again.
            let deadline = std::time::Instant::now() + Duration::from_secs(2);
            while launched && !self.0.join("codex.exited").exists() {
                let pid = fs::read_to_string(self.0.join("codex.launches"))
                    .ok()
                    .and_then(|ledger| {
                        ledger.split_whitespace().next().map(str::to_owned)
                    });
                if let Some(pid) = pid {
                    match fs::read_to_string(format!("/proc/{pid}/stat")) {
                        Err(error)
                            if error.kind() == io::ErrorKind::NotFound =>
                        {
                            break;
                        }
                        Ok(stat)
                            if stat.rsplit_once(") ").is_some_and(
                                |(_, fields)| {
                                    fields.starts_with("Z ")
                                        || fields.starts_with("X ")
                                },
                            ) =>
                        {
                            break;
                        }
                        _ => {}
                    }
                }
                if std::time::Instant::now() >= deadline {
                    if std::thread::panicking() {
                        eprintln!(
                            "retained unresponsive provider fixture at {}",
                            self.0.display()
                        );
                        return;
                    }
                    panic!(
                        "provider fixture did not stop; retained {}",
                        self.0.display()
                    );
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        fs::remove_dir_all(&self.0).unwrap();
    }
}

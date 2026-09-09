//! Every traced occurrence is crashed in a separate process, without unwinding.

use super::*;
use crate::fault::injection;
use crate::workspace::GitWorkspace;
use git2::{Repository, Signature};
use std::os::unix::fs::DirBuilderExt;

const CHILD: &str = "supervisor::crash_tests::crash_child";
const RUN: &str = "cr-01ARZ3NDEKTSV4RRFFQ69G5FAV";
const PROJECT: &str = "cp-01ARZ3NDEKTSV4RRFFQ69G5FAW";
const TASK: &str = "ct-01ARZ3NDEKTSV4RRFFQ69G5FAX";
const OPERATION: &str = "co-01ARZ3NDEKTSV4RRFFQ69G5FAY";

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
fn crash_matrix_fast_forward() {
    matrix("integrate");
}

#[test]
fn crash_matrix_merge() {
    matrix("merge");
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
fn crash_matrix_runtime_publication_and_retirement() {
    matrix("runtime");
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
fn crash_matrix_runtime_recovery() {
    recovery_matrix("runtime", "socket.permissions.after");
}

#[test]
fn crash_matrix_covers_all_declared_boundaries() {
    let mut declared = std::collections::BTreeSet::new();
    for source in [
        include_str!("../state.rs"),
        include_str!("../supervisor.rs"),
        include_str!("session.rs"),
        include_str!("../providers.rs"),
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
        "task",
        "spawn",
        "finish",
        "integrate",
        "merge",
        "transcript",
        "shutdown",
        "message",
        "acknowledge",
        "close",
        "runtime",
        "process",
        "interrupt-process",
        "terminate-process",
        "kill-process",
        "exit-process",
        "malformed-process",
        "foreground",
        "foreground-exit",
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
                "{case} {mode} {index:?} timed out: {}",
                fs::read_to_string(root.join("child.log")).unwrap()
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
    if case == "runtime" {
        runtime_child(&root, &mode);
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
            injection::disarm();
        });
    } else {
        let now = unix_timestamp().unwrap();
        fixture
            .sessions
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
                LifecycleState::Unknown | LifecycleState::Exited
            ));
            assert_eq!(session.process_owner, SessionProcessOwner::Foreground);
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
}

// This executable exercises the actual Codex process adapter without a model,
// network, or credentials. Its independent ledger detects repeated launches.
const PROCESS_PROVIDER: &str = r#"#!/bin/sh
if [ "$1" = "--version" ]; then printf 'codex-cli 0.151.0\n'; exit 0; fi
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

fn runtime_child(root: &Path, mode: &str) {
    if mode == "exercise" {
        prepare_project(root);
    }
    crate::private_fs::directory(&root.join("rt")).unwrap();
    let project = DiscoveredProject::discover(root.join("project")).unwrap();
    let active = ActiveRunEntry::new(
        RUN.parse().unwrap(),
        PROJECT.parse().unwrap(),
        project.identity.clone(),
    );
    let directories = CoterieDirectories::from_environment().unwrap();
    if mode == "exercise" {
        let index = std::env::var("COTERIE_CRASH_TEST_INDEX")
            .unwrap()
            .parse()
            .ok();
        injection::arm(&root.join("trace"), index);
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let result = runtime.block_on(async {
        let socket = directories.socket_path(active.run_id);
        let operator_entry = active.clone();
        let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let operator_done = Arc::clone(&done);
        // The operator performs its RPC on another thread so its filesystem
        // reads cannot consume the supervisor's fault schedule.
        let operator = std::thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(async {
                    for _ in 0..200 {
                        if operator_done
                            .load(std::sync::atomic::Ordering::Relaxed)
                        {
                            return;
                        }
                        if let Ok(mut client) =
                            SupervisorClient::connect_operator_at(
                                &socket,
                                &operator_entry,
                            )
                            .await
                        {
                            client
                                .shutdown(OPERATION.parse().unwrap())
                                .await
                                .unwrap();
                            return;
                        }
                        sleep(Duration::from_millis(5)).await;
                    }
                    // A stopped run only needs retirement, so no listener is opened.
                });
        });
        let result =
            serve(active.clone(), project.clone(), directories.clone()).await;
        done.store(true, std::sync::atomic::Ordering::Relaxed);
        operator.join().unwrap();
        result
    });
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
        if matches!(case, "task" | "spawn") {
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
        if matches!(case, "finish" | "integrate" | "merge" | "close") {
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
        if matches!(case, "integrate" | "merge" | "close") {
            self.finish();
        }
        if case == "close" {
            self.integrate();
        }
        if case == "merge" {
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
            "integrate" | "merge" => self.integrate(),
            "message" => self.message(),
            "acknowledge" => self.acknowledge(),
            "close" => self.close(),
            "transcript" => {
                let scope = self.scope();
                TranscriptStore::new(&self.run)
                    .append(scope.session_id, b"{\"type\":\"partial")
                    .unwrap();
            }
            "shutdown" => self.shutdown(),
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

    fn close(&mut self) {
        close_task(
            &mut self.store,
            RUN.parse().unwrap(),
            &AuthenticatedCaller::Operator,
            "co-01ARZ3NDEKTSV4RRFFQ69G5FB4".parse().unwrap(),
            TASK.parse().unwrap(),
            "Validated the integrated result.".to_owned(),
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

    fn integrate(&mut self) {
        let assignment = self.workspace().assignment_id;
        integrate_workspace(
            &mut self.store,
            &mut self.workspaces,
            RUN.parse().unwrap(),
            &AuthenticatedCaller::Operator,
            "co-01ARZ3NDEKTSV4RRFFQ69G5FB0".parse().unwrap(),
            assignment,
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

    fn recover(&mut self, case: &str) {
        let run_id = RUN.parse().unwrap();
        let now = unix_timestamp().unwrap();
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
            "integrate" | "merge" => {
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
                    self.integrate();
                }
            }
            "message" => self.message(),
            "acknowledge" => self.acknowledge(),
            "close" => self.close(),
            "initialize" | "finish" | "transcript" => {}
            _ => panic!("unknown recovery case: {case}"),
        }
    }

    fn verify(&mut self, case: &str) {
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
                    if case == "shutdown" {
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
                    if !matches!(case, "task") {
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
                        if case == "close" {
                            assert_eq!(task.status, TaskStatus::Closed);
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
                    ("message.sent", matches!(case, "message" | "acknowledge")),
                    ("message.acknowledged", case == "acknowledge"),
                    ("run.stopped", case == "shutdown"),
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
        if !matches!(case, "initialize" | "task") {
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
            if matches!(case, "finish" | "integrate" | "merge" | "close") {
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
            if matches!(case, "integrate" | "merge")
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
                assert!(
                    fs::read_to_string(self.root.join("trace"))
                        .unwrap()
                        .lines()
                        .last()
                        == Some("integration.checkout.progress")
                );
                return;
            }
            if matches!(case, "integrate" | "merge" | "close") {
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
                assert!(repository.statuses(None).unwrap().is_empty());
                assert_eq!(
                    repository
                        .reflog(repository.head().unwrap().name().unwrap())
                        .unwrap()
                        .len(),
                    if case == "merge" { 3 } else { 2 }
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
        let root = std::env::temp_dir()
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

use super::*;

fn fixture() -> (Directory, Fixture) {
    let directory = Directory::new();
    prepare_project(&directory.0);
    let mut fixture = Fixture::open(directory.0.clone());
    fixture.prepare("recovery");
    (directory, fixture)
}

fn request(
    fixture: &mut Fixture,
    caller: &AuthenticatedCaller,
    operation_id: OperationId,
    assignment_id: AssignmentId,
    reason: &str,
) -> Result<RpcResponse, RpcFailure> {
    execute_request(
        &mut fixture.store,
        &mut fixture.sessions,
        &mut fixture.workspaces,
        RuntimePaths {
            run_state_directory: &fixture.run,
            socket_path: &fixture.socket,
        },
        RUN.parse().unwrap(),
        caller,
        RpcRequest::TaskRecover {
            operation_id,
            assignment_id,
            reason: reason.into(),
        },
    )
}

fn exit(fixture: &mut Fixture) {
    let scope = fixture.scope();
    fixture
        .sessions
        .terminate(
            &mut fixture.store,
            scope.session_id,
            unix_timestamp().unwrap(),
        )
        .unwrap();
    fixture
        .sessions
        .drive_controls(
            &mut fixture.store,
            scope.run_id,
            unix_timestamp().unwrap() * 1000,
        )
        .unwrap();
}

#[test]
fn recovery_preserves_dirty_source_and_fences_late_output_before_continuation()
{
    let (_directory, mut fixture) = fixture();
    let scope = fixture.scope();
    let workspace = fixture.workspace();
    commit(&workspace.path, "committed.txt", "preserve commit\n");
    fs::write(workspace.path.join("committed.txt"), "staged\n").unwrap();
    let repository = Repository::open(&workspace.path).unwrap();
    let mut index = repository.index().unwrap();
    index.add_path(Path::new("committed.txt")).unwrap();
    index.write().unwrap();
    fs::write(workspace.path.join("committed.txt"), "unstaged\n").unwrap();
    fs::write(workspace.path.join("untracked.txt"), "recover this too\n")
        .unwrap();
    let old_index = fs::read(repository.path().join("index")).unwrap();
    let old_head = repository.head().unwrap().target().unwrap();
    let operation = OperationId::generate();
    let caller = AuthenticatedCaller::Operator;
    let error = request(
        &mut fixture,
        &caller,
        operation,
        workspace.assignment_id,
        "Interrupted.",
    )
    .unwrap_err();
    assert_eq!(error.code, RpcFailureCode::Conflict);
    assert!(error.message.contains("inactivity"));
    exit(&mut fixture);
    let session = fixture
        .store
        .transaction(|r| r.session(scope.session_id))
        .unwrap()
        .unwrap();
    let response = request(
        &mut fixture,
        &caller,
        operation,
        workspace.assignment_id,
        "Interrupted.",
    )
    .unwrap();
    assert_eq!(
        request(
            &mut fixture,
            &caller,
            operation,
            workspace.assignment_id,
            "Interrupted."
        )
        .unwrap(),
        response
    );
    assert_eq!(
        request(
            &mut fixture,
            &caller,
            operation,
            workspace.assignment_id,
            "Different."
        )
        .unwrap_err()
        .code,
        RpcFailureCode::Conflict
    );
    assert_eq!(
        request(
            &mut fixture,
            &caller,
            OperationId::generate(),
            workspace.assignment_id,
            "Already retired."
        )
        .unwrap_err()
        .code,
        RpcFailureCode::Conflict
    );
    fixture
        .store
        .transaction(|r| {
            assert_eq!(
                r.workspace(workspace.assignment_id)?.unwrap(),
                workspace
            );
            assert_eq!(r.session(scope.session_id)?.unwrap(), session);
            let old = r.assignment(workspace.assignment_id)?.unwrap();
            assert_eq!(old.state, "released");
            assert!(old.completed_at.is_some());
            assert_eq!(old.summary, None);
            assert!(!r.session_scope_is_current(scope)?);
            assert!(!r.assignment_scope_is_current(workspace.scope())?);
            assert_eq!(
                r.record_session_lifecycle(scope, LifecycleState::Running, 99)?,
                SessionTransitionOutcome::Stale
            );
            assert_eq!(
                r.task(TASK.parse().unwrap())?.unwrap().status,
                TaskStatus::Open
            );
            Ok(())
        })
        .unwrap();
    // The fake still has queued output, which cannot touch the retired transcript.
    assert!(
        fixture
            .sessions
            .advance(&mut fixture.store, scope.session_id, 100)
            .unwrap()
            .is_none()
    );
    fixture
        .workspaces
        .reconcile_after_restart(&mut fixture.store, scope.run_id, 101)
        .unwrap();
    assert_eq!(repository.head().unwrap().target().unwrap(), old_head);
    assert_eq!(
        fs::read(repository.path().join("index")).unwrap(),
        old_index
    );
    assert_eq!(
        fs::read_to_string(workspace.path.join("committed.txt")).unwrap(),
        "unstaged\n"
    );
    assert_eq!(
        fs::read_to_string(workspace.path.join("untracked.txt")).unwrap(),
        "recover this too\n"
    );
    assert!(
        spawn_agent(
            fixture.runtime(),
            &caller,
            OperationId::generate(),
            "reviewer".into(),
            TASK.parse().unwrap()
        )
        .unwrap_err()
        .message
        .contains("fresh worktree")
    );
    let next_operation = OperationId::generate();
    let continued = spawn_agent(
        fixture.runtime(),
        &caller,
        next_operation,
        "worker".into(),
        TASK.parse().unwrap(),
    )
    .unwrap();
    assert_eq!(
        spawn_agent(
            fixture.runtime(),
            &caller,
            next_operation,
            "worker".into(),
            TASK.parse().unwrap()
        )
        .unwrap(),
        continued
    );
    let RpcResponse::Spawned { assignment_id, .. } = continued else {
        panic!("spawn expected")
    };
    fixture
        .store
        .transaction(|r| {
            let sources = r.task_recoveries(scope.run_id)?;
            assert_eq!(sources.len(), 1);
            assert_eq!(
                sources[0].continuation_assignment_id,
                Some(assignment_id)
            );
            assert_eq!(sources[0].assignment_id, workspace.assignment_id);
            assert_ne!(
                r.workspace(assignment_id)?.unwrap().path,
                workspace.path
            );
            assert_eq!(
                r.events_after(scope.run_id, 0, 1000)?
                    .iter()
                    .filter(|event| event.event_type == "assignment.continued")
                    .count(),
                1
            );
            Ok(())
        })
        .unwrap();
    assert_eq!(
        request(
            &mut fixture,
            &caller,
            operation,
            workspace.assignment_id,
            "Interrupted."
        )
        .unwrap(),
        response
    );
}

#[test]
fn recovery_refuses_uncertain_sessions_controls_and_integration_intents() {
    for change in [
        "unknown",
        "lost",
        "quarantined",
        "missing_end",
        "unrevoked",
        "generation",
        "launch",
        "control",
        "integration",
        "workspace",
        "draining",
    ] {
        let (_directory, mut fixture) = fixture();
        exit(&mut fixture);
        let workspace = fixture.workspace();
        let database =
            rusqlite::Connection::open(fixture.run.join("state.sqlite3"))
                .unwrap();
        match change {
            "unknown" => database.execute_batch("UPDATE sessions SET reconciliation_state = 'unknown'").unwrap(),
            "lost" => database.execute_batch("UPDATE sessions SET state = 'lost', reconciliation_state = 'lost'").unwrap(),
            "quarantined" => database.execute_batch("UPDATE sessions SET state = 'quarantined'").unwrap(),
            "missing_end" => database.execute_batch("UPDATE sessions SET ended_at = NULL").unwrap(),
            "unrevoked" => database.execute_batch("UPDATE session_credentials SET revoked_at = NULL").unwrap(),
            "generation" => database.execute_batch("UPDATE agents SET generation = generation + 1").unwrap(),
            "launch" => database.execute_batch("UPDATE operations SET reconciliation_state = 'desired' WHERE kind = 'agent.spawn'").unwrap(),
            "workspace" => database.execute_batch("UPDATE workspaces SET state = 'unknown'").unwrap(),
            "draining" => database.execute_batch("UPDATE runs SET status = 'stopped'").unwrap(),
            "control" => { let scope = fixture.scope(); fixture.store.transaction(|r| {r.request_session_control(scope, crate::state::supervision::ControlReason::ExecutionTimeout, 1, 2, 10)?; Ok(())}).unwrap(); }
            "integration" => { let mutation = Mutation { id: OperationId::generate(), run_id: RUN.parse().unwrap(), kind: "workspace.integrate".into(), actor_agent_id: None, request: json!({"assignment_id": workspace.assignment_id}), created_at: 1 }; fixture.store.mutate(&mutation, |_| Ok(())).unwrap(); }
            _ => unreachable!(),
        }
        let operation = OperationId::generate();
        let error = request(
            &mut fixture,
            &AuthenticatedCaller::Operator,
            operation,
            workspace.assignment_id,
            "Needs recovery.",
        )
        .unwrap_err();
        assert_eq!(error.code, RpcFailureCode::Conflict, "{change}: {error:?}");
        assert!(error.message.contains("doctor"), "{change}: {error:?}");
        fixture
            .store
            .transaction(|r| {
                assert!(r.operation(operation)?.is_none());
                assert!(r.task_recoveries(RUN.parse().unwrap())?.is_empty());
                assert!(
                    r.assignment(workspace.assignment_id)?
                        .unwrap()
                        .completed_at
                        .is_none()
                );
                Ok(())
            })
            .unwrap();
    }
}

#[test]
fn recovery_requires_capability_and_current_caller_even_for_replay() {
    let (_directory, mut fixture) = fixture();
    let workspace = fixture.workspace();
    let worker = AuthenticatedCaller::Agent(fixture.scope());
    assert_eq!(
        request(
            &mut fixture,
            &worker,
            OperationId::generate(),
            workspace.assignment_id,
            "Not authorized."
        )
        .unwrap_err()
        .code,
        RpcFailureCode::PermissionDenied
    );
    let RpcResponse::ForegroundPrepared {
        run_id,
        agent,
        session_id,
        generation,
        ..
    } = launch_foreground(
        &mut fixture.store,
        RUN.parse().unwrap(),
        &AuthenticatedCaller::Operator,
        OperationId::generate(),
        &AgentToken::generate().unwrap(),
    )
    .unwrap()
    else {
        panic!("foreground expected")
    };
    let scope = SessionScope {
        run_id,
        agent_id: agent.id,
        session_id,
        generation,
    };
    let caller = AuthenticatedCaller::Agent(scope);
    exit(&mut fixture);
    let operation = OperationId::generate();
    let secret = format!("cot1_{}", "a".repeat(64));
    let reason = format!("Interrupted {secret}");
    let response = request(
        &mut fixture,
        &caller,
        operation,
        workspace.assignment_id,
        &reason,
    )
    .unwrap();
    assert!(!serde_json::to_string(&response).unwrap().contains(&secret));
    fixture
        .store
        .transaction(|r| {
            assert!(
                !r.operation(operation)?
                    .unwrap()
                    .request
                    .to_string()
                    .contains(&secret)
            );
            r.record_session_lifecycle(
                scope,
                LifecycleState::Exited,
                unix_timestamp().unwrap(),
            )?;
            Ok(())
        })
        .unwrap();
    assert_eq!(
        request(
            &mut fixture,
            &caller,
            operation,
            workspace.assignment_id,
            &reason
        )
        .unwrap_err()
        .code,
        RpcFailureCode::Unauthenticated
    );
    assert_eq!(
        request(
            &mut fixture,
            &worker,
            OperationId::generate(),
            workspace.assignment_id,
            "Late worker."
        )
        .unwrap_err()
        .code,
        RpcFailureCode::Unauthenticated
    );
}

#[test]
fn recovery_after_execution_timeout_requires_completed_process_control() {
    let (_directory, mut fixture) = fixture();
    let scope = fixture.scope();
    let workspace = fixture.workspace();
    let created = fixture
        .store
        .transaction(|r| r.session(scope.session_id))
        .unwrap()
        .unwrap()
        .created_at;
    let timeout = fixture
        .store
        .configuration(scope.run_id)
        .unwrap()
        .supervision
        .job_timeout_seconds;
    let now_ms = (created + timeout) * 1000;
    fixture
        .sessions
        .drive_controls(&mut fixture.store, scope.run_id, now_ms)
        .unwrap();
    fixture
        .sessions
        .drive_controls(&mut fixture.store, scope.run_id, now_ms + 6000)
        .unwrap();
    fixture
        .sessions
        .drive_controls(&mut fixture.store, scope.run_id, now_ms + 6001)
        .unwrap();
    request(
        &mut fixture,
        &AuthenticatedCaller::Operator,
        OperationId::generate(),
        workspace.assignment_id,
        "Execution timed out before commit.",
    )
    .unwrap();
}

const RECOVERY_OPERATION: &str = "co-01ARZ3NDEKTSV4RRFFQ69G5FB6";
const CONTINUATION_OPERATION: &str = "co-01ARZ3NDEKTSV4RRFFQ69G5FB7";

pub(super) fn prepare(fixture: &mut Fixture, case: &str) {
    let workspace = fixture.workspace();
    commit(&workspace.path, "preserved.txt", "committed work\n");
    fs::write(workspace.path.join("preserved.txt"), "uncommitted work\n")
        .unwrap();
    fs::write(workspace.path.join("untracked.txt"), "untracked work\n")
        .unwrap();
    exit(fixture);
    if case == "continue-assignment" {
        exercise(fixture, "recover-assignment");
    }
}

pub(super) fn exercise(fixture: &mut Fixture, case: &str) {
    if case == "recover-assignment" {
        let assignment_id = fixture.workspace().assignment_id;
        request(
            fixture,
            &AuthenticatedCaller::Operator,
            RECOVERY_OPERATION.parse().unwrap(),
            assignment_id,
            "Continue interrupted work.",
        )
        .unwrap();
    } else {
        spawn_agent(
            fixture.runtime(),
            &AuthenticatedCaller::Operator,
            CONTINUATION_OPERATION.parse().unwrap(),
            "worker".into(),
            TASK.parse().unwrap(),
        )
        .unwrap();
    }
}

pub(super) fn recover(fixture: &mut Fixture, case: &str) {
    let run_id = RUN.parse().unwrap();
    let now = unix_timestamp().unwrap();
    fixture
        .workspaces
        .reconcile_after_restart(&mut fixture.store, run_id, now)
        .unwrap();
    fixture
        .sessions
        .reconcile_after_restart(&mut fixture.store, run_id, now)
        .unwrap();
    reconcile_operations(&mut fixture.runtime(), now).unwrap();
    if case == "recover-assignment"
        || fixture
            .store
            .transaction(|r| {
                r.operation(CONTINUATION_OPERATION.parse().unwrap())
            })
            .unwrap()
            .is_none()
    {
        exercise(fixture, case);
    }
    if case == "continue-assignment" {
        fixture.sessions = runtime_sessions(&fixture.run);
        fixture
            .sessions
            .reconcile_after_restart(&mut fixture.store, run_id, now)
            .unwrap();
        reconcile_operations(&mut fixture.runtime(), now).unwrap();
    }
}

pub(super) fn verify(fixture: &mut Fixture, case: &str) {
    let workspace = fixture.workspace();
    assert_eq!(
        fs::read_to_string(workspace.path.join("preserved.txt")).unwrap(),
        "uncommitted work\n"
    );
    assert_eq!(
        fs::read_to_string(workspace.path.join("untracked.txt")).unwrap(),
        "untracked work\n"
    );
    assert_eq!(
        fs::read_to_string(workspace.path.join("AGENTS.md")).unwrap(),
        "Preserve project instructions.\n"
    );
    let repository = Repository::open(&workspace.path).unwrap();
    let head = repository.head().unwrap().peel_to_commit().unwrap();
    assert_eq!(
        repository
            .find_blob(
                head.tree().unwrap().get_name("preserved.txt").unwrap().id()
            )
            .unwrap()
            .content(),
        b"committed work\n"
    );
    fixture
        .store
        .transaction(|r| {
            let run_id = RUN.parse().unwrap();
            let recoveries = r.task_recoveries(run_id)?;
            assert_eq!(recoveries.len(), 1);
            assert_eq!(recoveries[0].assignment_id, workspace.assignment_id);
            assert_eq!(
                r.assignment(workspace.assignment_id)?.unwrap().state,
                "released"
            );
            assert_eq!(
                r.session(recoveries[0].session_id)?.unwrap().state,
                LifecycleState::Exited
            );
            assert_eq!(
                r.task(TASK.parse().unwrap())?.unwrap().status,
                if case == "recover-assignment" {
                    TaskStatus::Open
                } else {
                    TaskStatus::InProgress
                }
            );
            assert_eq!(
                r.workspaces(run_id)?.len(),
                if case == "recover-assignment" { 1 } else { 2 }
            );
            if case == "continue-assignment" {
                let continuation =
                    recoveries[0].continuation_assignment_id.unwrap();
                assert_ne!(continuation, workspace.assignment_id);
                assert_ne!(
                    r.workspace(continuation)?.unwrap().path,
                    workspace.path
                );
            } else {
                assert!(recoveries[0].continuation_assignment_id.is_none());
            }
            Ok(())
        })
        .unwrap();
}

#[test]
fn repeated_recovery_preserves_each_source_and_continuation_link() {
    let (_directory, mut fixture) = fixture();
    let run_id = RUN.parse().unwrap();
    let mut assignment_id = fixture.workspace().assignment_id;
    let mut session_id = fixture.scope().session_id;
    let mut sources = Vec::new();
    for iteration in 0..3 {
        let workspace = fixture
            .store
            .transaction(|r| r.workspace(assignment_id))
            .unwrap()
            .unwrap();
        fs::write(
            workspace.path.join("pending.txt"),
            format!("attempt {iteration}\n"),
        )
        .unwrap();
        sources.push(workspace.path);
        fixture
            .sessions
            .terminate(
                &mut fixture.store,
                session_id,
                unix_timestamp().unwrap(),
            )
            .unwrap();
        request(
            &mut fixture,
            &AuthenticatedCaller::Operator,
            OperationId::generate(),
            assignment_id,
            "Interrupted again.",
        )
        .unwrap();
        let next = spawn_agent(
            fixture.runtime(),
            &AuthenticatedCaller::Operator,
            OperationId::generate(),
            "worker".into(),
            TASK.parse().unwrap(),
        )
        .unwrap();
        let RpcResponse::Spawned {
            assignment_id: next_assignment,
            session_id: next_session,
            ..
        } = next
        else {
            panic!("spawn expected")
        };
        let recoveries = fixture
            .store
            .transaction(|r| r.task_recoveries(run_id))
            .unwrap();
        assert_eq!(recoveries.len(), iteration + 1);
        assert_eq!(recoveries[iteration].assignment_id, assignment_id);
        assert_eq!(
            recoveries[iteration].continuation_assignment_id,
            Some(next_assignment)
        );
        assignment_id = next_assignment;
        session_id = next_session;
    }
    for (iteration, source) in sources.iter().enumerate() {
        assert_eq!(
            fs::read_to_string(source.join("pending.txt")).unwrap(),
            format!("attempt {iteration}\n")
        );
    }
}

#[test]
fn recovery_refuses_moved_source_and_retries_after_ownership_is_restored() {
    let (_directory, mut fixture) = fixture();
    exit(&mut fixture);
    let workspace = fixture.workspace();
    let moved = workspace.path.with_extension("preserved");
    fs::rename(&workspace.path, &moved).unwrap();
    let operation = OperationId::generate();
    let error = request(
        &mut fixture,
        &AuthenticatedCaller::Operator,
        operation,
        workspace.assignment_id,
        "Recover preserved work.",
    )
    .unwrap_err();
    assert!(error.message.contains("doctor"));
    assert!(
        fixture
            .store
            .transaction(|r| r.operation(operation))
            .unwrap()
            .is_none()
    );
    fs::rename(&moved, &workspace.path).unwrap();
    request(
        &mut fixture,
        &AuthenticatedCaller::Operator,
        operation,
        workspace.assignment_id,
        "Recover preserved work.",
    )
    .unwrap();
}

#[test]
fn recovery_rechecks_provider_identity_and_refuses_unknown_or_live_observations()
 {
    use crate::providers::{ProviderRecovery, ProviderSessionHandle};
    for condition in [
        "unknown",
        "running",
        "no_exit",
        "wrong_scope",
        "wrong_id",
        "absent",
    ] {
        let (_directory, mut fixture) = fixture();
        exit(&mut fixture);
        let workspace = fixture.workspace();
        let scope = fixture.scope();
        let session = fixture
            .store
            .transaction(|r| r.session(scope.session_id))
            .unwrap()
            .unwrap();
        let mut handle = ProviderSessionHandle::new(
            session.provider_session_id.unwrap(),
            scope,
        );
        let mut observation = SessionObservation::exited(0);
        match condition {
            "running" => {
                observation.lifecycle = LifecycleState::Running;
                observation.exit = None;
            }
            "no_exit" => observation.exit = None,
            "wrong_scope" => handle.scope.generation += 1,
            "wrong_id" => {
                handle = ProviderSessionHandle::new("another-process", scope)
            }
            _ => (),
        }
        let proof = match condition {
            "unknown" => ProviderRecovery::Unknown,
            "absent" => ProviderRecovery::Lost,
            _ => ProviderRecovery::Observed {
                handle,
                observation,
            },
        };
        fixture.sessions = AgentSessionSupervisor::new(
            FakeProvider::new([]).with_missing_recoveries([proof]),
            &fixture.run,
        );
        let operation = OperationId::generate();
        let outcome = request(
            &mut fixture,
            &AuthenticatedCaller::Operator,
            operation,
            workspace.assignment_id,
            "Check process ownership again.",
        );
        if condition == "absent" {
            outcome.unwrap();
        } else {
            assert!(
                outcome
                    .unwrap_err()
                    .message
                    .contains("provider cannot verify"),
                "{condition}"
            );
            assert!(
                fixture
                    .store
                    .transaction(|r| r.operation(operation))
                    .unwrap()
                    .is_none()
            );
        }
    }
}

#[test]
fn recovery_does_not_treat_a_terminal_label_as_provider_exit_evidence() {
    let (_directory, mut fixture) = fixture();
    let workspace = fixture.workspace();
    let scope = fixture.scope();
    fixture
        .store
        .transaction(|r| {
            r.record_session_lifecycle(
                scope,
                LifecycleState::Exited,
                unix_timestamp().unwrap(),
            )?;
            Ok(())
        })
        .unwrap();
    let error = request(
        &mut fixture,
        &AuthenticatedCaller::Operator,
        OperationId::generate(),
        workspace.assignment_id,
        "Not actually observed.",
    )
    .unwrap_err();
    assert!(error.message.contains("process-exit record"));
    assert!(
        fixture
            .store
            .transaction(|r| r.task_recoveries(scope.run_id))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn recovered_source_spawn_replay_preserves_history_before_and_after_continuation_and_restart()
 {
    let (_directory, mut fixture) = fixture();
    let original = fixture.spawn().unwrap();
    prepare(&mut fixture, "continue-assignment");
    fn assert_replay(fixture: &mut Fixture, original: &RpcResponse) {
        let before = snapshot(&fixture.root);
        let replayed = fixture.spawn().unwrap();
        let original = serde_json::to_value(original).unwrap();
        let replayed = serde_json::to_value(replayed).unwrap();
        for field in ["operation_id", "assignment_id", "session_id", "task_id"]
        {
            assert_eq!(replayed[field], original[field]);
        }
        assert_eq!(replayed["agent"]["id"], original["agent"]["id"]);
        let after = snapshot(&fixture.root);
        assert_eq!(before.database, after.database);
        assert_eq!(before.files, after.files);
    }
    assert_replay(&mut fixture, &original);
    exercise(&mut fixture, "continue-assignment");
    assert_replay(&mut fixture, &original);
    let root = fixture.root.clone();
    drop(fixture);
    let mut restarted = Fixture::open(root);
    recover(&mut restarted, "continue-assignment");
    assert_replay(&mut restarted, &original);
    verify(&mut restarted, "continue-assignment");
}

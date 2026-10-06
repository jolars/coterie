use super::*;
use crate::state::retained::RetainedSubmission;

fn fixture() -> (Directory, Fixture) {
    let directory = Directory::new();
    prepare_project(&directory.0);
    let mut fixture = Fixture::open(directory.0.clone());
    fixture.prepare("recovery");
    (directory, fixture)
}

fn submission(fixture: &mut Fixture) -> RetainedSubmission {
    let workspace = fixture.workspace();
    RetainedSubmission {
        assignment_id: workspace.assignment_id,
        result_commit: Repository::open(&workspace.path)
            .unwrap()
            .head()
            .unwrap()
            .peel_to_commit()
            .unwrap()
            .id()
            .to_string(),
        summary: "Validated exact file contents; target tests still pending."
            .into(),
        reason: "Worker exited after the coordinator committed reviewed work."
            .into(),
        review: crate::protocol::recovery::ReportedEvidence {
            text: "Independently reviewed the exact commit and intended paths."
                .into(),
            source: "operator review".into(),
        },
    }
}

fn request(
    fixture: &mut Fixture,
    operation_id: OperationId,
    submission: RetainedSubmission,
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
        &AuthenticatedCaller::Operator,
        RpcRequest::TaskSubmitRetained {
            operation_id,
            submission: Box::new(submission),
        },
    )
}

#[test]
fn retained_submission_preserves_reviewed_commit_and_requires_accepted_closure()
{
    let (_directory, mut fixture) = fixture();
    let workspace = fixture.workspace();
    commit(&workspace.path, "result.txt", "reviewed result\n");
    let submission = submission(&mut fixture);
    let repository = Repository::open(&workspace.path).unwrap();
    let index_path = repository.path().join("index");
    let index = fs::read(&index_path).unwrap();
    let operation = OperationId::generate();
    assert_eq!(
        request(&mut fixture, operation, submission.clone())
            .unwrap_err()
            .code,
        RpcFailureCode::Conflict
    );
    recovery_tests::exit(&mut fixture);
    let response =
        request(&mut fixture, operation, submission.clone()).unwrap();
    assert!(
        matches!(response, RpcResponse::TaskRetainedSubmitted { ref submission, .. }
        if submission.task.status == TaskStatus::Submitted)
    );
    assert_eq!(fs::read(index_path).unwrap(), index);
    assert_eq!(
        fs::read_to_string(workspace.path.join("result.txt")).unwrap(),
        "reviewed result\n"
    );
    fixture
        .store
        .transaction(|r| {
            assert_eq!(r.workspaces(RUN.parse().unwrap())?.len(), 1);
            assert_eq!(
                r.workspace(workspace.assignment_id)?.unwrap().result_commit,
                Some(submission.result_commit.clone())
            );
            assert_eq!(
                r.assignment(workspace.assignment_id)?.unwrap().state,
                "completed"
            );
            Ok(())
        })
        .unwrap();
    let run_id = RUN.parse().unwrap();
    let task_id = TASK.parse().unwrap();
    assert_eq!(
        close_task(
            &mut fixture.store,
            &fixture.workspaces,
            run_id,
            &AuthenticatedCaller::Operator,
            OperationId::generate(),
            task_id,
            "Not integrated yet.".into(),
            None,
            None
        )
        .unwrap_err()
        .code,
        RpcFailureCode::Conflict
    );
    fixture.integrate("integrate");
    assert_eq!(
        fs::read_to_string(fixture.root.join("project/result.txt")).unwrap(),
        "reviewed result\n"
    );
    close_task(
        &mut fixture.store,
        &fixture.workspaces,
        run_id,
        &AuthenticatedCaller::Operator,
        OperationId::generate(),
        task_id,
        "Verified integrated exact file contents.".into(),
        None,
        None,
    )
    .unwrap();
    // Replaying an acknowledged operation must not inspect subsequently edited files.
    fs::write(workspace.path.join("later.txt"), "preserve\n").unwrap();
    assert_eq!(
        request(&mut fixture, operation, submission.clone()).unwrap(),
        response
    );
    assert!(
        request(&mut fixture, OperationId::generate(), submission).is_err()
    );
}

#[test]
fn retained_submission_refuses_dirty_or_stale_review_without_recording_result()
{
    for case in ["before_commit", "after_commit", "stale_review", "staged"] {
        let (_directory, mut fixture) = fixture();
        let workspace = fixture.workspace();
        if case != "before_commit" {
            commit(&workspace.path, "result.txt", "reviewed\n");
        }
        let reviewed = submission(&mut fixture);
        if case == "stale_review" {
            commit(&workspace.path, "later.txt", "unreviewed\n");
        } else {
            fs::write(workspace.path.join("result.txt"), "uncommitted\n")
                .unwrap();
            if case == "staged" {
                let repository = Repository::open(&workspace.path).unwrap();
                let mut index = repository.index().unwrap();
                index.add_path(Path::new("result.txt")).unwrap();
                index.write().unwrap();
            }
        }
        recovery_tests::exit(&mut fixture);
        let operation = OperationId::generate();
        assert_eq!(
            request(&mut fixture, operation, reviewed).unwrap_err().code,
            RpcFailureCode::Conflict
        );
        fixture
            .store
            .transaction(|r| {
                assert!(r.operation(operation)?.is_none());
                assert!(
                    r.workspace(workspace.assignment_id)?
                        .unwrap()
                        .result_commit
                        .is_none()
                );
                assert_eq!(
                    r.task(TASK.parse().unwrap())?.unwrap().status,
                    TaskStatus::InProgress
                );
                Ok(())
            })
            .unwrap();
    }
}

#[test]
fn retained_submission_handles_exit_around_finish_and_fences_recovery() {
    for case in [
        "before_finish",
        "partial_finish",
        "after_finish",
        "after_recovery",
    ] {
        let (_directory, mut fixture) = fixture();
        let workspace = fixture.workspace();
        commit(&workspace.path, "result.txt", "reviewed\n");
        let reviewed = submission(&mut fixture);
        if case == "partial_finish" {
            fixture
                .workspaces
                .record_result_commit(&mut fixture.store, workspace.scope())
                .unwrap();
        }
        if case == "after_finish" {
            fixture.finish();
        }
        recovery_tests::exit(&mut fixture);
        if case == "after_recovery" {
            recovery::recover_task(
                &mut fixture.store,
                &mut fixture.sessions,
                &fixture.workspaces,
                RUN.parse().unwrap(),
                &AuthenticatedCaller::Operator,
                OperationId::generate(),
                workspace.assignment_id,
                "Continue in a fresh workspace.".into(),
                None,
                false,
                None,
            )
            .unwrap();
        }
        let outcome = request(&mut fixture, OperationId::generate(), reviewed);
        if matches!(case, "after_finish" | "after_recovery") {
            assert_eq!(outcome.unwrap_err().code, RpcFailureCode::Conflict);
        } else {
            outcome.unwrap();
        }
    }
}

#[test]
fn retained_submission_requires_fresh_exit_proof_and_rejects_lost_sessions() {
    use crate::providers::{ProviderRecovery, ProviderSessionHandle};
    for proof in ["absent", "unknown", "live", "lost"] {
        let (_directory, mut fixture) = fixture();
        let reviewed = submission(&mut fixture);
        let scope = fixture.scope();
        let provider_id = fixture
            .store
            .transaction(|r| r.session(scope.session_id))
            .unwrap()
            .unwrap()
            .provider_session_id
            .unwrap();
        if proof != "lost" {
            recovery_tests::exit(&mut fixture);
        }
        let observation = match proof {
            "unknown" => ProviderRecovery::Unknown,
            "live" => ProviderRecovery::Observed {
                handle: ProviderSessionHandle::new(provider_id, scope),
                observation: SessionObservation {
                    lifecycle: LifecycleState::Running,
                    activity: ActivityState::Unknown,
                    exit: None,
                },
            },
            _ => ProviderRecovery::Lost,
        };
        fixture.sessions = AgentSessionSupervisor::new(
            FakeProvider::new([]).with_missing_recoveries([observation]),
            &fixture.run,
        );
        if proof == "lost" {
            fixture
                .sessions
                .reconcile_after_restart(
                    &mut fixture.store,
                    scope.run_id,
                    unix_timestamp().unwrap(),
                )
                .unwrap();
        }
        let outcome = request(&mut fixture, OperationId::generate(), reviewed);
        if proof == "absent" {
            outcome.unwrap();
        } else {
            assert_eq!(outcome.unwrap_err().code, RpcFailureCode::Conflict);
        }
    }
}

#[test]
fn retained_submission_authorizes_callers_and_fences_replays() {
    let (_directory, mut fixture) = fixture();
    let worker = fixture.scope();
    let reviewed = submission(&mut fixture);
    let call = |fixture: &mut Fixture,
                caller: &AuthenticatedCaller,
                operation,
                submission| {
        retained::submit_retained(
            &mut fixture.store,
            &mut fixture.sessions,
            &fixture.workspaces,
            RUN.parse().unwrap(),
            caller,
            operation,
            submission,
            None,
        )
    };
    assert_eq!(
        call(
            &mut fixture,
            &AuthenticatedCaller::Agent(worker),
            OperationId::generate(),
            reviewed.clone()
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
        panic!("foreground expected");
    };
    let scope = SessionScope {
        run_id,
        agent_id: agent.id,
        session_id,
        generation,
    };
    let caller = AuthenticatedCaller::Agent(scope);
    recovery_tests::exit(&mut fixture);
    let operation = OperationId::generate();
    let response =
        call(&mut fixture, &caller, operation, reviewed.clone()).unwrap();
    assert_eq!(
        call(&mut fixture, &caller, operation, reviewed.clone()).unwrap(),
        response
    );
    let mut changed = reviewed.clone();
    changed.review.source = "different evidence".into();
    assert_eq!(
        call(&mut fixture, &caller, operation, changed)
            .unwrap_err()
            .code,
        RpcFailureCode::Conflict
    );
    let stale = AuthenticatedCaller::Agent(SessionScope {
        generation: generation + 1,
        ..scope
    });
    assert_eq!(
        call(&mut fixture, &stale, operation, reviewed.clone())
            .unwrap_err()
            .code,
        RpcFailureCode::Unauthenticated
    );
    assert_eq!(
        call(
            &mut fixture,
            &AuthenticatedCaller::Agent(worker),
            operation,
            reviewed
        )
        .unwrap_err()
        .code,
        RpcFailureCode::Unauthenticated
    );
}

#[test]
fn retained_submission_rejects_invalid_evidence_and_redacts_before_storage() {
    let (_directory, mut fixture) = fixture();
    recovery_tests::exit(&mut fixture);
    let reviewed = submission(&mut fixture);
    for field in ["commit", "summary", "reason", "review", "source"] {
        let mut invalid = reviewed.clone();
        match field {
            "commit" => invalid.result_commit = "HEAD".into(),
            "summary" => invalid.summary = " ".into(),
            "reason" => invalid.reason = " ".into(),
            "review" => invalid.review.text = " ".into(),
            "source" => invalid.review.source = " ".into(),
            _ => unreachable!(),
        }
        let operation = OperationId::generate();
        assert_eq!(
            request(&mut fixture, operation, invalid).unwrap_err().code,
            RpcFailureCode::InvalidArgument
        );
        assert!(
            fixture
                .store
                .transaction(|r| r.operation(operation))
                .unwrap()
                .is_none()
        );
    }
    let secret = format!("cot1_{}", "a".repeat(64));
    let mut reviewed = reviewed;
    reviewed.reason = secret.clone();
    reviewed.summary = secret.clone();
    reviewed.review.text = secret.clone();
    reviewed.review.source = secret.clone();
    let operation = OperationId::generate();
    let response = request(&mut fixture, operation, reviewed.clone()).unwrap();
    assert!(!serde_json::to_string(&response).unwrap().contains(&secret));
    fixture
        .store
        .transaction(|r| {
            let operation = r.operation(operation)?.unwrap();
            assert!(!operation.request.to_string().contains(&secret));
            assert!(!operation.result.unwrap().to_string().contains(&secret));
            assert!(
                !serde_json::to_string(&r.events_after(
                    RUN.parse().unwrap(),
                    0,
                    1000
                )?)
                .unwrap()
                .contains(&secret)
            );
            Ok(())
        })
        .unwrap();
    reviewed.reason = format!("cot1_{}", "b".repeat(64));
    assert_eq!(
        request(&mut fixture, operation, reviewed).unwrap_err().code,
        RpcFailureCode::Conflict
    );
}

pub(super) fn prepare(fixture: &mut Fixture) {
    let workspace = fixture.workspace();
    commit(&workspace.path, "result.txt", "reviewed retained result\n");
    let repository = Repository::open(&workspace.path).unwrap();
    fs::write(
        fixture.run.join("retained-index"),
        fs::read(repository.path().join("index")).unwrap(),
    )
    .unwrap();
    recovery_tests::exit(fixture);
}

pub(super) fn exercise(fixture: &mut Fixture) {
    let reviewed = submission(fixture);
    request(
        fixture,
        "co-01ARZ3NDEKTSV4RRFFQ69G5FC1".parse().unwrap(),
        reviewed,
    )
    .unwrap();
}

pub(super) fn verify(fixture: &mut Fixture) {
    let workspace = fixture.workspace();
    let repository = Repository::open(&workspace.path).unwrap();
    assert_eq!(
        fs::read(repository.path().join("index")).unwrap(),
        fs::read(fixture.run.join("retained-index")).unwrap()
    );
    assert_eq!(
        fs::read_to_string(workspace.path.join("result.txt")).unwrap(),
        "reviewed retained result\n"
    );
    fixture
        .store
        .transaction(|r| {
            assert_eq!(
                r.task(TASK.parse().unwrap())?.unwrap().status,
                TaskStatus::Submitted
            );
            assert_eq!(r.workspaces(RUN.parse().unwrap())?.len(), 1);
            assert_eq!(
                r.events_after(RUN.parse().unwrap(), 0, 1000)?
                    .iter()
                    .filter(|e| e.event_type == "task.lifecycle_changed"
                        && e.payload["data"]["transition"] == "submit_retained")
                    .count(),
                1
            );
            Ok(())
        })
        .unwrap();
    exercise(fixture);
}

#[test]
fn repeated_recovery_keeps_retired_sources_fenced_from_retained_submission() {
    let (_directory, mut fixture) = fixture();
    let mut reviewed = submission(&mut fixture);
    let run_id = RUN.parse().unwrap();
    let mut preserved = Vec::new();
    for _ in 0..2 {
        let assignment = fixture
            .store
            .transaction(|r| r.assignment(reviewed.assignment_id))
            .unwrap()
            .unwrap();
        let workspace = fixture
            .store
            .transaction(|r| r.workspace(assignment.id))
            .unwrap()
            .unwrap();
        fs::write(
            workspace.path.join("dirty.txt"),
            "preserve interrupted edits\n",
        )
        .unwrap();
        fixture
            .sessions
            .terminate(
                &mut fixture.store,
                assignment.session_id.unwrap(),
                unix_timestamp().unwrap(),
            )
            .unwrap();
        fixture
            .sessions
            .drive_controls(
                &mut fixture.store,
                run_id,
                unix_timestamp().unwrap() * 1000,
            )
            .unwrap();
        let operation = OperationId::generate();
        let recover = |fixture: &mut Fixture| {
            recovery::recover_task(
                &mut fixture.store,
                &mut fixture.sessions,
                &fixture.workspaces,
                run_id,
                &AuthenticatedCaller::Operator,
                operation,
                assignment.id,
                "Continue interrupted work.".into(),
                None,
                false,
                None,
            )
        };
        let response = recover(&mut fixture).unwrap();
        let repository = Repository::open(&workspace.path).unwrap();
        let index_path = repository.path().join("index");
        preserved.push((
            workspace.path.clone(),
            index_path.clone(),
            fs::read(index_path).unwrap(),
        ));
        assert!(
            request(&mut fixture, OperationId::generate(), reviewed.clone())
                .is_err()
        );
        let RpcResponse::Spawned { assignment_id, .. } = spawn_agent(
            fixture.runtime(),
            &AuthenticatedCaller::Operator,
            OperationId::generate(),
            "worker".into(),
            TASK.parse().unwrap(),
        )
        .unwrap() else {
            panic!("spawn expected");
        };
        assert_eq!(recover(&mut fixture).unwrap(), response);
        let workspace = fixture
            .store
            .transaction(|r| r.workspace(assignment_id))
            .unwrap()
            .unwrap();
        reviewed.assignment_id = assignment_id;
        reviewed.result_commit = workspace.base_commit.unwrap();
    }
    let assignment = fixture
        .store
        .transaction(|r| r.assignment(reviewed.assignment_id))
        .unwrap()
        .unwrap();
    fixture
        .sessions
        .terminate(
            &mut fixture.store,
            assignment.session_id.unwrap(),
            unix_timestamp().unwrap(),
        )
        .unwrap();
    fixture
        .sessions
        .drive_controls(
            &mut fixture.store,
            run_id,
            unix_timestamp().unwrap() * 1000,
        )
        .unwrap();
    request(&mut fixture, OperationId::generate(), reviewed).unwrap();
    for (workspace, index_path, index) in preserved {
        assert_eq!(fs::read(index_path).unwrap(), index);
        assert_eq!(
            fs::read_to_string(workspace.join("dirty.txt")).unwrap(),
            "preserve interrupted edits\n"
        );
    }
}

#[test]
fn retained_submission_rejects_hidden_index_and_unowned_head() {
    for case in ["hidden", "detached", "unfinished", "recorded_mismatch"] {
        let (_directory, mut fixture) = fixture();
        let workspace = fixture.workspace();
        commit(&workspace.path, "result.txt", "reviewed\n");
        let reviewed = submission(&mut fixture);
        let repository = Repository::open(&workspace.path).unwrap();
        match case {
            "hidden" => {
                let mut index = repository.index().unwrap();
                let mut entry =
                    index.get_path(Path::new("result.txt"), 0).unwrap();
                entry.flags |= git2::IndexEntryFlag::VALID.bits();
                index.add(&entry).unwrap();
                index.write().unwrap();
            }
            "detached" => repository
                .set_head_detached(reviewed.result_commit.parse().unwrap())
                .unwrap(),
            "unfinished" => fs::write(
                repository.path().join("MERGE_HEAD"),
                &reviewed.result_commit,
            )
            .unwrap(),
            "recorded_mismatch" => {
                fixture
                    .store
                    .transaction(|r| {
                        r.record_workspace_result_commit(
                            workspace.scope(),
                            workspace.base_commit.as_deref().unwrap(),
                        )
                    })
                    .unwrap();
            }
            _ => unreachable!(),
        }
        let index = fs::read(repository.path().join("index")).unwrap();
        recovery_tests::exit(&mut fixture);
        let operation = OperationId::generate();
        assert_eq!(
            request(&mut fixture, operation, reviewed).unwrap_err().code,
            if case == "detached" {
                RpcFailureCode::Unavailable
            } else {
                RpcFailureCode::Conflict
            },
            "{case}"
        );
        assert_eq!(fs::read(repository.path().join("index")).unwrap(), index);
        assert!(
            fixture
                .store
                .transaction(|r| r.operation(operation))
                .unwrap()
                .is_none()
        );
    }
}

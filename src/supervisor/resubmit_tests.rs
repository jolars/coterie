use super::*;

fn fixture() -> (Directory, Fixture) {
    let directory = Directory::new();
    prepare_project(&directory.0);
    let mut fixture = Fixture::open(directory.0.clone());
    fixture.prepare("resubmit");
    (directory, fixture)
}

fn request(
    fixture: &mut Fixture,
    operation_id: OperationId,
    submission: crate::state::resubmit::Resubmission,
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
        RpcRequest::TaskResubmit {
            operation_id,
            submission,
        },
    )
}

#[test]
fn resubmit_redacts_reasons_and_preserves_retry_identity_across_corrections() {
    let (_directory, mut fixture) = fixture();
    let mut correction = fixture.correction();
    let secret = format!("cot1_{}", "a".repeat(64));
    correction.reason = format!("Remove accidental credential {secret}.");
    let first_operation = OperationId::generate();
    let first =
        request(&mut fixture, first_operation, correction.clone()).unwrap();
    let operation = fixture
        .store
        .transaction(|r| r.operation(first_operation))
        .unwrap()
        .unwrap();
    assert!(!operation.request.to_string().contains(&secret));
    assert!(!operation.result.unwrap().to_string().contains(&secret));
    fixture
        .store
        .transaction(|r| {
            let event = r
                .events_after(RUN.parse().unwrap(), 0, 1000)?
                .into_iter()
                .find(|event| event.event_type == "task.resubmitted")
                .unwrap();
            assert!(!event.payload.to_string().contains(&secret));
            Ok(())
        })
        .unwrap();
    let mut different = correction.clone();
    different.reason =
        format!("Remove accidental credential cot1_{}.", "b".repeat(64));
    assert_eq!(
        request(&mut fixture, first_operation, different)
            .unwrap_err()
            .code,
        RpcFailureCode::Conflict
    );
    let workspace = fixture.workspace();
    commit(
        &workspace.path,
        "another-correction.txt",
        "another validated correction\n",
    );
    let mut next = fixture.correction();
    next.expected_result = correction.result_commit.clone();
    request(&mut fixture, OperationId::generate(), next.clone()).unwrap();
    assert_eq!(
        request(&mut fixture, first_operation, correction).unwrap(),
        first
    );
    assert_eq!(fixture.workspace().result_commit, Some(next.result_commit));
}

#[test]
fn resubmit_rejects_invalid_inputs_without_recording_a_mutation() {
    let (_directory, mut fixture) = fixture();
    let correction = fixture.correction();
    for field in ["expected", "replacement", "unchanged", "summary", "reason"] {
        let mut invalid = correction.clone();
        match field {
            "expected" => invalid.expected_result = "HEAD".into(),
            "replacement" => invalid.result_commit = "abc".into(),
            "unchanged" => {
                invalid.result_commit = invalid.expected_result.clone()
            }
            "summary" => invalid.summary = " ".into(),
            "reason" => invalid.reason = " ".into(),
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
}

#[test]
fn resubmit_refuses_every_existing_integration_intent() {
    for state in [
        ExternalResourceState::Desired,
        ExternalResourceState::Unknown,
        ExternalResourceState::Lost,
        ExternalResourceState::Observed,
    ] {
        let (_directory, mut fixture) = fixture();
        let correction = fixture.correction();
        let mutation = Mutation {
            id: OperationId::generate(),
            run_id: RUN.parse().unwrap(),
            actor_agent_id: None,
            kind: "workspace.integrate".to_owned(),
            request: json!({"assignment_id": correction.assignment_id}),
            created_at: 1,
        };
        fixture
            .store
            .mutate(&mutation, |r| {
                r.mark_operation_reconciliation_desired(mutation.id)?;
                Ok(json!({"preserve": "original integration intent"}))
            })
            .unwrap();
        fixture
            .store
            .transaction(|r| {
                r.record_operation_reconciliation(mutation.id, state, None, 2)?;
                Ok(())
            })
            .unwrap();
        let operation = OperationId::generate();
        let error = resubmit::resubmit_task(
            &mut fixture.store,
            &fixture.workspaces,
            RUN.parse().unwrap(),
            &AuthenticatedCaller::Operator,
            operation,
            correction.clone(),
            None,
        )
        .unwrap_err();
        assert_eq!(error.code, RpcFailureCode::Conflict);
        assert!(error.message.contains("integration intent"));
        assert!(error.message.contains("doctor"));
        fixture
            .store
            .transaction(|r| {
                assert!(r.operation(operation)?.is_none());
                assert_eq!(
                    r.workspace(correction.assignment_id)?
                        .unwrap()
                        .result_commit,
                    Some(correction.expected_result.clone())
                );
                assert!(
                    !r.events_after(RUN.parse().unwrap(), 0, 1000)?
                        .iter()
                        .any(|event| event.event_type == "task.resubmitted")
                );
                Ok(())
            })
            .unwrap();
    }
}

#[test]
fn resubmit_authorizes_capabilities_and_fences_stale_sessions_before_replay() {
    let (_directory, mut fixture) = fixture();
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
        panic!("expected foreground session");
    };
    let scope = SessionScope {
        run_id,
        agent_id: agent.id,
        session_id,
        generation,
    };
    let correction = fixture.correction();
    let operation = OperationId::generate();
    let authorized = AuthenticatedCaller::Agent(scope);
    let response = resubmit::resubmit_task(
        &mut fixture.store,
        &fixture.workspaces,
        run_id,
        &authorized,
        operation,
        correction.clone(),
        None,
    )
    .unwrap();
    assert!(matches!(response, RpcResponse::TaskResubmitted { .. }));
    for stale in [
        SessionScope {
            generation: generation + 1,
            ..scope
        },
        SessionScope {
            run_id: RunId::generate(),
            ..scope
        },
        SessionScope {
            session_id: SessionId::generate(),
            ..scope
        },
    ] {
        let error = resubmit::resubmit_task(
            &mut fixture.store,
            &fixture.workspaces,
            run_id,
            &AuthenticatedCaller::Agent(stale),
            operation,
            correction.clone(),
            None,
        )
        .unwrap_err();
        assert_eq!(error.code, RpcFailureCode::Unauthenticated);
    }
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
    let error = resubmit::resubmit_task(
        &mut fixture.store,
        &fixture.workspaces,
        run_id,
        &authorized,
        operation,
        correction,
        None,
    )
    .unwrap_err();
    assert_eq!(error.code, RpcFailureCode::Unauthenticated);
}

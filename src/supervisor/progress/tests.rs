use super::*;
use crate::project::ProjectIdentity;
use crate::protocol::progress::ProgressState;

struct Fixture {
    store: Store,
    active: ActiveRunEntry,
    scope: SessionScope,
    token: AgentToken,
}

impl Fixture {
    fn new(mut store: Store, root: &Path, allowed: bool) -> Self {
        let active = ActiveRunEntry::new(
            RunId::generate(),
            ProjectId::generate(),
            ProjectIdentity::Directory {
                canonical_directory: root.to_owned(),
            },
        );
        let global = include_str!("../../../examples/config/global.toml");
        let global = if allowed {
            global.to_owned()
        } else {
            global.replace(
                "\"spawn:builder\", \"send:*\", \"task:*\", \"logs:*\"",
                "\"send:*\"",
            )
        };
        let config = crate::config::resolve(
            &toml::from_str(&global).unwrap(),
            &Default::default(),
            &Default::default(),
        )
        .unwrap();
        store
            .transaction(|r| {
                r.insert_run(&RunRecord {
                    id: active.run_id,
                    status: "active".into(),
                    created_at: 1,
                    stopped_at: None,
                })?;
                r.snapshot_configuration(active.run_id, &config, 1)?;
                r.insert_project(&ProjectRecord {
                    id: active.project_id,
                    run_id: active.run_id,
                    alias: "primary".into(),
                    original_path: root.to_owned(),
                    canonical_path: root.to_owned(),
                    identity: active.project_identity.clone(),
                    is_primary: true,
                    attached_at: 1,
                })
            })
            .unwrap();
        let token = AgentToken::generate().unwrap();
        let RpcResponse::ForegroundPrepared {
            agent,
            session_id,
            generation,
            ..
        } = launch_foreground(
            &mut store,
            active.run_id,
            &AuthenticatedCaller::Operator,
            OperationId::generate(),
            &token,
        )
        .unwrap()
        else {
            panic!("foreground launch")
        };
        assert_eq!(agent.role, "coordinator");
        let scope = SessionScope {
            run_id: active.run_id,
            agent_id: agent.id,
            session_id,
            generation,
        };
        Self {
            store,
            active,
            scope,
            token,
        }
    }

    fn page(
        &mut self,
        after: Option<&str>,
        limit: u16,
    ) -> Result<ProgressPage, RpcFailure> {
        let response = poll(
            &mut self.store,
            self.active.run_id,
            &AuthenticatedCaller::Agent(self.scope),
            after,
            limit,
            0,
        )?;
        Ok(page(response))
    }

    fn task(&mut self, title: &str) -> TaskId {
        let RpcResponse::TaskCreated { task, .. } = create_task(
            &mut self.store,
            self.active.run_id,
            &AuthenticatedCaller::Operator,
            OperationId::generate(),
            title.into(),
            "private task description".repeat(100),
            "primary".into(),
            None,
            Vec::new(),
            None,
        )
        .unwrap() else {
            panic!("task creation")
        };
        task.id
    }
}

fn page(response: RpcResponse) -> ProgressPage {
    let RpcResponse::Progress { page } = response else {
        panic!("progress page")
    };
    page
}

#[test]
fn progress_authorizes_custom_roles_and_rejects_cursors_from_other_scopes() {
    let mut fixture =
        Fixture::new(Store::open_in_memory().unwrap(), Path::new("/tmp"), true);
    let initial = fixture.page(None, 100).unwrap();
    assert!(!initial.has_more);
    assert!(!initial.timed_out);
    assert_eq!(fixture.page(None, 100).unwrap(), initial);
    let cursor = initial.next_cursor;
    assert!(fixture.page(Some(&cursor), 100).unwrap().changes.is_empty());
    let task = fixture.task("hidden task title");
    let changed = fixture.page(Some(&cursor), 100).unwrap();
    assert!(changed.changes.iter().any(|c| matches!(c.state, ProgressState::Task { task_id, status: TaskStatus::Open, .. } if task_id == task)));
    for invalid in [
        "".to_owned(),
        "0".into(),
        cursor.replace("p1:", "p2:"),
        cursor.replace(
            &fixture.active.run_id.to_string(),
            &RunId::generate().to_string(),
        ),
        cursor.replace(&fixture.scope.agent_id.to_string(), "operator"),
        format!(
            "{}999999",
            cursor_prefix(
                fixture.active.run_id,
                &AuthenticatedCaller::Agent(fixture.scope)
            )
        ),
        format!(
            "{}-1",
            cursor_prefix(
                fixture.active.run_id,
                &AuthenticatedCaller::Agent(fixture.scope)
            )
        ),
    ] {
        assert_eq!(
            fixture.page(Some(&invalid), 100).unwrap_err().code,
            RpcFailureCode::InvalidArgument
        );
    }
    let operator = page(
        poll(
            &mut fixture.store,
            fixture.active.run_id,
            &AuthenticatedCaller::Operator,
            None,
            100,
            0,
        )
        .unwrap(),
    );
    assert_eq!(operator.changes, fixture.page(None, 100).unwrap().changes);
    assert_eq!(
        fixture
            .page(Some(&operator.next_cursor), 100)
            .unwrap_err()
            .code,
        RpcFailureCode::InvalidArgument
    );
    let mut denied = Fixture::new(
        Store::open_in_memory().unwrap(),
        Path::new("/tmp"),
        false,
    );
    assert_eq!(
        denied.page(None, 100).unwrap_err().code,
        RpcFailureCode::PermissionDenied
    );
    assert_eq!(
        denied.page(Some(&cursor), 100).unwrap_err().code,
        RpcFailureCode::PermissionDenied
    );
    assert!(
        !available_commands(
            &mut denied.store,
            denied.active.run_id,
            &AuthenticatedCaller::Agent(denied.scope)
        )
        .unwrap()
        .iter()
        .any(|c| c == "progress")
    );
    assert!(
        available_commands(
            &mut fixture.store,
            fixture.active.run_id,
            &AuthenticatedCaller::Agent(fixture.scope)
        )
        .unwrap()
        .iter()
        .any(|c| c == "progress")
    );
}

#[test]
fn progress_cursor_survives_session_renewal_but_requires_fresh_authentication()
{
    let mut fixture =
        Fixture::new(Store::open_in_memory().unwrap(), Path::new("/tmp"), true);
    let cursor = fixture.page(None, 100).unwrap().next_cursor;
    let old_scope = fixture.scope;
    observe_foreground_ended(
        &mut fixture.store,
        fixture.active.run_id,
        &AuthenticatedCaller::Operator,
        old_scope,
        ForegroundEnd::LaunchFailed,
    )
    .unwrap();
    let token = AgentToken::generate().unwrap();
    let operation_id = OperationId::generate();
    let RpcResponse::ForegroundPrepared {
        agent,
        session_id,
        generation,
        ..
    } = launch_foreground(
        &mut fixture.store,
        fixture.active.run_id,
        &AuthenticatedCaller::Operator,
        operation_id,
        &token,
    )
    .unwrap()
    else {
        panic!("replacement session")
    };
    assert_eq!(agent.id, old_scope.agent_id);
    assert!(generation > old_scope.generation);
    assert_eq!(
        fixture.page(Some(&cursor), 100).unwrap_err().code,
        RpcFailureCode::Unauthenticated
    );
    assert!(
        authenticate_agent(
            &mut fixture.store,
            old_scope.run_id,
            old_scope.agent_id,
            old_scope.session_id,
            &fixture.token
        )
        .unwrap()
        .is_none()
    );
    fixture.scope = authenticate_agent(
        &mut fixture.store,
        old_scope.run_id,
        agent.id,
        session_id,
        &token,
    )
    .unwrap()
    .unwrap();
    let replaced = fixture.page(Some(&cursor), 100).unwrap();
    assert!(replaced.changes.iter().any(|change| change.state
        == ProgressState::Agent {
            agent_id: agent.id,
            generation,
            state: LifecycleState::Starting,
        }));
    assert!(replaced.changes.iter().any(|change| change.state
        == ProgressState::Session {
            session_id,
            agent_id: agent.id,
            generation,
            state: LifecycleState::Starting,
        }));
    let replay = launch_foreground(
        &mut fixture.store,
        fixture.active.run_id,
        &AuthenticatedCaller::Operator,
        operation_id,
        &token,
    );
    assert_eq!(replay.unwrap_err().code, RpcFailureCode::Conflict);
    assert_eq!(fixture.page(Some(&cursor), 100).unwrap(), replaced);
}

#[test]
fn progress_enforces_rpc_bounds_and_never_confuses_submission_with_exit() {
    let mut fixture =
        Fixture::new(Store::open_in_memory().unwrap(), Path::new("/tmp"), true);
    for limit in [0, 101, u16::MAX] {
        assert_eq!(
            fixture.page(None, limit).unwrap_err().code,
            RpcFailureCode::InvalidArgument
        );
    }
    assert_eq!(
        poll(
            &mut fixture.store,
            fixture.active.run_id,
            &AuthenticatedCaller::Agent(fixture.scope),
            None,
            100,
            6
        )
        .unwrap_err()
        .code,
        RpcFailureCode::InvalidArgument
    );
    let cursor = fixture.page(None, 100).unwrap().next_cursor;
    for _ in 0..3 {
        let task = fixture.task("private title");
        fixture
            .store
            .claim_task(&ClaimTaskMutation {
                operation_id: OperationId::generate(),
                run_id: fixture.active.run_id,
                actor_agent_id: Some(fixture.scope.agent_id),
                task_id: task,
                agent_id: fixture.scope.agent_id,
                assignment_id: AssignmentId::generate(),
                claimed_at: 2,
            })
            .unwrap();
        fixture
            .store
            .transition_task(&TaskTransitionMutation {
                operation_id: OperationId::generate(),
                run_id: fixture.active.run_id,
                actor_agent_id: Some(fixture.scope.agent_id),
                task_id: task,
                transition: TaskTransition::Submit,
                result: None,
                summary: Some("private summary".into()),
                transitioned_at: 3,
            })
            .unwrap();
    }
    let submissions = fixture.page(Some(&cursor), 100).unwrap();
    assert_eq!(
        submissions
            .changes
            .iter()
            .filter(|c| matches!(
                c.state,
                ProgressState::Task {
                    status: TaskStatus::Submitted,
                    ..
                }
            ))
            .count(),
        3
    );
    assert_eq!(
        submissions
            .changes
            .iter()
            .filter(|c| matches!(
                c.state,
                ProgressState::Assignment {
                    state:
                        crate::protocol::progress::AssignmentState::Completed,
                    ..
                }
            ))
            .count(),
        3
    );
    assert!(!submissions.changes.iter().any(|c| matches!(
        c.state,
        ProgressState::Session {
            state: LifecycleState::Exited,
            ..
        } | ProgressState::Agent {
            state: LifecycleState::Exited,
            ..
        }
    )));
    let encoded = serde_json::to_string(&submissions).unwrap();
    assert!(!encoded.contains("private"));
    assert!(encoded.len() < 64 * 1024);
    let mut after = cursor;
    let mut paged = Vec::new();
    loop {
        let next = fixture.page(Some(&after), 2).unwrap();
        assert!(next.changes.len() <= 2);
        after = next.next_cursor;
        paged.extend(next.changes);
        if !next.has_more {
            break;
        }
    }
    assert_eq!(paged, submissions.changes);
}

#[tokio::test]
async fn progress_wait_times_out_and_revalidates_the_caller_each_poll() {
    for invalidate in [false, true] {
        let mut fixture = Fixture::new(
            Store::open_in_memory().unwrap(),
            Path::new("/tmp"),
            true,
        );
        let cursor = fixture.page(None, 100).unwrap().next_cursor;
        let scope = fixture.scope;
        let (commands, mut requests) = mpsc::channel(1);
        let server = tokio::spawn(async move {
            let mut polls = 0;
            while let Some(SupervisorCommand::Dispatch {
                caller,
                request:
                    RpcRequest::Progress {
                        after,
                        limit,
                        wait_seconds,
                    },
                response,
            }) = requests.recv().await
            {
                polls += 1;
                let result = poll(
                    &mut fixture.store,
                    fixture.active.run_id,
                    &caller,
                    after.as_deref(),
                    limit,
                    wait_seconds,
                );
                response.send(result).unwrap();
                if invalidate && polls == 1 {
                    observe_foreground_ended(
                        &mut fixture.store,
                        fixture.active.run_id,
                        &AuthenticatedCaller::Operator,
                        scope,
                        ForegroundEnd::LaunchFailed,
                    )
                    .unwrap();
                }
            }
            polls
        });
        let start = Instant::now();
        let result = wait(
            &commands,
            AuthenticatedCaller::Agent(scope),
            Some(cursor),
            100,
            1,
        )
        .await
        .unwrap();
        if invalidate {
            assert!(matches!(
                result,
                RpcResult::Err(RpcFailure {
                    code: RpcFailureCode::Unauthenticated,
                    ..
                })
            ));
        } else {
            let RpcResult::Ok(response) = result else {
                panic!("timeout should succeed")
            };
            let result = page(*response);
            assert!(result.timed_out);
            assert!(result.changes.is_empty());
            assert!(!result.has_more);
            assert!(start.elapsed() >= Duration::from_secs(1));
        }
        drop(commands);
        assert!(server.await.unwrap() >= 2);
    }
}

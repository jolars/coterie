use super::*;
use crate::protocol::admission::AdmissionView;

fn fixture(project_policy: &str) -> (Directory, Fixture) {
    let directory = Directory::new();
    prepare_project(&directory.0);
    let root = directory.0.clone();
    let project = DiscoveredProject::discover(root.join("project")).unwrap();
    let active = ActiveRunEntry::new(
        RUN.parse().unwrap(),
        PROJECT.parse().unwrap(),
        project.identity.clone(),
    );
    let run = root.join("run");
    crate::private_fs::directory(&run).unwrap();
    let config = crate::config::resolve(
        &Default::default(),
        &toml::from_str(project_policy).unwrap(),
        &Default::default(),
    )
    .unwrap();
    let store =
        initialize_store_with_configuration(&run, &active, &project, &config)
            .unwrap();
    let fixture = Fixture {
        socket: root.join("supervisor.sock"),
        root,
        sessions: runtime_sessions(&run),
        workspaces: WorkspaceSupervisor::new(GitWorkspace::new(&run)),
        run,
        store,
    };
    (directory, fixture)
}

fn task(fixture: &mut Fixture) -> TaskId {
    let RpcResponse::TaskCreated { task, .. } = create_task(
        &mut fixture.store,
        RUN.parse().unwrap(),
        &AuthenticatedCaller::Operator,
        OperationId::generate(),
        "Admission evidence".into(),
        "Explicit acceptance remains required.".into(),
        "primary".into(),
        None,
        Vec::new(),
        None,
    )
    .unwrap() else {
        panic!("task");
    };
    task.id
}

fn spawn(
    fixture: &mut Fixture,
    task_id: TaskId,
    operation: OperationId,
) -> Result<RpcResponse, RpcFailure> {
    spawn_agent(
        fixture.runtime(),
        &AuthenticatedCaller::Operator,
        operation,
        "worker".into(),
        task_id,
    )
}

fn capacity(fixture: &mut Fixture) -> AdmissionView {
    let RpcResponse::Prime { page } = context::prime(
        &mut fixture.store,
        RUN.parse().unwrap(),
        &AuthenticatedCaller::Operator,
        None,
        20,
    )
    .unwrap() else {
        panic!("prime");
    };
    *page.admission.unwrap()
}

#[test]
fn admission_exited_workers_and_repeated_recoveries_keep_lifetime_usage() {
    let (_directory, mut fixture) = fixture("[limits]\nmax_agents_per_run = 2");
    let task_id = task(&mut fixture);
    let mut assignments = Vec::new();
    for expected in 1..=2 {
        let operation = OperationId::generate();
        let response = spawn(&mut fixture, task_id, operation).unwrap();
        assert_eq!(spawn(&mut fixture, task_id, operation).unwrap(), response);
        let RpcResponse::Spawned {
            session_id,
            assignment_id,
            ..
        } = response
        else {
            panic!("spawn");
        };
        assignments.push(assignment_id);
        fixture
            .sessions
            .terminate(
                &mut fixture.store,
                session_id,
                unix_timestamp().unwrap(),
            )
            .unwrap();
        fixture
            .sessions
            .drive_controls(
                &mut fixture.store,
                RUN.parse().unwrap(),
                unix_timestamp().unwrap() * 1000,
            )
            .unwrap();
        let before = capacity(&mut fixture);
        assert_eq!(before.agents_per_run.used, expected);
        assert_eq!(before.concurrent_agents.used, 0);
        assert_eq!(before.spawns_per_minute.used, expected);
        assert_eq!(
            before
                .roles
                .iter()
                .find(|r| r.role.text == "worker")
                .unwrap()
                .active_instances
                .used,
            0
        );
        assert!(before.warning.as_ref().unwrap().contains("review"));
        let recovery_operation = OperationId::generate();
        let recover = |fixture: &mut Fixture| {
            recovery::recover_task(
                &mut fixture.store,
                &mut fixture.sessions,
                &fixture.workspaces,
                RUN.parse().unwrap(),
                &AuthenticatedCaller::Operator,
                recovery_operation,
                assignment_id,
                "Inspect preserved source.".into(),
                None,
                false,
                None,
            )
            .unwrap()
        };
        let recovered = recover(&mut fixture);
        assert_eq!(recover(&mut fixture), recovered);
        let after = capacity(&mut fixture);
        assert_eq!(after.agents_per_run, before.agents_per_run);
        assert_eq!(after.spawns_per_minute, before.spawns_per_minute);
        assert_eq!(after.concurrent_agents, before.concurrent_agents);
    }
    let failed = OperationId::generate();
    let error = spawn(&mut fixture, task_id, failed).unwrap_err();
    assert_eq!(error.code, RpcFailureCode::Conflict);
    for fragment in [
        "max_agents_per_run",
        "used 2",
        "ceiling 2",
        "project:limits.max_agents_per_run",
        "operator",
        "stop the run",
        "acceptance",
    ] {
        assert!(error.message.contains(fragment), "{}", error.message);
    }
    fixture
        .store
        .transaction(|r| {
            assert!(r.operation(failed)?.is_none());
            assert_eq!(r.task(task_id)?.unwrap().status, TaskStatus::Open);
            for assignment in assignments {
                assert!(r.workspace(assignment)?.is_some());
            }
            Ok(())
        })
        .unwrap();
}

#[test]
fn admission_diagnostics_distinguish_rate_role_and_concurrency() {
    for (policy, expected, source) in [
        (
            "[limits]\nmax_concurrent_agents = 1",
            "max_concurrent_agents",
            "project:limits.max_concurrent_agents",
        ),
        (
            "[roles.worker]\nmax_instances = 1",
            "roles.worker.max_instances",
            "project:roles.worker.max_instances",
        ),
        (
            "[limits]\nmax_spawns_per_minute = 1",
            "max_spawns_per_minute",
            "project:limits.max_spawns_per_minute",
        ),
    ] {
        let (_directory, mut fixture) = fixture(policy);
        let first = task(&mut fixture);
        spawn(&mut fixture, first, OperationId::generate()).unwrap();
        let second = task(&mut fixture);
        let operation = OperationId::generate();
        let error = spawn(&mut fixture, second, operation).unwrap_err();
        for fragment in [expected, source, "used 1", "ceiling 1"] {
            assert!(error.message.contains(fragment), "{}", error.message);
        }
        fixture
            .store
            .transaction(|r| {
                assert!(r.operation(operation)?.is_none());
                assert_eq!(r.task(second)?.unwrap().status, TaskStatus::Open);
                assert_eq!(r.agents(RUN.parse().unwrap())?.len(), 1);
                Ok(())
            })
            .unwrap();
        fixture
            .store
            .transaction(|r| {
                let agent = r.agents(RUN.parse().unwrap())?.remove(0);
                assert!(!agent.state.is_terminal());
                Ok(())
            })
            .unwrap();
    }
}

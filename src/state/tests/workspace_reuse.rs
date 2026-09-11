use super::*;

fn fixture(kind: &str) -> (Store, WorkspaceRecord) {
    let mut store = Store::open_in_memory().unwrap();
    let mut records = Records::fixture();
    records.workspace.kind = kind.into();
    records.workspace.path = records.project.canonical_path.clone();
    insert_claim_prerequisites(&mut store, &records);
    store
        .transaction(|r| {
            r.insert_session(&records.session)?;
            r.insert_operation(&records.operation)?;
            r.insert_claim(&records.claim)?;
            r.insert_assignment(&records.assignment)?;
            r.insert_workspace(&records.workspace)
        })
        .unwrap();
    (store, records.workspace)
}

fn next_binding(
    store: &mut Store,
    first: &WorkspaceRecord,
    kind: &str,
) -> WorkspaceRecord {
    let mut records = Records::fixture();
    records.agent.id = AgentId::generate();
    records.task.id = TaskId::generate();
    records.operation.id = OperationId::generate();
    records.operation.actor_agent_id = Some(records.agent.id);
    records.claim.id = 2;
    records.claim.task_id = records.task.id;
    records.claim.agent_id = records.agent.id;
    records.claim.operation_id = records.operation.id;
    records.assignment.id = AssignmentId::generate();
    records.assignment.agent_id = records.agent.id;
    records.assignment.task_id = records.task.id;
    records.assignment.claim_id = records.claim.id;
    records.assignment.session_id = None;
    store
        .transaction(|r| {
            r.insert_agent(&records.agent)?;
            r.insert_task(&records.task)?;
            r.insert_operation(&records.operation)?;
            r.insert_claim(&records.claim)?;
            r.insert_assignment(&records.assignment)
        })
        .unwrap();
    WorkspaceRecord {
        assignment_id: records.assignment.id,
        kind: kind.into(),
        ..first.clone()
    }
}

#[test]
fn readers_share_project_paths_but_worktrees_never_share() {
    for (first_kind, next_kind, allowed) in [
        ("read_only", "read_only", true),
        ("read_only", "project", true),
        ("project", "read_only", true),
        ("project", "project", false),
        ("worktree", "read_only", false),
        ("worktree", "project", false),
        ("worktree", "worktree", false),
        ("read_only", "worktree", false),
        ("project", "worktree", false),
    ] {
        let (mut store, first) = fixture(first_kind);
        let next = next_binding(&mut store, &first, next_kind);
        let result = store.transaction(|r| r.insert_workspace(&next));
        assert_eq!(
            result.is_ok(),
            allowed,
            "{first_kind} -> {next_kind}: {result:?}"
        );
        if !allowed {
            let direct = store.connection.execute(
                "INSERT INTO workspaces (assignment_id, run_id, project_id, generation, kind, path, state, created_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'desired', 30)",
                rusqlite::params![next.assignment_id, next.run_id, next.project_id, next.generation, next.kind, super::super::path_bytes(&next.path)],
            );
            assert!(
                direct.is_err(),
                "SQL must also reject {first_kind} -> {next_kind}"
            );
        }
        store
            .transaction(|r| {
                assert_eq!(
                    r.workspace(first.assignment_id)?,
                    Some(first.clone())
                );
                Ok(())
            })
            .unwrap();
    }
}

#[test]
fn another_session_for_the_previous_writer_keeps_ownership_reserved() {
    let (mut store, first) = fixture("project");
    let next = next_binding(&mut store, &first, "project");
    store.connection.execute("UPDATE assignments SET state = 'completed', completed_at = 30 WHERE id = ?1", [first.assignment_id]).unwrap();
    store.connection.execute("UPDATE sessions SET state = 'exited', reconciliation_state = 'observed'", []).unwrap();
    let mut session = Records::fixture().session;
    session.id = SessionId::generate();
    session.generation += 1;
    session.state = LifecycleState::Unknown;
    session.reconciliation_state = ExternalResourceState::Unknown;
    store.transaction(|r| r.insert_session(&session)).unwrap();
    assert!(store.transaction(|r| r.insert_workspace(&next)).is_err());
    store.connection.execute("UPDATE sessions SET state = 'exited', reconciliation_state = 'observed'", []).unwrap();
    store.transaction(|r| r.insert_workspace(&next)).unwrap();
}

#[test]
fn project_writer_requires_terminal_assignment_and_observed_exit() {
    for (state, completed_at, session_state, observed, associated, allowed) in [
        ("active", None, "exited", true, true, false),
        ("draining", None, "exited", true, true, false),
        ("completed", Some(30), "running", true, true, false),
        ("completed", Some(30), "unknown", false, true, false),
        ("completed", Some(30), "lost", true, true, false),
        ("completed", Some(30), "exited", false, true, false),
        ("completed", Some(30), "exited", true, false, false),
        ("completed", Some(30), "exited", true, true, true),
        ("released", Some(30), "exited", true, true, true),
        ("canceled", Some(30), "exited", true, true, true),
    ] {
        let (mut store, first) = fixture("project");
        let next = next_binding(&mut store, &first, "project");
        store.connection.execute("UPDATE assignments SET state = ?1, completed_at = ?2, session_id = ?3 WHERE id = ?4", rusqlite::params![state, completed_at, associated.then_some(SESSION_ID), first.assignment_id]).unwrap();
        store
            .connection
            .execute(
                "UPDATE sessions SET state = ?1, reconciliation_state = ?2",
                [session_state, if observed { "observed" } else { "unknown" }],
            )
            .unwrap();
        let result = store.transaction(|r| r.insert_workspace(&next));
        assert_eq!(
            result.is_ok(),
            allowed,
            "{state}, {session_state}, {observed}, {associated}: {result:?}"
        );
    }
}

#[test]
fn historical_worktree_path_and_workspace_fences_are_retained() {
    let (mut store, first) = fixture("worktree");
    let next = next_binding(&mut store, &first, "read_only");
    store
        .connection
        .execute(
            "UPDATE assignments SET state = 'completed', completed_at = 30",
            [],
        )
        .unwrap();
    store.connection.execute("UPDATE sessions SET state = 'exited', reconciliation_state = 'observed'", []).unwrap();
    assert!(store.transaction(|r| r.insert_workspace(&next)).is_err());
    for sql in [
        "UPDATE workspaces SET assignment_id = 'missing'",
        "UPDATE workspaces SET run_id = 'missing'",
        "UPDATE workspaces SET project_id = 'missing'",
        "UPDATE workspaces SET generation = 3",
        "UPDATE workspaces SET kind = 'read_only'",
        "UPDATE workspaces SET path = x'00'",
        "UPDATE workspaces SET base_commit = NULL",
        "UPDATE workspaces SET state = 'invalid'",
    ] {
        assert!(store.connection.execute(sql, []).is_err(), "{sql}");
    }
    assert!(
        store
            .connection
            .execute(
                "DELETE FROM assignments WHERE id = ?1",
                [first.assignment_id]
            )
            .is_err()
    );
}

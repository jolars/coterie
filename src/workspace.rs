//! Assignment workspace side effects and durable reconciliation.

use serde_json::json;
use thiserror::Error;

use crate::id::{AssignmentId, RunId};
use crate::state::{
    EventKind, ExternalResourceState, NewEvent, ResourceTransitionOutcome,
    Store, StoreError, WorkspaceRecord,
};

/// The workspace boundary implemented by deterministic fakes and, later, Git.
pub(crate) trait WorkspaceBackend {
    fn create(
        &mut self,
        workspace: &WorkspaceRecord,
    ) -> Result<(), WorkspaceBackendError>;

    fn observe(
        &self,
        workspace: &WorkspaceRecord,
    ) -> Result<ExternalResourceState, WorkspaceBackendError>;
}

/// Reconciles durable workspace ownership with an external backend.
pub(crate) struct WorkspaceSupervisor<B> {
    backend: B,
}

impl<B: WorkspaceBackend> WorkspaceSupervisor<B> {
    #[must_use]
    pub(crate) const fn new(backend: B) -> Self {
        Self { backend }
    }

    #[cfg(test)]
    const fn backend(&self) -> &B {
        &self.backend
    }

    #[cfg(test)]
    fn backend_mut(&mut self) -> &mut B {
        &mut self.backend
    }

    /// Ensures one previously recorded workspace intent is materialized once.
    pub(crate) fn materialize(
        &mut self,
        store: &mut Store,
        assignment_id: AssignmentId,
        reconciled_at: i64,
    ) -> Result<ExternalResourceState, WorkspaceError> {
        let workspace = store
            .transaction(|repositories| repositories.workspace(assignment_id))?
            .ok_or(WorkspaceError::MissingIntent { assignment_id })?;
        let observed = self.backend.observe(&workspace)?;
        let state = match (workspace.state, observed) {
            (ExternalResourceState::Desired, ExternalResourceState::Lost) => {
                self.backend.create(&workspace)?;
                ExternalResourceState::Observed
            }
            (_, state) => state,
        };
        self.record_state(store, &workspace, state, reconciled_at)?;
        Ok(state)
    }

    /// Rechecks all durable workspaces without repeating observed side effects.
    pub(crate) fn reconcile_after_restart(
        &mut self,
        store: &mut Store,
        run_id: RunId,
        reconciled_at: i64,
    ) -> Result<(), WorkspaceError> {
        let workspaces = store
            .transaction(|repositories| repositories.workspaces(run_id))?;
        for workspace in workspaces {
            self.materialize(store, workspace.assignment_id, reconciled_at)?;
        }
        Ok(())
    }

    fn record_state(
        &self,
        store: &mut Store,
        workspace: &WorkspaceRecord,
        state: ExternalResourceState,
        reconciled_at: i64,
    ) -> Result<(), WorkspaceError> {
        store.transaction(|repositories| {
            let outcome = repositories.record_workspace_reconciliation_state(
                workspace.assignment_id,
                state,
                reconciled_at,
            )?;
            if outcome == ResourceTransitionOutcome::Applied {
                let assignment = repositories
                    .assignment(workspace.assignment_id)?
                    .ok_or_else(|| StoreError::CorruptAssignmentState {
                        id: workspace.assignment_id,
                        reason: "the workspace assignment does not exist"
                            .to_owned(),
                    })?;
                repositories.append_event(&NewEvent {
                    run_id: workspace.run_id,
                    kind: EventKind::WorkspaceReconciliationChanged,
                    actor: "reconciler".to_owned(),
                    subject: workspace.assignment_id.to_string(),
                    project_id: Some(workspace.project_id),
                    agent_id: Some(assignment.agent_id),
                    task_id: Some(assignment.task_id),
                    operation_id: None,
                    correlation_id: None,
                    causation_id: None,
                    data: json!({
                        "previous_state": workspace.state.as_str(),
                        "state": state.as_str(),
                    }),
                    summary: format!(
                        "Workspace {} reconciliation changed from {} to {}.",
                        workspace.assignment_id, workspace.state, state
                    ),
                    created_at: reconciled_at,
                })?;
            }
            Ok(())
        })?;
        Ok(())
    }
}

/// A deterministic in-memory workspace boundary for supervised-runtime tests.
pub(crate) mod fake {
    use std::collections::BTreeMap;

    use super::{WorkspaceBackend, WorkspaceBackendError};
    use crate::id::AssignmentId;
    use crate::state::{ExternalResourceState, WorkspaceRecord};

    #[derive(Default)]
    pub(crate) struct FakeWorkspace {
        workspaces: BTreeMap<AssignmentId, WorkspaceRecord>,
        fail_creations: usize,
        successful_creations: usize,
    }

    impl FakeWorkspace {
        #[must_use]
        pub(crate) fn new() -> Self {
            Self::default()
        }

        #[cfg(test)]
        #[must_use]
        pub(crate) fn failing_creations(count: usize) -> Self {
            Self {
                fail_creations: count,
                ..Self::default()
            }
        }

        #[cfg(test)]
        pub(crate) const fn successful_creations(&self) -> usize {
            self.successful_creations
        }

        #[cfg(test)]
        pub(crate) fn forget(&mut self, assignment_id: AssignmentId) {
            self.workspaces.remove(&assignment_id);
        }

        #[cfg(test)]
        pub(crate) fn insert_conflict(&mut self, workspace: &WorkspaceRecord) {
            let mut conflicting = workspace.clone();
            conflicting.path = workspace.path.join("conflict");
            self.workspaces.insert(workspace.assignment_id, conflicting);
        }
    }

    impl WorkspaceBackend for FakeWorkspace {
        fn create(
            &mut self,
            workspace: &WorkspaceRecord,
        ) -> Result<(), WorkspaceBackendError> {
            if self.fail_creations > 0 {
                self.fail_creations -= 1;
                return Err(WorkspaceBackendError::InjectedFailure);
            }
            match self.workspaces.get(&workspace.assignment_id) {
                Some(existing) if same_intent(existing, workspace) => Ok(()),
                Some(_) => Err(WorkspaceBackendError::OwnershipConflict {
                    assignment_id: workspace.assignment_id,
                }),
                None => {
                    self.workspaces
                        .insert(workspace.assignment_id, workspace.clone());
                    self.successful_creations += 1;
                    Ok(())
                }
            }
        }

        fn observe(
            &self,
            workspace: &WorkspaceRecord,
        ) -> Result<ExternalResourceState, WorkspaceBackendError> {
            Ok(match self.workspaces.get(&workspace.assignment_id) {
                Some(existing) if same_intent(existing, workspace) => {
                    ExternalResourceState::Observed
                }
                Some(_) => ExternalResourceState::Unknown,
                None => ExternalResourceState::Lost,
            })
        }
    }

    fn same_intent(left: &WorkspaceRecord, right: &WorkspaceRecord) -> bool {
        left.assignment_id == right.assignment_id
            && left.run_id == right.run_id
            && left.project_id == right.project_id
            && left.kind == right.kind
            && left.path == right.path
    }
}

/// A deterministic workspace adapter could not perform an external operation.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub(crate) enum WorkspaceBackendError {
    #[error("the fake workspace creation failed at an injected boundary")]
    InjectedFailure,
    #[error("workspace `{assignment_id}` has conflicting external ownership")]
    OwnershipConflict { assignment_id: AssignmentId },
}

/// A workspace intent could not be reconciled with its external state.
#[derive(Debug, Error)]
pub(crate) enum WorkspaceError {
    #[error(transparent)]
    State(#[from] StoreError),
    #[error(transparent)]
    Backend(#[from] WorkspaceBackendError),
    #[error("assignment `{assignment_id}` has no durable workspace intent")]
    MissingIntent { assignment_id: AssignmentId },
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use serde_json::json;

    use super::WorkspaceSupervisor;
    use super::fake::FakeWorkspace;
    use crate::id::{
        AgentId, AssignmentId, OperationId, ProjectId, RunId, TaskId,
    };
    use crate::project::ProjectIdentity;
    use crate::providers::LifecycleState;
    use crate::state::{
        AgentRecord, AssignmentRecord, ClaimRecord, ExternalResourceState,
        OperationRecord, ProjectRecord, RunRecord, Store, TaskRecord,
        WorkspaceRecord,
    };
    use crate::tasks::TaskStatus;

    const RUN_ID: &str = "cr-01ARZ3NDEKTSV4RRFFQ69G5FAV";
    const PROJECT_ID: &str = "cp-01ARZ3NDEKTSV4RRFFQ69G5FAW";
    const AGENT_ID: &str = "cg-01ARZ3NDEKTSV4RRFFQ69G5FAX";
    const TASK_ID: &str = "ct-01ARZ3NDEKTSV4RRFFQ69G5FAY";
    const ASSIGNMENT_ID: &str = "ca-01ARZ3NDEKTSV4RRFFQ69G5FAZ";
    const OPERATION_ID: &str = "co-01ARZ3NDEKTSV4RRFFQ69G5FB0";

    #[test]
    fn workspace_intent_survives_failure_and_reconciliation_is_idempotent() {
        let (mut store, workspace) = store_with_workspace();
        let mut supervisor =
            WorkspaceSupervisor::new(FakeWorkspace::failing_creations(1));

        assert!(
            supervisor
                .materialize(&mut store, workspace.assignment_id, 11)
                .is_err(),
            "the injected external failure should be visible"
        );
        assert_eq!(
            stored_workspace(&mut store, workspace.assignment_id).state,
            ExternalResourceState::Desired
        );

        assert_eq!(
            supervisor
                .materialize(&mut store, workspace.assignment_id, 12)
                .expect("retry should materialize the desired workspace"),
            ExternalResourceState::Observed
        );
        assert_eq!(
            supervisor
                .materialize(&mut store, workspace.assignment_id, 13)
                .expect("reconciliation should be repeatable"),
            ExternalResourceState::Observed
        );
        assert_eq!(supervisor.backend().successful_creations(), 1);
        assert_eq!(
            stored_workspace(&mut store, workspace.assignment_id).reconciled_at,
            Some(12),
            "an unchanged observation must not rewrite durable state"
        );
    }

    #[test]
    fn reconciliation_marks_a_vanished_observed_workspace_lost_once() {
        let (mut store, workspace) = store_with_workspace();
        let mut supervisor = WorkspaceSupervisor::new(FakeWorkspace::new());
        supervisor
            .materialize(&mut store, workspace.assignment_id, 11)
            .expect("the desired workspace should materialize");
        supervisor.backend_mut().forget(workspace.assignment_id);

        assert_eq!(
            supervisor
                .materialize(&mut store, workspace.assignment_id, 12)
                .expect("the missing side effect should reconcile"),
            ExternalResourceState::Lost
        );
        supervisor
            .materialize(&mut store, workspace.assignment_id, 13)
            .expect("lost reconciliation should be idempotent");

        let (stored, lost_events) = store
            .transaction(|repositories| {
                let stored = repositories
                    .workspace(workspace.assignment_id)?
                    .expect("the workspace should remain durable");
                let lost_events = repositories
                    .events_after(workspace.run_id, 0, 100)?
                    .into_iter()
                    .filter(|event| {
                        event.event_type == "workspace.reconciliation_changed"
                            && event.payload["data"]["state"] == "lost"
                    })
                    .count();
                Ok((stored, lost_events))
            })
            .expect("the reconciliation result should be readable");
        assert_eq!(stored.state, ExternalResourceState::Lost);
        assert_eq!(stored.reconciled_at, Some(12));
        assert_eq!(lost_events, 1);
    }

    #[test]
    fn startup_reconciliation_repairs_a_desired_workspace() {
        let (mut store, workspace) = store_with_workspace();
        let mut supervisor = WorkspaceSupervisor::new(FakeWorkspace::new());

        supervisor
            .reconcile_after_restart(&mut store, workspace.run_id, 11)
            .expect("a desired absent workspace is safe to create");

        assert_eq!(supervisor.backend().successful_creations(), 1);
        assert_eq!(
            stored_workspace(&mut store, workspace.assignment_id).state,
            ExternalResourceState::Observed
        );
    }

    #[test]
    fn conflicting_external_ownership_remains_unknown() {
        let (mut store, workspace) = store_with_workspace();
        let mut supervisor = WorkspaceSupervisor::new(FakeWorkspace::new());
        supervisor.backend_mut().insert_conflict(&workspace);

        assert_eq!(
            supervisor
                .materialize(&mut store, workspace.assignment_id, 11)
                .expect("ambiguous ownership should remain inspectable"),
            ExternalResourceState::Unknown
        );
        assert_eq!(supervisor.backend().successful_creations(), 0);
        assert_eq!(
            stored_workspace(&mut store, workspace.assignment_id).state,
            ExternalResourceState::Unknown
        );
    }

    fn stored_workspace(
        store: &mut Store,
        assignment_id: AssignmentId,
    ) -> WorkspaceRecord {
        store
            .transaction(|repositories| repositories.workspace(assignment_id))
            .expect("the workspace query should succeed")
            .expect("the workspace should exist")
    }

    fn store_with_workspace() -> (Store, WorkspaceRecord) {
        let run_id = RUN_ID.parse::<RunId>().expect("valid run ID");
        let project_id =
            PROJECT_ID.parse::<ProjectId>().expect("valid project ID");
        let agent_id = AGENT_ID.parse::<AgentId>().expect("valid agent ID");
        let task_id = TASK_ID.parse::<TaskId>().expect("valid task ID");
        let assignment_id = ASSIGNMENT_ID
            .parse::<AssignmentId>()
            .expect("valid assignment ID");
        let operation_id = OPERATION_ID
            .parse::<OperationId>()
            .expect("valid operation ID");
        let workspace = WorkspaceRecord {
            assignment_id,
            run_id,
            project_id,
            kind: "worktree".to_owned(),
            path: PathBuf::from("/state/workspaces/assignment"),
            state: ExternalResourceState::Desired,
            base_commit: None,
            result_commit: None,
            target_commit: None,
            created_at: 10,
            reconciled_at: None,
        };
        let mut store = Store::open_in_memory().expect("the store should open");
        store
            .transaction(|repositories| {
                repositories.insert_run(&RunRecord {
                    id: run_id,
                    status: "active".to_owned(),
                    created_at: 1,
                    stopped_at: None,
                })?;
                repositories.insert_project(&ProjectRecord {
                    id: project_id,
                    run_id,
                    alias: "primary".to_owned(),
                    original_path: PathBuf::from("/project"),
                    canonical_path: PathBuf::from("/project"),
                    identity: ProjectIdentity::Directory {
                        canonical_directory: PathBuf::from("/project"),
                    },
                    is_primary: true,
                    attached_at: 2,
                })?;
                repositories.insert_agent(&AgentRecord {
                    id: agent_id,
                    run_id,
                    role: "worker".to_owned(),
                    generation: 0,
                    state: LifecycleState::Starting,
                    created_at: 3,
                })?;
                repositories.insert_task(&TaskRecord {
                    id: task_id,
                    run_id,
                    project_id,
                    group_id: None,
                    title: "Implement".to_owned(),
                    description: "Implement the task.".to_owned(),
                    status: TaskStatus::InProgress,
                    result: None,
                    created_at: 4,
                    updated_at: 5,
                })?;
                repositories.insert_operation(&OperationRecord {
                    id: operation_id,
                    run_id,
                    kind: "agent.spawn".to_owned(),
                    actor_agent_id: None,
                    status: "succeeded".to_owned(),
                    request: json!({"role": "worker", "task_id": task_id}),
                    result: Some(json!({"assignment_id": assignment_id})),
                    attempt_count: 1,
                    created_at: 5,
                    updated_at: 5,
                })?;
                repositories.insert_claim(&ClaimRecord {
                    id: 1,
                    run_id,
                    task_id,
                    agent_id,
                    operation_id,
                    state: "active".to_owned(),
                    claimed_at: 5,
                    released_at: None,
                })?;
                repositories.insert_assignment(&AssignmentRecord {
                    id: assignment_id,
                    run_id,
                    task_id,
                    agent_id,
                    session_id: None,
                    claim_id: 1,
                    generation: 0,
                    state: "active".to_owned(),
                    summary: None,
                    created_at: 5,
                    completed_at: None,
                })?;
                repositories.insert_workspace(&workspace)
            })
            .expect("workspace prerequisites should commit");
        (store, workspace)
    }
}

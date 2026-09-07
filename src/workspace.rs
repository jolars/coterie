//! Assignment workspace side effects and durable reconciliation.

use std::fs;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};

use git2::build::CheckoutBuilder;
use git2::{
    ErrorCode, ObjectType, Oid, Repository, RepositoryState, Signature, Status,
    StatusOptions, Time, WorktreeAddOptions,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use thiserror::Error;

use crate::id::{AssignmentId, RunId};
use crate::project::ProjectIdentity;
use crate::state::{
    EventKind, ExternalResourceState, NewEvent, ProjectRecord,
    ResourceTransitionOutcome, Store, StoreError, WorkspaceRecord,
};

const PROJECT_WORKSPACE: &str = "project";
const WORKTREE_WORKSPACE: &str = "worktree";
const READ_ONLY_WORKSPACE: &str = "read_only";

/// The workspace boundary implemented by Git and deterministic fakes.
pub(crate) trait WorkspaceBackend {
    fn base_commit(
        &self,
        kind: &str,
        project: &ProjectRecord,
    ) -> Result<Option<String>, WorkspaceBackendError>;

    fn create(
        &mut self,
        workspace: &WorkspaceRecord,
        project: &ProjectRecord,
    ) -> Result<(), WorkspaceBackendError>;

    fn observe(
        &self,
        workspace: &WorkspaceRecord,
        project: &ProjectRecord,
    ) -> Result<ExternalResourceState, WorkspaceBackendError>;

    fn result_commit(
        &self,
        workspace: &WorkspaceRecord,
        project: &ProjectRecord,
    ) -> Result<Option<String>, WorkspaceBackendError>;

    fn prepare_integration(
        &self,
        workspace: &WorkspaceRecord,
        project: &ProjectRecord,
        integrated_at: i64,
    ) -> Result<IntegrationPlan, WorkspaceBackendError>;

    fn integrate(
        &mut self,
        workspace: &WorkspaceRecord,
        project: &ProjectRecord,
        plan: &IntegrationPlan,
    ) -> Result<IntegrationRecord, WorkspaceBackendError>;
}

/// Immutable Git observations recorded before an integration side effect.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct IntegrationPlan {
    pub(crate) assignment_id: AssignmentId,
    pub(crate) project_id: crate::id::ProjectId,
    pub(crate) target_reference: String,
    pub(crate) target_commit: String,
    pub(crate) integrated_at: i64,
}

/// Exact Git identities observed after applying an integration plan.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct IntegrationRecord {
    pub(crate) assignment_id: AssignmentId,
    pub(crate) project_id: crate::id::ProjectId,
    pub(crate) target_reference: String,
    pub(crate) base_commit: String,
    pub(crate) result_commit: String,
    pub(crate) target_commit_before: String,
    pub(crate) target_commit: String,
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
        let (workspace, project) = store.transaction(|repositories| {
            let workspace = repositories.workspace(assignment_id)?;
            let project = workspace
                .as_ref()
                .map(|workspace| repositories.project(workspace.project_id))
                .transpose()?
                .flatten();
            Ok((workspace, project))
        })?;
        let workspace =
            workspace.ok_or(WorkspaceError::MissingIntent { assignment_id })?;
        let project = project.ok_or(WorkspaceError::MissingProject {
            project_id: workspace.project_id,
        })?;
        let observed = self.backend.observe(&workspace, &project)?;
        let state = match (workspace.state, observed) {
            (ExternalResourceState::Desired, ExternalResourceState::Lost) => {
                self.backend.create(&workspace, &project)?;
                self.backend.observe(&workspace, &project)?
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

    /// Resolves the immutable commit from which a workspace will be created.
    pub(crate) fn base_commit(
        &self,
        kind: &str,
        project: &ProjectRecord,
    ) -> Result<Option<String>, WorkspaceError> {
        Ok(self.backend.base_commit(kind, project)?)
    }

    /// Observes and durably records the commit produced by an assignment.
    pub(crate) fn record_result_commit(
        &self,
        store: &mut Store,
        assignment_id: AssignmentId,
    ) -> Result<Option<String>, WorkspaceError> {
        let (workspace, project) = store.transaction(|repositories| {
            let workspace = repositories.workspace(assignment_id)?;
            let project = workspace
                .as_ref()
                .map(|workspace| repositories.project(workspace.project_id))
                .transpose()?
                .flatten();
            Ok((workspace, project))
        })?;
        let workspace =
            workspace.ok_or(WorkspaceError::MissingIntent { assignment_id })?;
        let project = project.ok_or(WorkspaceError::MissingProject {
            project_id: workspace.project_id,
        })?;
        let result_commit = self.backend.result_commit(&workspace, &project)?;
        if let Some(result_commit) = result_commit.as_deref() {
            store.transaction(|repositories| {
                repositories.record_workspace_result_commit(
                    assignment_id,
                    result_commit,
                )?;
                Ok(())
            })?;
        }
        Ok(result_commit)
    }

    /// Captures a read-only, immutable plan for one guarded integration.
    pub(crate) fn prepare_integration(
        &self,
        store: &mut Store,
        assignment_id: AssignmentId,
        integrated_at: i64,
    ) -> Result<IntegrationPlan, WorkspaceError> {
        let (workspace, project) = integration_records(store, assignment_id)?;
        Ok(self.backend.prepare_integration(
            &workspace,
            &project,
            integrated_at,
        )?)
    }

    /// Applies a durable integration plan and records its exact Git identities.
    pub(crate) fn integrate(
        &mut self,
        store: &mut Store,
        assignment_id: AssignmentId,
        plan: &IntegrationPlan,
        operation_id: crate::id::OperationId,
        actor: &str,
    ) -> Result<IntegrationRecord, WorkspaceError> {
        let (workspace, project) = integration_records(store, assignment_id)?;
        let integration = self.backend.integrate(&workspace, &project, plan)?;
        store.transaction(|repositories| {
            let outcome = repositories.record_workspace_target_commit(
                assignment_id,
                &integration.target_commit,
            )?;
            if outcome == ResourceTransitionOutcome::Applied {
                let assignment = repositories
                    .assignment(assignment_id)?
                    .ok_or_else(|| StoreError::CorruptAssignmentState {
                        id: assignment_id,
                        reason: "the integrated workspace assignment does not exist"
                            .to_owned(),
                    })?;
                repositories.append_event(&NewEvent {
                    run_id: workspace.run_id,
                    kind: EventKind::WorkspaceIntegrated,
                    actor: actor.to_owned(),
                    subject: assignment_id.to_string(),
                    project_id: Some(workspace.project_id),
                    agent_id: Some(assignment.agent_id),
                    task_id: Some(assignment.task_id),
                    operation_id: Some(operation_id),
                    correlation_id: None,
                    causation_id: None,
                    data: serde_json::to_value(&integration)?,
                    summary: format!(
                        "Integrated workspace {assignment_id} as target commit {}.",
                        integration.target_commit
                    ),
                    created_at: plan.integrated_at,
                })?;
            }
            Ok(())
        })?;
        Ok(integration)
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

fn integration_records(
    store: &mut Store,
    assignment_id: AssignmentId,
) -> Result<(WorkspaceRecord, ProjectRecord), WorkspaceError> {
    let (workspace, project) = store.transaction(|repositories| {
        let workspace = repositories.workspace(assignment_id)?;
        let project = workspace
            .as_ref()
            .map(|workspace| repositories.project(workspace.project_id))
            .transpose()?
            .flatten();
        Ok((workspace, project))
    })?;
    let workspace =
        workspace.ok_or(WorkspaceError::MissingIntent { assignment_id })?;
    let project = project.ok_or(WorkspaceError::MissingProject {
        project_id: workspace.project_id,
    })?;
    Ok((workspace, project))
}

/// Native Git workspace operations rooted in one run's private state.
pub(crate) struct GitWorkspace {
    run_state_directory: PathBuf,
}

impl GitWorkspace {
    #[must_use]
    pub(crate) fn new(run_state_directory: impl AsRef<Path>) -> Self {
        Self {
            run_state_directory: run_state_directory.as_ref().to_owned(),
        }
    }

    fn validate_identity(
        workspace: &WorkspaceRecord,
        project: &ProjectRecord,
    ) -> Result<(), WorkspaceBackendError> {
        if workspace.run_id != project.run_id
            || workspace.project_id != project.id
        {
            return Err(WorkspaceBackendError::OwnershipConflict {
                assignment_id: workspace.assignment_id,
            });
        }
        Ok(())
    }

    fn validate_path(
        &self,
        workspace: &WorkspaceRecord,
        project: &ProjectRecord,
    ) -> Result<(), WorkspaceBackendError> {
        let expected = match workspace.kind.as_str() {
            PROJECT_WORKSPACE | READ_ONLY_WORKSPACE => {
                project.canonical_path.clone()
            }
            WORKTREE_WORKSPACE => self
                .run_state_directory
                .join("workspaces")
                .join(project.id.to_string())
                .join(workspace.assignment_id.to_string()),
            kind => {
                return Err(WorkspaceBackendError::UnsupportedKind {
                    kind: kind.to_owned(),
                });
            }
        };
        if workspace.path != expected {
            return Err(WorkspaceBackendError::UnexpectedPath {
                assignment_id: workspace.assignment_id,
                expected,
                actual: workspace.path.clone(),
            });
        }
        Ok(())
    }

    fn source_repository(
        project: &ProjectRecord,
    ) -> Result<Repository, WorkspaceBackendError> {
        let ProjectIdentity::Git {
            common_directory,
            git_directory,
        } = &project.identity
        else {
            return Err(WorkspaceBackendError::WorktreeRequiresGit {
                project_id: project.id,
            });
        };
        let repository =
            Repository::open(&project.canonical_path).map_err(|source| {
                WorkspaceBackendError::Git {
                    action: "open the target project",
                    path: project.canonical_path.clone(),
                    source,
                }
            })?;
        let observed_common = canonicalize_git_path(
            "resolve the target Git common directory",
            repository.commondir(),
        )?;
        let observed_git = canonicalize_git_path(
            "resolve the target Git directory",
            repository.path(),
        )?;
        if &observed_common != common_directory
            || &observed_git != git_directory
        {
            return Err(WorkspaceBackendError::ProjectIdentityChanged {
                project_id: project.id,
            });
        }
        Ok(repository)
    }

    fn common_repository(
        project: &ProjectRecord,
    ) -> Result<Repository, WorkspaceBackendError> {
        let ProjectIdentity::Git {
            common_directory, ..
        } = &project.identity
        else {
            return Err(WorkspaceBackendError::WorktreeRequiresGit {
                project_id: project.id,
            });
        };
        Repository::open(common_directory).map_err(|source| {
            WorkspaceBackendError::Git {
                action: "open the target Git common directory",
                path: common_directory.clone(),
                source,
            }
        })
    }

    fn observe_worktree(
        &self,
        workspace: &WorkspaceRecord,
        project: &ProjectRecord,
    ) -> Result<ExternalResourceState, WorkspaceBackendError> {
        let repository = Self::common_repository(project)?;
        let name = worktree_name(workspace);
        let registered = match repository.find_worktree(&name) {
            Ok(worktree) => worktree,
            Err(error) if error.code() == ErrorCode::NotFound => {
                if workspace.path.exists() {
                    return Ok(ExternalResourceState::Unknown);
                }
                return self.observe_orphan_reference(&repository, workspace);
            }
            Err(source) => {
                return Err(WorkspaceBackendError::Git {
                    action: "look up the assignment worktree",
                    path: workspace.path.clone(),
                    source,
                });
            }
        };
        if registered.validate().is_err() {
            return Ok(ExternalResourceState::Unknown);
        }
        let Ok(registered_path) = fs::canonicalize(registered.path()) else {
            return Ok(ExternalResourceState::Unknown);
        };
        let Ok(expected_path) = fs::canonicalize(&workspace.path) else {
            return Ok(ExternalResourceState::Unknown);
        };
        if registered_path != expected_path {
            return Ok(ExternalResourceState::Unknown);
        }
        let worktree_repository = match Repository::open(&workspace.path) {
            Ok(repository) => repository,
            Err(_) => return Ok(ExternalResourceState::Unknown),
        };
        if !repository_matches_project(&worktree_repository, project)
            || worktree_repository
                .head()
                .ok()
                .and_then(|head| head.name().ok().map(str::to_owned))
                .as_deref()
                != Some(workspace_reference(workspace).as_str())
        {
            return Ok(ExternalResourceState::Unknown);
        }
        Ok(ExternalResourceState::Observed)
    }

    fn observe_orphan_reference(
        &self,
        repository: &Repository,
        workspace: &WorkspaceRecord,
    ) -> Result<ExternalResourceState, WorkspaceBackendError> {
        let reference =
            match repository.find_reference(&workspace_reference(workspace)) {
                Ok(reference) => reference,
                Err(error) if error.code() == ErrorCode::NotFound => {
                    return Ok(ExternalResourceState::Lost);
                }
                Err(source) => {
                    return Err(WorkspaceBackendError::Git {
                        action: "look up the assignment reference",
                        path: repository.path().to_owned(),
                        source,
                    });
                }
            };
        let recorded_base = workspace
            .base_commit
            .as_deref()
            .and_then(|value| Oid::from_str(value).ok());
        Ok(if reference.target() == recorded_base {
            ExternalResourceState::Lost
        } else {
            ExternalResourceState::Unknown
        })
    }

    fn owned_worktree_repository(
        &self,
        workspace: &WorkspaceRecord,
        project: &ProjectRecord,
    ) -> Result<Repository, WorkspaceBackendError> {
        if self.observe_worktree(workspace, project)?
            != ExternalResourceState::Observed
        {
            return Err(WorkspaceBackendError::OwnershipConflict {
                assignment_id: workspace.assignment_id,
            });
        }
        Repository::open(&workspace.path).map_err(|source| {
            WorkspaceBackendError::Git {
                action: "open the assignment worktree",
                path: workspace.path.clone(),
                source,
            }
        })
    }

    fn prepare_workspace_parent(
        &self,
        project: &ProjectRecord,
    ) -> Result<PathBuf, WorkspaceBackendError> {
        require_real_directory(&self.run_state_directory)?;
        let canonical_root = canonicalize_git_path(
            "resolve the run state directory",
            &self.run_state_directory,
        )?;
        let mut parent = self.run_state_directory.clone();
        for component in ["workspaces".to_owned(), project.id.to_string()] {
            parent.push(component);
            match fs::symlink_metadata(&parent) {
                Ok(metadata)
                    if metadata.file_type().is_symlink()
                        || !metadata.is_dir() =>
                {
                    return Err(WorkspaceBackendError::UnsafeStateDirectory {
                        path: parent,
                    });
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    fs::DirBuilder::new().mode(0o700).create(&parent).map_err(
                        |source| WorkspaceBackendError::Io {
                            action: "create an assignment workspace parent",
                            path: parent.clone(),
                            source,
                        },
                    )?;
                }
                Err(source) => {
                    return Err(WorkspaceBackendError::Io {
                        action: "inspect an assignment workspace parent",
                        path: parent,
                        source,
                    });
                }
            }
            require_real_directory(&parent)?;
            let canonical_parent = canonicalize_git_path(
                "resolve an assignment workspace parent",
                &parent,
            )?;
            if !canonical_parent.starts_with(&canonical_root) {
                return Err(WorkspaceBackendError::UnsafeStateDirectory {
                    path: parent,
                });
            }
        }
        Ok(parent)
    }

    fn integration_inputs(
        &self,
        workspace: &WorkspaceRecord,
        project: &ProjectRecord,
    ) -> Result<IntegrationInputs, WorkspaceBackendError> {
        Self::validate_identity(workspace, project)?;
        self.validate_path(workspace, project)?;
        if workspace.kind != WORKTREE_WORKSPACE {
            return Err(WorkspaceBackendError::UnsupportedIntegrationKind {
                assignment_id: workspace.assignment_id,
                kind: workspace.kind.clone(),
            });
        }
        let base = parse_workspace_commit(
            workspace,
            "base",
            workspace.base_commit.as_deref(),
        )?;
        let result = parse_workspace_commit(
            workspace,
            "result",
            workspace.result_commit.as_deref(),
        )?;
        let worktree = self.owned_worktree_repository(workspace, project)?;
        require_clean_repository(
            &worktree,
            &workspace.path,
            WorkspaceBackendError::DirtyWorkspace {
                assignment_id: workspace.assignment_id,
                path: workspace.path.clone(),
            },
        )?;
        let observed_result = head_oid(
            &worktree,
            "resolve the assignment worktree tip",
            &workspace.path,
        )?;
        if observed_result != result {
            return Err(WorkspaceBackendError::UnexpectedWorkspaceTip {
                assignment_id: workspace.assignment_id,
                expected: result.to_string(),
                actual: observed_result.to_string(),
            });
        }
        require_linear_result_history(&worktree, workspace, base, result)?;

        let target = Self::source_repository(project)?;
        require_clean_repository(
            &target,
            &project.canonical_path,
            WorkspaceBackendError::DirtyTarget {
                project_id: project.id,
                path: project.canonical_path.clone(),
            },
        )?;
        let target_head =
            target.head().map_err(|source| WorkspaceBackendError::Git {
                action: "resolve the integration target HEAD",
                path: project.canonical_path.clone(),
                source,
            })?;
        if !target_head.is_branch() {
            return Err(WorkspaceBackendError::AmbiguousTarget {
                project_id: project.id,
            });
        }
        let target_reference = target_head.name().map_err(|_| {
            WorkspaceBackendError::AmbiguousTarget {
                project_id: project.id,
            }
        })?;
        let target_commit = target_head.target().ok_or(
            WorkspaceBackendError::AmbiguousTarget {
                project_id: project.id,
            },
        )?;
        let target_reference = target_reference.to_owned();
        drop(target_head);
        validate_integration_history(
            &target,
            workspace,
            base,
            result,
            target_commit,
        )?;
        preflight_integration_tree(
            &target,
            workspace,
            base,
            result,
            target_commit,
        )?;

        Ok(IntegrationInputs {
            target_reference,
            target_commit,
        })
    }
}

struct IntegrationInputs {
    target_reference: String,
    target_commit: Oid,
}

impl WorkspaceBackend for GitWorkspace {
    fn base_commit(
        &self,
        kind: &str,
        project: &ProjectRecord,
    ) -> Result<Option<String>, WorkspaceBackendError> {
        match kind {
            PROJECT_WORKSPACE | READ_ONLY_WORKSPACE => Ok(None),
            WORKTREE_WORKSPACE => {
                let repository = Self::source_repository(project)?;
                let commit = repository
                    .head()
                    .and_then(|head| head.peel_to_commit())
                    .map_err(|source| WorkspaceBackendError::Git {
                        action: "resolve the target project's HEAD commit",
                        path: project.canonical_path.clone(),
                        source,
                    })?;
                Ok(Some(commit.id().to_string()))
            }
            kind => Err(WorkspaceBackendError::UnsupportedKind {
                kind: kind.to_owned(),
            }),
        }
    }

    fn create(
        &mut self,
        workspace: &WorkspaceRecord,
        project: &ProjectRecord,
    ) -> Result<(), WorkspaceBackendError> {
        Self::validate_identity(workspace, project)?;
        self.validate_path(workspace, project)?;
        match workspace.kind.as_str() {
            PROJECT_WORKSPACE | READ_ONLY_WORKSPACE => {
                if workspace.path.is_dir() {
                    Ok(())
                } else {
                    Err(WorkspaceBackendError::MissingProjectDirectory {
                        path: workspace.path.clone(),
                    })
                }
            }
            WORKTREE_WORKSPACE => {
                if self.observe_worktree(workspace, project)?
                    == ExternalResourceState::Observed
                {
                    return Ok(());
                }
                let base = workspace.base_commit.as_deref().ok_or(
                    WorkspaceBackendError::MissingBaseCommit {
                        assignment_id: workspace.assignment_id,
                    },
                )?;
                let base = Oid::from_str(base).map_err(|source| {
                    WorkspaceBackendError::InvalidCommit {
                        assignment_id: workspace.assignment_id,
                        value: base.to_owned(),
                        source,
                    }
                })?;
                let parent = self.prepare_workspace_parent(project)?;
                debug_assert_eq!(
                    workspace.path.parent(),
                    Some(parent.as_path())
                );
                let repository = Self::common_repository(project)?;
                repository.find_commit(base).map_err(|source| {
                    WorkspaceBackendError::Git {
                        action: "resolve the recorded base commit",
                        path: repository.path().to_owned(),
                        source,
                    }
                })?;
                let reference_name = workspace_reference(workspace);
                match repository.find_reference(&reference_name) {
                    Ok(reference) if reference.target() == Some(base) => {}
                    Ok(_) => {
                        return Err(WorkspaceBackendError::OwnershipConflict {
                            assignment_id: workspace.assignment_id,
                        });
                    }
                    Err(error) if error.code() == ErrorCode::NotFound => {
                        repository
                            .reference(
                                &reference_name,
                                base,
                                false,
                                "coterie: create assignment workspace",
                            )
                            .map_err(|source| WorkspaceBackendError::Git {
                                action: "create the assignment reference",
                                path: repository.path().to_owned(),
                                source,
                            })?;
                    }
                    Err(source) => {
                        return Err(WorkspaceBackendError::Git {
                            action: "look up the assignment reference",
                            path: repository.path().to_owned(),
                            source,
                        });
                    }
                }
                match repository.find_worktree(&worktree_name(workspace)) {
                    Ok(_) => {
                        return Err(WorkspaceBackendError::OwnershipConflict {
                            assignment_id: workspace.assignment_id,
                        });
                    }
                    Err(error) if error.code() == ErrorCode::NotFound => {}
                    Err(source) => {
                        return Err(WorkspaceBackendError::Git {
                            action: "look up the assignment worktree",
                            path: workspace.path.clone(),
                            source,
                        });
                    }
                }
                if workspace.path.exists() {
                    return Err(WorkspaceBackendError::OwnershipConflict {
                        assignment_id: workspace.assignment_id,
                    });
                }
                let reference = repository
                    .find_reference(&reference_name)
                    .map_err(|source| WorkspaceBackendError::Git {
                        action: "reopen the assignment reference",
                        path: repository.path().to_owned(),
                        source,
                    })?;
                let mut options = WorktreeAddOptions::new();
                options.reference(Some(&reference)).lock(true);
                repository
                    .worktree(
                        &worktree_name(workspace),
                        &workspace.path,
                        Some(&options),
                    )
                    .map_err(|source| WorkspaceBackendError::Git {
                        action: "create the assignment worktree",
                        path: workspace.path.clone(),
                        source,
                    })?;
                Ok(())
            }
            kind => Err(WorkspaceBackendError::UnsupportedKind {
                kind: kind.to_owned(),
            }),
        }
    }

    fn observe(
        &self,
        workspace: &WorkspaceRecord,
        project: &ProjectRecord,
    ) -> Result<ExternalResourceState, WorkspaceBackendError> {
        Self::validate_identity(workspace, project)?;
        self.validate_path(workspace, project)?;
        match workspace.kind.as_str() {
            PROJECT_WORKSPACE | READ_ONLY_WORKSPACE => {
                Ok(if workspace.path.is_dir() {
                    ExternalResourceState::Observed
                } else {
                    ExternalResourceState::Lost
                })
            }
            WORKTREE_WORKSPACE => self.observe_worktree(workspace, project),
            kind => Err(WorkspaceBackendError::UnsupportedKind {
                kind: kind.to_owned(),
            }),
        }
    }

    fn result_commit(
        &self,
        workspace: &WorkspaceRecord,
        project: &ProjectRecord,
    ) -> Result<Option<String>, WorkspaceBackendError> {
        Self::validate_identity(workspace, project)?;
        self.validate_path(workspace, project)?;
        match workspace.kind.as_str() {
            PROJECT_WORKSPACE | READ_ONLY_WORKSPACE => Ok(None),
            WORKTREE_WORKSPACE => {
                let repository =
                    self.owned_worktree_repository(workspace, project)?;
                let commit = repository
                    .head()
                    .and_then(|head| head.peel_to_commit())
                    .map_err(|source| WorkspaceBackendError::Git {
                        action: "resolve the assignment result commit",
                        path: workspace.path.clone(),
                        source,
                    })?;
                Ok(Some(commit.id().to_string()))
            }
            kind => Err(WorkspaceBackendError::UnsupportedKind {
                kind: kind.to_owned(),
            }),
        }
    }

    fn prepare_integration(
        &self,
        workspace: &WorkspaceRecord,
        project: &ProjectRecord,
        integrated_at: i64,
    ) -> Result<IntegrationPlan, WorkspaceBackendError> {
        let inputs = self.integration_inputs(workspace, project)?;
        Ok(IntegrationPlan {
            assignment_id: workspace.assignment_id,
            project_id: project.id,
            target_reference: inputs.target_reference,
            target_commit: inputs.target_commit.to_string(),
            integrated_at,
        })
    }

    fn integrate(
        &mut self,
        workspace: &WorkspaceRecord,
        project: &ProjectRecord,
        plan: &IntegrationPlan,
    ) -> Result<IntegrationRecord, WorkspaceBackendError> {
        if plan.assignment_id != workspace.assignment_id
            || plan.project_id != project.id
        {
            return Err(WorkspaceBackendError::IntegrationPlanMismatch {
                assignment_id: workspace.assignment_id,
            });
        }
        Self::validate_identity(workspace, project)?;
        self.validate_path(workspace, project)?;
        if workspace.kind != WORKTREE_WORKSPACE {
            return Err(WorkspaceBackendError::UnsupportedIntegrationKind {
                assignment_id: workspace.assignment_id,
                kind: workspace.kind.clone(),
            });
        }
        let base = parse_workspace_commit(
            workspace,
            "base",
            workspace.base_commit.as_deref(),
        )?;
        let result = parse_workspace_commit(
            workspace,
            "result",
            workspace.result_commit.as_deref(),
        )?;
        let worktree = self.owned_worktree_repository(workspace, project)?;
        require_clean_repository(
            &worktree,
            &workspace.path,
            WorkspaceBackendError::DirtyWorkspace {
                assignment_id: workspace.assignment_id,
                path: workspace.path.clone(),
            },
        )?;
        let observed_result = head_oid(
            &worktree,
            "resolve the assignment worktree tip",
            &workspace.path,
        )?;
        if observed_result != result {
            return Err(WorkspaceBackendError::UnexpectedWorkspaceTip {
                assignment_id: workspace.assignment_id,
                expected: result.to_string(),
                actual: observed_result.to_string(),
            });
        }
        require_linear_result_history(&worktree, workspace, base, result)?;

        let target = Self::source_repository(project)?;
        let expected_target =
            Oid::from_str(&plan.target_commit).map_err(|source| {
                WorkspaceBackendError::InvalidIntegrationPlanCommit {
                    assignment_id: workspace.assignment_id,
                    value: plan.target_commit.clone(),
                    source,
                }
            })?;
        validate_integration_history(
            &target,
            workspace,
            base,
            result,
            expected_target,
        )?;
        let candidate = integration_candidate(
            &target,
            workspace,
            base,
            result,
            expected_target,
            plan.integrated_at,
        )?;
        let head =
            target.head().map_err(|source| WorkspaceBackendError::Git {
                action: "recheck the integration target HEAD",
                path: project.canonical_path.clone(),
                source,
            })?;
        let actual_reference = head.name().map_err(|_| {
            WorkspaceBackendError::AmbiguousTarget {
                project_id: project.id,
            }
        })?;
        let actual_target =
            head.target()
                .ok_or(WorkspaceBackendError::AmbiguousTarget {
                    project_id: project.id,
                })?;
        if !head.is_branch() || actual_reference != plan.target_reference {
            return Err(WorkspaceBackendError::UnexpectedTargetReference {
                project_id: project.id,
                expected: plan.target_reference.clone(),
                actual: actual_reference.to_owned(),
            });
        }
        if actual_target == candidate.target_commit {
            require_clean_repository(
                &target,
                &project.canonical_path,
                WorkspaceBackendError::DirtyTarget {
                    project_id: project.id,
                    path: project.canonical_path.clone(),
                },
            )?;
            return Ok(candidate.record(workspace, project, plan));
        }
        if actual_target != expected_target {
            return Err(WorkspaceBackendError::UnexpectedTargetTip {
                project_id: project.id,
                expected: expected_target.to_string(),
                actual: actual_target.to_string(),
            });
        }

        if !repository_is_clean(&target)? {
            if !target_matches_candidate(&target, candidate.tree_id)? {
                return Err(WorkspaceBackendError::DirtyTarget {
                    project_id: project.id,
                    path: project.canonical_path.clone(),
                });
            }
        } else if candidate.target_commit != expected_target {
            let tree =
                target.find_tree(candidate.tree_id).map_err(|source| {
                    WorkspaceBackendError::Git {
                        action: "resolve the preflighted integration tree",
                        path: project.canonical_path.clone(),
                        source,
                    }
                })?;
            let mut checkout = CheckoutBuilder::new();
            checkout.safe();
            target
                .checkout_tree(tree.as_object(), Some(&mut checkout))
                .map_err(|source| WorkspaceBackendError::Git {
                    action: "check out the preflighted integration tree",
                    path: project.canonical_path.clone(),
                    source,
                })?;
        }

        candidate.write_commit(&target, workspace, plan)?;
        target
            .reference_matching(
                &plan.target_reference,
                candidate.target_commit,
                true,
                expected_target,
                "coterie: integrate assignment workspace",
            )
            .map_err(|source| {
                if source.code() == ErrorCode::Modified {
                    let actual = target
                        .head()
                        .ok()
                        .and_then(|head| head.target())
                        .map_or_else(
                            || "unknown".to_owned(),
                            |oid| oid.to_string(),
                        );
                    WorkspaceBackendError::UnexpectedTargetTip {
                        project_id: project.id,
                        expected: expected_target.to_string(),
                        actual,
                    }
                } else {
                    WorkspaceBackendError::Git {
                        action: "advance the integration target reference",
                        path: project.canonical_path.clone(),
                        source,
                    }
                }
            })?;
        require_clean_repository(
            &target,
            &project.canonical_path,
            WorkspaceBackendError::DirtyTarget {
                project_id: project.id,
                path: project.canonical_path.clone(),
            },
        )?;
        Ok(candidate.record(workspace, project, plan))
    }
}

struct IntegrationCandidate {
    tree_id: Oid,
    target_commit: Oid,
    commit_buffer: Option<Vec<u8>>,
}

impl IntegrationCandidate {
    fn record(
        &self,
        workspace: &WorkspaceRecord,
        project: &ProjectRecord,
        plan: &IntegrationPlan,
    ) -> IntegrationRecord {
        IntegrationRecord {
            assignment_id: workspace.assignment_id,
            project_id: project.id,
            target_reference: plan.target_reference.clone(),
            base_commit: workspace
                .base_commit
                .clone()
                .expect("an integration candidate has a validated base"),
            result_commit: workspace
                .result_commit
                .clone()
                .expect("an integration candidate has a validated result"),
            target_commit_before: plan.target_commit.clone(),
            target_commit: self.target_commit.to_string(),
        }
    }

    fn write_commit(
        &self,
        repository: &Repository,
        workspace: &WorkspaceRecord,
        _plan: &IntegrationPlan,
    ) -> Result<(), WorkspaceBackendError> {
        let Some(buffer) = self.commit_buffer.as_deref() else {
            return Ok(());
        };
        let written = repository
            .odb()
            .and_then(|database| database.write(ObjectType::Commit, buffer))
            .map_err(|source| WorkspaceBackendError::Git {
                action: "write the integration commit",
                path: repository.path().to_owned(),
                source,
            })?;
        if written != self.target_commit {
            return Err(WorkspaceBackendError::IntegrationPlanMismatch {
                assignment_id: workspace.assignment_id,
            });
        }
        Ok(())
    }
}

fn parse_workspace_commit(
    workspace: &WorkspaceRecord,
    field: &'static str,
    value: Option<&str>,
) -> Result<Oid, WorkspaceBackendError> {
    let value = value.ok_or(WorkspaceBackendError::MissingCommit {
        assignment_id: workspace.assignment_id,
        field,
    })?;
    Oid::from_str(value).map_err(|source| {
        WorkspaceBackendError::InvalidCommit {
            assignment_id: workspace.assignment_id,
            value: value.to_owned(),
            source,
        }
    })
}

fn head_oid(
    repository: &Repository,
    action: &'static str,
    path: &Path,
) -> Result<Oid, WorkspaceBackendError> {
    repository
        .head()
        .and_then(|head| head.peel_to_commit())
        .map(|commit| commit.id())
        .map_err(|source| WorkspaceBackendError::Git {
            action,
            path: path.to_owned(),
            source,
        })
}

fn repository_is_clean(
    repository: &Repository,
) -> Result<bool, WorkspaceBackendError> {
    if repository.state() != RepositoryState::Clean {
        return Ok(false);
    }
    let mut options = StatusOptions::new();
    options.include_untracked(true).recurse_untracked_dirs(true);
    let statuses =
        repository.statuses(Some(&mut options)).map_err(|source| {
            WorkspaceBackendError::Git {
                action: "inspect repository status",
                path: repository.path().to_owned(),
                source,
            }
        })?;
    Ok(statuses.is_empty())
}

fn require_clean_repository(
    repository: &Repository,
    _path: &Path,
    dirty: WorkspaceBackendError,
) -> Result<(), WorkspaceBackendError> {
    if repository_is_clean(repository)? {
        Ok(())
    } else {
        Err(dirty)
    }
}

fn require_linear_result_history(
    repository: &Repository,
    workspace: &WorkspaceRecord,
    base: Oid,
    result: Oid,
) -> Result<(), WorkspaceBackendError> {
    let mut current = result;
    while current != base {
        let commit = repository.find_commit(current).map_err(|source| {
            WorkspaceBackendError::Git {
                action: "walk the assignment result history",
                path: workspace.path.clone(),
                source,
            }
        })?;
        if commit.parent_count() != 1 {
            return Err(WorkspaceBackendError::AmbiguousHistory {
                assignment_id: workspace.assignment_id,
            });
        }
        current = commit.parent_id(0).map_err(|source| {
            WorkspaceBackendError::Git {
                action: "walk the assignment result history",
                path: workspace.path.clone(),
                source,
            }
        })?;
    }
    Ok(())
}

fn validate_integration_history(
    repository: &Repository,
    workspace: &WorkspaceRecord,
    base: Oid,
    result: Oid,
    target: Oid,
) -> Result<(), WorkspaceBackendError> {
    repository.find_commit(base).map_err(|source| {
        WorkspaceBackendError::Git {
            action: "resolve the recorded assignment base",
            path: repository.path().to_owned(),
            source,
        }
    })?;
    repository.find_commit(result).map_err(|source| {
        WorkspaceBackendError::Git {
            action: "resolve the recorded assignment result",
            path: repository.path().to_owned(),
            source,
        }
    })?;
    repository.find_commit(target).map_err(|source| {
        WorkspaceBackendError::Git {
            action: "resolve the integration target commit",
            path: repository.path().to_owned(),
            source,
        }
    })?;
    if target == result
        || repository
            .graph_descendant_of(target, result)
            .map_err(|source| WorkspaceBackendError::Git {
                action: "check whether the assignment result is already integrated",
                path: repository.path().to_owned(),
                source,
            })?
    {
        return Ok(());
    }
    let bases = repository.merge_bases(target, result).map_err(|source| {
        WorkspaceBackendError::Git {
            action: "resolve the integration merge bases",
            path: repository.path().to_owned(),
            source,
        }
    })?;
    if bases.len() != 1 || bases.first() != Some(&base) {
        return Err(WorkspaceBackendError::AmbiguousHistory {
            assignment_id: workspace.assignment_id,
        });
    }
    Ok(())
}

fn preflight_integration_tree(
    repository: &Repository,
    workspace: &WorkspaceRecord,
    base: Oid,
    result: Oid,
    target: Oid,
) -> Result<(), WorkspaceBackendError> {
    if target == result
        || target == base
        || repository
            .graph_descendant_of(target, result)
            .map_err(|source| WorkspaceBackendError::Git {
                action: "check whether the assignment result is already integrated",
                path: repository.path().to_owned(),
                source,
            })?
    {
        return Ok(());
    }
    let base_tree = repository
        .find_commit(base)
        .and_then(|commit| commit.tree())
        .map_err(|source| WorkspaceBackendError::Git {
            action: "resolve the assignment base tree",
            path: repository.path().to_owned(),
            source,
        })?;
    let target_tree = repository
        .find_commit(target)
        .and_then(|commit| commit.tree())
        .map_err(|source| WorkspaceBackendError::Git {
            action: "resolve the integration target tree",
            path: repository.path().to_owned(),
            source,
        })?;
    let result_tree = repository
        .find_commit(result)
        .and_then(|commit| commit.tree())
        .map_err(|source| WorkspaceBackendError::Git {
            action: "resolve the assignment result tree",
            path: repository.path().to_owned(),
            source,
        })?;
    let index = repository
        .merge_trees(&base_tree, &target_tree, &result_tree, None)
        .map_err(|source| WorkspaceBackendError::Git {
            action: "preflight the integration merge",
            path: repository.path().to_owned(),
            source,
        })?;
    if index.has_conflicts() {
        return Err(WorkspaceBackendError::IntegrationConflict {
            assignment_id: workspace.assignment_id,
        });
    }
    Ok(())
}

fn integration_candidate(
    repository: &Repository,
    workspace: &WorkspaceRecord,
    base: Oid,
    result: Oid,
    target: Oid,
    integrated_at: i64,
) -> Result<IntegrationCandidate, WorkspaceBackendError> {
    if target == result
        || repository
            .graph_descendant_of(target, result)
            .map_err(|source| WorkspaceBackendError::Git {
                action: "check whether the assignment result is already integrated",
                path: repository.path().to_owned(),
                source,
            })?
    {
        let tree_id = repository
            .find_commit(target)
            .and_then(|commit| commit.tree().map(|tree| tree.id()))
            .map_err(|source| WorkspaceBackendError::Git {
                action: "resolve the integrated target tree",
                path: repository.path().to_owned(),
                source,
            })?;
        return Ok(IntegrationCandidate {
            tree_id,
            target_commit: target,
            commit_buffer: None,
        });
    }
    if target == base {
        let tree_id = repository
            .find_commit(result)
            .and_then(|commit| commit.tree().map(|tree| tree.id()))
            .map_err(|source| WorkspaceBackendError::Git {
                action: "resolve the assignment result tree",
                path: repository.path().to_owned(),
                source,
            })?;
        return Ok(IntegrationCandidate {
            tree_id,
            target_commit: result,
            commit_buffer: None,
        });
    }

    let base_tree = repository
        .find_commit(base)
        .and_then(|commit| commit.tree())
        .map_err(|source| WorkspaceBackendError::Git {
            action: "resolve the assignment base tree",
            path: repository.path().to_owned(),
            source,
        })?;
    let target_commit = repository.find_commit(target).map_err(|source| {
        WorkspaceBackendError::Git {
            action: "resolve the integration target commit",
            path: repository.path().to_owned(),
            source,
        }
    })?;
    let target_tree =
        target_commit
            .tree()
            .map_err(|source| WorkspaceBackendError::Git {
                action: "resolve the integration target tree",
                path: repository.path().to_owned(),
                source,
            })?;
    let result_commit = repository.find_commit(result).map_err(|source| {
        WorkspaceBackendError::Git {
            action: "resolve the assignment result commit",
            path: repository.path().to_owned(),
            source,
        }
    })?;
    let result_tree =
        result_commit
            .tree()
            .map_err(|source| WorkspaceBackendError::Git {
                action: "resolve the assignment result tree",
                path: repository.path().to_owned(),
                source,
            })?;
    let mut index = repository
        .merge_trees(&base_tree, &target_tree, &result_tree, None)
        .map_err(|source| WorkspaceBackendError::Git {
            action: "prepare the integration merge",
            path: repository.path().to_owned(),
            source,
        })?;
    if index.has_conflicts() {
        return Err(WorkspaceBackendError::IntegrationConflict {
            assignment_id: workspace.assignment_id,
        });
    }
    let tree_id = index.write_tree_to(repository).map_err(|source| {
        WorkspaceBackendError::Git {
            action: "write the integration tree",
            path: repository.path().to_owned(),
            source,
        }
    })?;
    let tree = repository.find_tree(tree_id).map_err(|source| {
        WorkspaceBackendError::Git {
            action: "resolve the integration tree",
            path: repository.path().to_owned(),
            source,
        }
    })?;
    let signature = Signature::new(
        "Coterie",
        "coterie@localhost",
        &Time::new(integrated_at, 0),
    )
    .map_err(|source| WorkspaceBackendError::Git {
        action: "construct the integration signature",
        path: repository.path().to_owned(),
        source,
    })?;
    let message =
        format!("coterie: integrate assignment {}", workspace.assignment_id);
    let buffer = repository
        .commit_create_buffer(
            &signature,
            &signature,
            &message,
            &tree,
            &[&target_commit, &result_commit],
        )
        .map_err(|source| WorkspaceBackendError::Git {
            action: "construct the integration commit",
            path: repository.path().to_owned(),
            source,
        })?
        .to_vec();
    let target_commit =
        Oid::hash_object(ObjectType::Commit, &buffer).map_err(|source| {
            WorkspaceBackendError::Git {
                action: "identify the integration commit",
                path: repository.path().to_owned(),
                source,
            }
        })?;
    Ok(IntegrationCandidate {
        tree_id,
        target_commit,
        commit_buffer: Some(buffer),
    })
}

fn target_matches_candidate(
    repository: &Repository,
    candidate_tree: Oid,
) -> Result<bool, WorkspaceBackendError> {
    if repository.state() != RepositoryState::Clean {
        return Ok(false);
    }
    let mut index =
        repository
            .index()
            .map_err(|source| WorkspaceBackendError::Git {
                action: "inspect the integration target index",
                path: repository.path().to_owned(),
                source,
            })?;
    let index_tree = index.write_tree_to(repository).map_err(|source| {
        WorkspaceBackendError::Git {
            action: "identify the integration target index",
            path: repository.path().to_owned(),
            source,
        }
    })?;
    if index_tree != candidate_tree {
        return Ok(false);
    }
    let mut options = StatusOptions::new();
    options.include_untracked(true).recurse_untracked_dirs(true);
    let statuses =
        repository.statuses(Some(&mut options)).map_err(|source| {
            WorkspaceBackendError::Git {
                action: "inspect the integration target worktree",
                path: repository.path().to_owned(),
                source,
            }
        })?;
    const WORKTREE_CHANGES: Status = Status::WT_NEW
        .union(Status::WT_MODIFIED)
        .union(Status::WT_DELETED)
        .union(Status::WT_TYPECHANGE)
        .union(Status::WT_RENAMED)
        .union(Status::CONFLICTED);
    Ok(statuses
        .iter()
        .all(|entry| !entry.status().intersects(WORKTREE_CHANGES)))
}

fn workspace_reference(workspace: &WorkspaceRecord) -> String {
    format!(
        "refs/heads/coterie/{}/{}",
        workspace.run_id, workspace.assignment_id
    )
}

fn worktree_name(workspace: &WorkspaceRecord) -> String {
    format!("{}-{}", workspace.run_id, workspace.assignment_id)
}

fn canonicalize_git_path(
    action: &'static str,
    path: &Path,
) -> Result<PathBuf, WorkspaceBackendError> {
    fs::canonicalize(path).map_err(|source| WorkspaceBackendError::Io {
        action,
        path: path.to_owned(),
        source,
    })
}

fn require_real_directory(path: &Path) -> Result<(), WorkspaceBackendError> {
    let metadata = fs::symlink_metadata(path).map_err(|source| {
        WorkspaceBackendError::Io {
            action: "inspect a workspace state directory",
            path: path.to_owned(),
            source,
        }
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(WorkspaceBackendError::UnsafeStateDirectory {
            path: path.to_owned(),
        });
    }
    Ok(())
}

fn repository_matches_project(
    repository: &Repository,
    project: &ProjectRecord,
) -> bool {
    let ProjectIdentity::Git {
        common_directory, ..
    } = &project.identity
    else {
        return false;
    };
    fs::canonicalize(repository.commondir())
        .is_ok_and(|observed| &observed == common_directory)
}

/// A deterministic in-memory workspace boundary for supervised-runtime tests.
#[cfg(test)]
pub(crate) mod fake {
    use std::collections::BTreeMap;

    use super::{
        IntegrationPlan, IntegrationRecord, WorkspaceBackend,
        WorkspaceBackendError,
    };
    use crate::id::AssignmentId;
    use crate::state::{ExternalResourceState, ProjectRecord, WorkspaceRecord};

    #[derive(Default)]
    pub(crate) struct FakeWorkspace {
        workspaces: BTreeMap<AssignmentId, WorkspaceRecord>,
        fail_creations: usize,
        successful_creations: usize,
        base_commit: Option<String>,
        result_commit: Option<String>,
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

        #[must_use]
        pub(crate) fn with_commits(
            base_commit: impl Into<String>,
            result_commit: impl Into<String>,
        ) -> Self {
            Self {
                base_commit: Some(base_commit.into()),
                result_commit: Some(result_commit.into()),
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
        fn base_commit(
            &self,
            _kind: &str,
            _project: &ProjectRecord,
        ) -> Result<Option<String>, WorkspaceBackendError> {
            Ok(self.base_commit.clone())
        }

        fn create(
            &mut self,
            workspace: &WorkspaceRecord,
            _project: &ProjectRecord,
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
            _project: &ProjectRecord,
        ) -> Result<ExternalResourceState, WorkspaceBackendError> {
            Ok(match self.workspaces.get(&workspace.assignment_id) {
                Some(existing) if same_intent(existing, workspace) => {
                    ExternalResourceState::Observed
                }
                Some(_) => ExternalResourceState::Unknown,
                None => ExternalResourceState::Lost,
            })
        }

        fn result_commit(
            &self,
            workspace: &WorkspaceRecord,
            _project: &ProjectRecord,
        ) -> Result<Option<String>, WorkspaceBackendError> {
            Ok(self
                .result_commit
                .clone()
                .or_else(|| workspace.result_commit.clone()))
        }

        fn prepare_integration(
            &self,
            workspace: &WorkspaceRecord,
            project: &ProjectRecord,
            integrated_at: i64,
        ) -> Result<IntegrationPlan, WorkspaceBackendError> {
            let target_commit = workspace.base_commit.clone().ok_or(
                WorkspaceBackendError::MissingCommit {
                    assignment_id: workspace.assignment_id,
                    field: "base",
                },
            )?;
            if workspace.result_commit.is_none() && self.result_commit.is_none()
            {
                return Err(WorkspaceBackendError::MissingCommit {
                    assignment_id: workspace.assignment_id,
                    field: "result",
                });
            }
            Ok(IntegrationPlan {
                assignment_id: workspace.assignment_id,
                project_id: project.id,
                target_reference: "refs/heads/main".to_owned(),
                target_commit,
                integrated_at,
            })
        }

        fn integrate(
            &mut self,
            workspace: &WorkspaceRecord,
            project: &ProjectRecord,
            plan: &IntegrationPlan,
        ) -> Result<IntegrationRecord, WorkspaceBackendError> {
            if plan.assignment_id != workspace.assignment_id
                || plan.project_id != project.id
            {
                return Err(WorkspaceBackendError::IntegrationPlanMismatch {
                    assignment_id: workspace.assignment_id,
                });
            }
            let base_commit = workspace.base_commit.clone().ok_or(
                WorkspaceBackendError::MissingCommit {
                    assignment_id: workspace.assignment_id,
                    field: "base",
                },
            )?;
            let result_commit = self
                .result_commit
                .clone()
                .or_else(|| workspace.result_commit.clone())
                .ok_or(WorkspaceBackendError::MissingCommit {
                    assignment_id: workspace.assignment_id,
                    field: "result",
                })?;
            Ok(IntegrationRecord {
                assignment_id: workspace.assignment_id,
                project_id: project.id,
                target_reference: plan.target_reference.clone(),
                base_commit,
                result_commit: result_commit.clone(),
                target_commit_before: plan.target_commit.clone(),
                target_commit: result_commit,
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

/// A workspace adapter could not perform an external operation safely.
#[derive(Debug, Error)]
pub(crate) enum WorkspaceBackendError {
    #[cfg(test)]
    #[error("the fake workspace creation failed at an injected boundary")]
    InjectedFailure,
    #[error("workspace `{assignment_id}` has conflicting external ownership")]
    OwnershipConflict { assignment_id: AssignmentId },
    #[error("workspace kind `{kind}` is not supported")]
    UnsupportedKind { kind: String },
    #[error(
        "project `{project_id}` is not Git-backed and cannot provide a worktree"
    )]
    WorktreeRequiresGit { project_id: crate::id::ProjectId },
    #[error(
        "Git identity for project `{project_id}` no longer matches durable state"
    )]
    ProjectIdentityChanged { project_id: crate::id::ProjectId },
    #[error(
        "workspace `{assignment_id}` has path {actual:?}, expected {expected:?}"
    )]
    UnexpectedPath {
        assignment_id: AssignmentId,
        expected: PathBuf,
        actual: PathBuf,
    },
    #[error("project workspace directory {path:?} does not exist")]
    MissingProjectDirectory { path: PathBuf },
    #[error("workspace state directory {path:?} is not a real directory")]
    UnsafeStateDirectory { path: PathBuf },
    #[error("workspace `{assignment_id}` has no recorded base commit")]
    MissingBaseCommit { assignment_id: AssignmentId },
    #[error(
        "workspace `{assignment_id}` has no recorded {field} commit for integration"
    )]
    MissingCommit {
        assignment_id: AssignmentId,
        field: &'static str,
    },
    #[error(
        "workspace `{assignment_id}` has invalid commit `{value}`: {source}"
    )]
    InvalidCommit {
        assignment_id: AssignmentId,
        value: String,
        #[source]
        source: git2::Error,
    },
    #[error("workspace `{assignment_id}` kind `{kind}` cannot be integrated")]
    UnsupportedIntegrationKind {
        assignment_id: AssignmentId,
        kind: String,
    },
    #[error("workspace `{assignment_id}` is dirty at {path:?}")]
    DirtyWorkspace {
        assignment_id: AssignmentId,
        path: PathBuf,
    },
    #[error("integration target project `{project_id}` is dirty at {path:?}")]
    DirtyTarget {
        project_id: crate::id::ProjectId,
        path: PathBuf,
    },
    #[error(
        "workspace `{assignment_id}` tip is `{actual}`, expected recorded result `{expected}`"
    )]
    UnexpectedWorkspaceTip {
        assignment_id: AssignmentId,
        expected: String,
        actual: String,
    },
    #[error(
        "integration target project `{project_id}` has no unambiguous branch HEAD"
    )]
    AmbiguousTarget { project_id: crate::id::ProjectId },
    #[error(
        "workspace `{assignment_id}` result and target do not have an unambiguous recorded-base history"
    )]
    AmbiguousHistory { assignment_id: AssignmentId },
    #[error(
        "workspace `{assignment_id}` conflicts with the integration target"
    )]
    IntegrationConflict { assignment_id: AssignmentId },
    #[error(
        "integration target project `{project_id}` tip is `{actual}`, expected `{expected}`"
    )]
    UnexpectedTargetTip {
        project_id: crate::id::ProjectId,
        expected: String,
        actual: String,
    },
    #[error(
        "integration target project `{project_id}` reference is `{actual}`, expected `{expected}`"
    )]
    UnexpectedTargetReference {
        project_id: crate::id::ProjectId,
        expected: String,
        actual: String,
    },
    #[error("integration plan does not match workspace `{assignment_id}`")]
    IntegrationPlanMismatch { assignment_id: AssignmentId },
    #[error(
        "workspace `{assignment_id}` integration plan has invalid commit `{value}`: {source}"
    )]
    InvalidIntegrationPlanCommit {
        assignment_id: AssignmentId,
        value: String,
        #[source]
        source: git2::Error,
    },
    #[error("could not {action} at {path:?}: {source}")]
    Git {
        action: &'static str,
        path: PathBuf,
        #[source]
        source: git2::Error,
    },
    #[error("could not {action} at {path:?}: {source}")]
    Io {
        action: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
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
    #[error("project `{project_id}` for an assignment workspace is missing")]
    MissingProject { project_id: crate::id::ProjectId },
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use git2::{Repository, Signature};
    use serde_json::json;

    use super::fake::FakeWorkspace;
    use super::{
        GitWorkspace, WorkspaceBackend, WorkspaceBackendError,
        WorkspaceSupervisor,
    };
    use crate::id::{
        AgentId, AssignmentId, OperationId, ProjectId, RunId, TaskId,
    };
    use crate::project::{DiscoveredProject, ProjectIdentity};
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
    fn git_backend_creates_an_owned_worktree_from_the_recorded_base() {
        let fixture = GitFixture::new();
        let mut workspace = fixture.workspace();
        let mut backend = GitWorkspace::new(&fixture.state);
        workspace.base_commit = backend
            .base_commit(&workspace.kind, &fixture.project)
            .expect("the base commit should resolve");

        assert_eq!(
            workspace.base_commit.as_deref(),
            Some(fixture.base.as_str())
        );
        assert_eq!(
            backend
                .observe(&workspace, &fixture.project)
                .expect("an absent workspace should be observable"),
            ExternalResourceState::Lost
        );

        backend
            .create(&workspace, &fixture.project)
            .expect("the worktree should be created");
        backend
            .create(&workspace, &fixture.project)
            .expect("creation should be idempotent");

        let repository = Repository::open(&fixture.project.canonical_path)
            .expect("the source repository should reopen");
        let reference = repository
            .find_reference(&fixture.reference_name())
            .expect("the assignment reference should exist");
        assert_eq!(
            reference.target().map(|oid| oid.to_string()),
            Some(fixture.base.clone())
        );
        let worktree = repository
            .find_worktree(&fixture.worktree_name())
            .expect("the assignment worktree should be registered");
        assert_eq!(
            fs::canonicalize(worktree.path())
                .expect("worktree path should resolve"),
            fs::canonicalize(&workspace.path)
                .expect("workspace path should resolve")
        );
        assert_eq!(
            backend
                .observe(&workspace, &fixture.project)
                .expect("the created workspace should be observable"),
            ExternalResourceState::Observed
        );
    }

    #[test]
    fn git_backend_reports_the_resulting_worktree_commit() {
        let fixture = GitFixture::new();
        let mut workspace = fixture.workspace();
        let mut backend = GitWorkspace::new(&fixture.state);
        workspace.base_commit = backend
            .base_commit(&workspace.kind, &fixture.project)
            .expect("the base commit should resolve");
        backend
            .create(&workspace, &fixture.project)
            .expect("the worktree should be created");

        let repository = Repository::open(&workspace.path)
            .expect("the assignment worktree should open");
        fs::write(workspace.path.join("result.txt"), "result\n")
            .expect("the result fixture should be written");
        let mut index = repository.index().expect("the index should open");
        index
            .add_path(Path::new("result.txt"))
            .expect("the result should enter the index");
        index.write().expect("the index should be persisted");
        let tree_id = index.write_tree().expect("the tree should be written");
        let tree = repository
            .find_tree(tree_id)
            .expect("the tree should resolve");
        let parent = repository
            .head()
            .and_then(|head| head.peel_to_commit())
            .expect("the base commit should resolve");
        let signature = Signature::now("Coterie Test", "test@example.invalid")
            .expect("the signature should be valid");
        let result = repository
            .commit(
                Some("HEAD"),
                &signature,
                &signature,
                "result",
                &tree,
                &[&parent],
            )
            .expect("the result commit should be created");
        let result = result.to_string();

        assert_eq!(
            backend
                .result_commit(&workspace, &fixture.project)
                .expect("the result commit should resolve")
                .as_deref(),
            Some(result.as_str())
        );
        workspace.state = ExternalResourceState::Observed;
        let mut store = store_with_workspace_records(
            fixture.project.clone(),
            workspace.clone(),
        );
        let supervisor = WorkspaceSupervisor::new(backend);
        assert_eq!(
            supervisor
                .record_result_commit(&mut store, workspace.assignment_id)
                .expect("the result observation should be recorded")
                .as_deref(),
            Some(result.as_str())
        );
        supervisor
            .record_result_commit(&mut store, workspace.assignment_id)
            .expect("recording the same result should be idempotent");
        assert_eq!(
            stored_workspace(&mut store, workspace.assignment_id)
                .result_commit
                .as_deref(),
            Some(result.as_str())
        );
        assert!(
            store
                .transaction(|repositories| {
                    repositories.record_workspace_result_commit(
                        workspace.assignment_id,
                        &fixture.base,
                    )?;
                    Ok(())
                })
                .is_err(),
            "a different later observation must not replace the durable result"
        );
    }

    #[test]
    fn guarded_integration_fast_forwards_a_clean_target() {
        let fixture = GitFixture::new();
        let (mut backend, mut workspace) = fixture.materialized_workspace();
        let result = commit_file(
            &workspace.path,
            "result.txt",
            "result\n",
            "worker result",
        );
        workspace.result_commit = Some(result.clone());

        let plan = backend
            .prepare_integration(&workspace, &fixture.project, 11)
            .expect("the clean linear integration should preflight");
        let integrated = backend
            .integrate(&workspace, &fixture.project, &plan)
            .expect("the clean linear integration should apply");
        assert_eq!(
            backend
                .integrate(&workspace, &fixture.project, &plan)
                .expect("the same durable integration plan should replay"),
            integrated
        );

        assert_eq!(integrated.base_commit, fixture.base);
        assert_eq!(integrated.result_commit, result);
        assert_eq!(integrated.target_commit_before, fixture.base);
        assert_eq!(integrated.target_commit, integrated.result_commit);
        assert_eq!(head_commit(&fixture.project.canonical_path), result);
        assert_eq!(
            backend
                .observe(&workspace, &fixture.project)
                .expect("integration must preserve the assignment worktree"),
            ExternalResourceState::Observed
        );
        assert_eq!(
            fs::read_to_string(
                fixture.project.canonical_path.join("result.txt")
            )
            .expect("the integrated file should be checked out"),
            "result\n"
        );
    }

    #[test]
    fn guarded_integration_merges_nonconflicting_target_changes() {
        let fixture = GitFixture::new();
        let (mut backend, mut workspace) = fixture.materialized_workspace();
        let result = commit_file(
            &workspace.path,
            "result.txt",
            "result\n",
            "worker result",
        );
        workspace.result_commit = Some(result.clone());
        let target_before = commit_file(
            &fixture.project.canonical_path,
            "target.txt",
            "target\n",
            "target advanced",
        );

        let plan = backend
            .prepare_integration(&workspace, &fixture.project, 11)
            .expect("nonconflicting histories should preflight");
        let integrated = backend
            .integrate(&workspace, &fixture.project, &plan)
            .expect("nonconflicting histories should merge");

        assert_eq!(integrated.target_commit_before, target_before);
        assert_ne!(integrated.target_commit, target_before);
        assert_ne!(integrated.target_commit, result);
        let repository = Repository::open(&fixture.project.canonical_path)
            .expect("the target repository should open");
        let commit = repository
            .head()
            .and_then(|head| head.peel_to_commit())
            .expect("the integration commit should resolve");
        assert_eq!(commit.parent_count(), 2);
        assert_eq!(
            commit.parent_id(0).expect("a first parent").to_string(),
            target_before
        );
        assert_eq!(
            commit.parent_id(1).expect("a second parent").to_string(),
            result
        );
        assert_eq!(
            fs::read_to_string(
                fixture.project.canonical_path.join("result.txt")
            )
            .expect("the worker result should be checked out"),
            "result\n"
        );
        assert_eq!(
            fs::read_to_string(
                fixture.project.canonical_path.join("target.txt")
            )
            .expect("the target change should remain checked out"),
            "target\n"
        );
    }

    #[test]
    fn guarded_integration_refuses_a_dirty_target_without_changing_it() {
        let fixture = GitFixture::new();
        let (backend, mut workspace) = fixture.materialized_workspace();
        let result = commit_file(
            &workspace.path,
            "result.txt",
            "result\n",
            "worker result",
        );
        workspace.result_commit = Some(result);
        fs::write(
            fixture.project.canonical_path.join("README.md"),
            "operator edit\n",
        )
        .expect("the target should become dirty");

        assert!(matches!(
            backend.prepare_integration(&workspace, &fixture.project, 11),
            Err(WorkspaceBackendError::DirtyTarget { .. })
        ));
        assert_eq!(head_commit(&fixture.project.canonical_path), fixture.base);
        assert_eq!(
            fs::read_to_string(
                fixture.project.canonical_path.join("README.md")
            )
            .expect("the operator edit should remain"),
            "operator edit\n"
        );
    }

    #[test]
    fn guarded_integration_refuses_an_unexpected_target_tip() {
        let fixture = GitFixture::new();
        let (mut backend, mut workspace) = fixture.materialized_workspace();
        let result = commit_file(
            &workspace.path,
            "result.txt",
            "result\n",
            "worker result",
        );
        workspace.result_commit = Some(result);
        let plan = backend
            .prepare_integration(&workspace, &fixture.project, 11)
            .expect("the initial target should preflight");
        let new_target = commit_file(
            &fixture.project.canonical_path,
            "target.txt",
            "target\n",
            "target advanced",
        );

        assert!(matches!(
            backend.integrate(&workspace, &fixture.project, &plan),
            Err(WorkspaceBackendError::UnexpectedTargetTip {
                expected,
                actual,
                ..
            }) if expected == fixture.base && actual == new_target
        ));
        assert_eq!(head_commit(&fixture.project.canonical_path), new_target);
    }

    #[test]
    fn guarded_integration_refuses_an_unexpected_workspace_tip() {
        let fixture = GitFixture::new();
        let (backend, mut workspace) = fixture.materialized_workspace();
        let recorded_result = commit_file(
            &workspace.path,
            "result.txt",
            "result\n",
            "worker result",
        );
        workspace.result_commit = Some(recorded_result.clone());
        let unexpected_result = commit_file(
            &workspace.path,
            "later.txt",
            "later\n",
            "unreported worker result",
        );

        assert!(matches!(
            backend.prepare_integration(&workspace, &fixture.project, 11),
            Err(WorkspaceBackendError::UnexpectedWorkspaceTip {
                expected,
                actual,
                ..
            }) if expected == recorded_result && actual == unexpected_result
        ));
        assert_eq!(head_commit(&fixture.project.canonical_path), fixture.base);
    }

    #[test]
    fn guarded_integration_refuses_ambiguous_worker_history() {
        let fixture = GitFixture::new();
        let (backend, mut workspace) = fixture.materialized_workspace();
        let first_parent = commit_file(
            &workspace.path,
            "result.txt",
            "result\n",
            "worker result",
        );
        let repository = Repository::open(&workspace.path)
            .expect("the assignment worktree should open");
        let first_parent = repository
            .find_commit(first_parent.parse().expect("a valid commit ID"))
            .expect("the worker result should resolve");
        let base = repository
            .find_commit(fixture.base.parse().expect("a valid base commit ID"))
            .expect("the base should resolve");
        let tree = base.tree().expect("the base tree should resolve");
        let signature = Signature::now("Coterie Test", "test@example.invalid")
            .expect("the signature should be valid");
        let side = repository
            .commit(None, &signature, &signature, "side", &tree, &[&base])
            .expect("the side commit should be created");
        let side = repository
            .find_commit(side)
            .expect("the side commit should resolve");
        let tree = first_parent
            .tree()
            .expect("the worker result tree should resolve");
        let merge = repository
            .commit(
                Some("HEAD"),
                &signature,
                &signature,
                "ambiguous merge",
                &tree,
                &[&first_parent, &side],
            )
            .expect("the merge commit should be created")
            .to_string();
        workspace.result_commit = Some(merge);

        assert!(matches!(
            backend.prepare_integration(&workspace, &fixture.project, 11),
            Err(WorkspaceBackendError::AmbiguousHistory { .. })
        ));
        assert_eq!(head_commit(&fixture.project.canonical_path), fixture.base);
    }

    #[test]
    fn guarded_integration_refuses_conflicts_without_changing_the_target() {
        let fixture = GitFixture::new();
        let (backend, mut workspace) = fixture.materialized_workspace();
        let result = commit_file(
            &workspace.path,
            "README.md",
            "worker edit\n",
            "worker result",
        );
        workspace.result_commit = Some(result);
        let target = commit_file(
            &fixture.project.canonical_path,
            "README.md",
            "operator edit\n",
            "target advanced",
        );

        assert!(matches!(
            backend.prepare_integration(&workspace, &fixture.project, 11),
            Err(WorkspaceBackendError::IntegrationConflict { .. })
        ));
        assert_eq!(head_commit(&fixture.project.canonical_path), target);
        assert_eq!(
            fs::read_to_string(
                fixture.project.canonical_path.join("README.md")
            )
            .expect("the target file should remain readable"),
            "operator edit\n"
        );
    }

    #[test]
    fn git_backend_rejects_worktrees_for_non_git_projects() {
        let fixture = GitFixture::new();
        let mut workspace = fixture.workspace();
        let project = ProjectRecord {
            canonical_path: fixture.root.join("plain-project"),
            identity: ProjectIdentity::Directory {
                canonical_directory: fixture.root.join("plain-project"),
            },
            ..fixture.project.clone()
        };
        fs::create_dir(&project.canonical_path)
            .expect("the plain project should be created");
        let backend = GitWorkspace::new(&fixture.state);

        assert!(
            backend.base_commit(&workspace.kind, &project).is_err(),
            "worktree isolation must not weaken to a non-Git directory"
        );
        workspace.base_commit = None;
        assert!(backend.observe(&workspace, &project).is_err());
    }

    #[test]
    fn git_backend_does_not_create_through_a_state_directory_symlink() {
        let fixture = GitFixture::new();
        let mut workspace = fixture.workspace();
        let outside = fixture.root.join("outside");
        fs::create_dir(&outside).expect("the redirect target should exist");
        std::os::unix::fs::symlink(&outside, fixture.state.join("workspaces"))
            .expect("the state symlink should be created");
        let mut backend = GitWorkspace::new(&fixture.state);
        workspace.base_commit = backend
            .base_commit(&workspace.kind, &fixture.project)
            .expect("the base commit should resolve");

        assert!(backend.create(&workspace, &fixture.project).is_err());
        assert!(
            fs::read_dir(&outside)
                .expect("the redirect target should be readable")
                .next()
                .is_none(),
            "workspace creation must not write through a state symlink"
        );
    }

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
    fn integration_observation_records_the_target_and_event_once() {
        let fixture = GitFixture::new();
        let mut workspace = fixture.workspace();
        workspace.state = ExternalResourceState::Observed;
        workspace.base_commit =
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned());
        workspace.result_commit =
            Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_owned());
        let assignment_id = workspace.assignment_id;
        let mut store =
            store_with_workspace_records(fixture.project.clone(), workspace);
        let mut supervisor =
            WorkspaceSupervisor::new(FakeWorkspace::with_commits(
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            ));
        let operation_id = OPERATION_ID
            .parse::<OperationId>()
            .expect("valid operation ID");
        let plan = supervisor
            .prepare_integration(&mut store, assignment_id, 11)
            .expect("the fake integration should preflight");

        let integrated = supervisor
            .integrate(
                &mut store,
                assignment_id,
                &plan,
                operation_id,
                "operator",
            )
            .expect("the fake integration should be recorded");
        assert_eq!(
            supervisor
                .integrate(
                    &mut store,
                    assignment_id,
                    &plan,
                    operation_id,
                    "operator",
                )
                .expect("the integration observation should be idempotent"),
            integrated
        );

        let (stored, events) = store
            .transaction(|repositories| {
                Ok((
                    repositories
                        .workspace(assignment_id)?
                        .expect("the workspace should remain durable"),
                    repositories.events_after(
                        fixture.project.run_id,
                        0,
                        100,
                    )?,
                ))
            })
            .expect("the integration state should be readable");
        assert_eq!(
            stored.target_commit.as_deref(),
            Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| event.event_type == "workspace.integrated")
                .count(),
            1
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
        let assignment_id = ASSIGNMENT_ID
            .parse::<AssignmentId>()
            .expect("valid assignment ID");
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
        let project = ProjectRecord {
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
        };
        let store = store_with_workspace_records(project, workspace.clone());
        (store, workspace)
    }

    fn store_with_workspace_records(
        project: ProjectRecord,
        workspace: WorkspaceRecord,
    ) -> Store {
        let run_id = workspace.run_id;
        let project_id = workspace.project_id;
        let agent_id = AGENT_ID.parse::<AgentId>().expect("valid agent ID");
        let task_id = TASK_ID.parse::<TaskId>().expect("valid task ID");
        let assignment_id = workspace.assignment_id;
        let operation_id = OPERATION_ID
            .parse::<OperationId>()
            .expect("valid operation ID");
        let mut store = Store::open_in_memory().expect("the store should open");
        store
            .transaction(|repositories| {
                repositories.insert_run(&RunRecord {
                    id: run_id,
                    status: "active".to_owned(),
                    created_at: 1,
                    stopped_at: None,
                })?;
                repositories.insert_project(&project)?;
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
        store
    }

    struct GitFixture {
        root: PathBuf,
        state: PathBuf,
        project: ProjectRecord,
        base: String,
    }

    impl GitFixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "coterie-workspace-{}-{}",
                std::process::id(),
                ulid::Ulid::generate()
            ));
            let project_path = root.join("project");
            let state = root.join("state");
            fs::create_dir_all(&state).expect("the state root should exist");
            let repository = Repository::init(&project_path)
                .expect("the repository should initialize");
            fs::write(project_path.join("README.md"), "fixture\n")
                .expect("the fixture should be written");
            let mut index = repository.index().expect("the index should open");
            index
                .add_path(Path::new("README.md"))
                .expect("the fixture should enter the index");
            index.write().expect("the index should be persisted");
            let tree_id =
                index.write_tree().expect("the tree should be written");
            let tree = repository
                .find_tree(tree_id)
                .expect("the tree should resolve");
            let signature =
                Signature::now("Coterie Test", "test@example.invalid")
                    .expect("the signature should be valid");
            let base = repository
                .commit(
                    Some("HEAD"),
                    &signature,
                    &signature,
                    "initial",
                    &tree,
                    &[],
                )
                .expect("the initial commit should be created")
                .to_string();
            drop(tree);
            drop(index);
            drop(repository);
            let discovered = DiscoveredProject::discover(&project_path)
                .expect("the project should be discovered");
            let run_id = RUN_ID.parse::<RunId>().expect("valid run ID");
            let project_id =
                PROJECT_ID.parse::<ProjectId>().expect("valid project ID");
            let project = ProjectRecord {
                id: project_id,
                run_id,
                alias: "primary".to_owned(),
                original_path: discovered.original_path,
                canonical_path: discovered.canonical_path,
                identity: discovered.identity,
                is_primary: true,
                attached_at: 1,
            };
            Self {
                root,
                state,
                project,
                base,
            }
        }

        fn workspace(&self) -> WorkspaceRecord {
            let assignment_id = ASSIGNMENT_ID
                .parse::<AssignmentId>()
                .expect("valid assignment ID");
            WorkspaceRecord {
                assignment_id,
                run_id: self.project.run_id,
                project_id: self.project.id,
                kind: "worktree".to_owned(),
                path: self
                    .state
                    .join("workspaces")
                    .join(self.project.id.to_string())
                    .join(assignment_id.to_string()),
                state: ExternalResourceState::Desired,
                base_commit: None,
                result_commit: None,
                target_commit: None,
                created_at: 1,
                reconciled_at: None,
            }
        }

        fn materialized_workspace(&self) -> (GitWorkspace, WorkspaceRecord) {
            let mut workspace = self.workspace();
            let mut backend = GitWorkspace::new(&self.state);
            workspace.base_commit = backend
                .base_commit(&workspace.kind, &self.project)
                .expect("the base commit should resolve");
            backend
                .create(&workspace, &self.project)
                .expect("the assignment worktree should be created");
            workspace.state = ExternalResourceState::Observed;
            (backend, workspace)
        }

        fn reference_name(&self) -> String {
            format!(
                "refs/heads/coterie/{}/{}",
                self.project.run_id, ASSIGNMENT_ID
            )
        }

        fn worktree_name(&self) -> String {
            format!("{}-{}", self.project.run_id, ASSIGNMENT_ID)
        }
    }

    fn commit_file(
        repository_path: &Path,
        path: &str,
        contents: &str,
        message: &str,
    ) -> String {
        let repository = Repository::open(repository_path)
            .expect("the repository should open");
        fs::write(repository_path.join(path), contents)
            .expect("the commit fixture should be written");
        let mut index = repository.index().expect("the index should open");
        index
            .add_path(Path::new(path))
            .expect("the fixture should enter the index");
        index.write().expect("the index should be persisted");
        let tree_id = index.write_tree().expect("the tree should be written");
        let tree = repository
            .find_tree(tree_id)
            .expect("the tree should resolve");
        let parent = repository
            .head()
            .and_then(|head| head.peel_to_commit())
            .expect("the parent commit should resolve");
        let signature = Signature::now("Coterie Test", "test@example.invalid")
            .expect("the signature should be valid");
        repository
            .commit(
                Some("HEAD"),
                &signature,
                &signature,
                message,
                &tree,
                &[&parent],
            )
            .expect("the fixture commit should be created")
            .to_string()
    }

    fn head_commit(repository_path: &Path) -> String {
        let repository = Repository::open(repository_path)
            .expect("the repository should open");
        let commit = repository
            .head()
            .and_then(|head| head.peel_to_commit())
            .expect("the HEAD commit should resolve")
            .id();
        commit.to_string()
    }

    impl Drop for GitFixture {
        fn drop(&mut self) {
            if self.root.exists() {
                fs::remove_dir_all(&self.root)
                    .expect("the Git fixture should be removable");
            }
        }
    }
}

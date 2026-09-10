//! SQLite migrations, transactions, operations, messages, and events.

mod configuration;
mod diagnostics;
pub(crate) mod supervision;

#[cfg(test)]
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::io;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::types::Type;
use rusqlite::{
    Connection, OptionalExtension, Transaction, TransactionBehavior, params,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value as JsonValue, json};
use thiserror::Error;

use crate::auth::TokenVerifier;
use crate::id::{
    AgentId, AssignmentId, EventId, MessageId, OperationId, ProjectId, RunId,
    SessionId, TaskId,
};
use crate::project::ProjectIdentity;
use crate::providers::LifecycleState;
use crate::tasks::{TaskReadiness, TaskStatus, TaskTransition};

const BUSY_TIMEOUT: Duration = Duration::from_secs(2);
// Leave room for the RPC envelope and page cursor beneath the transport limit.
const MAXIMUM_EVENT_PAGE_LENGTH: usize = 900 * 1024;

const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "initial",
        sql: include_str!("state/migrations/0001_initial.sql"),
    },
    Migration {
        version: 2,
        name: "claim_invariants",
        sql: include_str!("state/migrations/0002_claim_invariants.sql"),
    },
    Migration {
        version: 3,
        name: "session_credentials",
        sql: include_str!("state/migrations/0003_session_credentials.sql"),
    },
    Migration {
        version: 4,
        name: "durable_messages",
        sql: include_str!("state/migrations/0004_durable_messages.sql"),
    },
    Migration {
        version: 5,
        name: "external_resource_reconciliation",
        sql: include_str!(
            "state/migrations/0005_external_resource_reconciliation.sql"
        ),
    },
    Migration {
        version: 6,
        name: "session_process_ownership",
        sql: include_str!(
            "state/migrations/0006_session_process_ownership.sql"
        ),
    },
    Migration {
        version: 7,
        name: "operation_reconciliation",
        sql: include_str!("state/migrations/0007_operation_reconciliation.sql"),
    },
    Migration {
        version: 8,
        name: "generation_fencing",
        sql: include_str!("state/migrations/0008_generation_fencing.sql"),
    },
    Migration {
        version: 9,
        name: "bounded_supervision",
        sql: include_str!("state/migrations/0009_bounded_supervision.sql"),
    },
    Migration {
        version: 10,
        name: "request_fingerprints",
        sql: include_str!("state/migrations/0010_request_fingerprints.sql"),
    },
    Migration {
        version: 11,
        name: "configuration_snapshots",
        sql: include_str!("state/migrations/0011_configuration_snapshots.sql"),
    },
    Migration {
        version: 12,
        name: "project_root_policy",
        sql: include_str!("state/migrations/0012_project_root_policy.sql"),
    },
];

#[derive(Debug)]
struct Migration {
    version: i64,
    name: &'static str,
    sql: &'static str,
}

/// A failure to open, migrate, or access durable run state.
#[derive(Debug, Error)]
pub(crate) enum StoreError {
    #[error(
        "run {run_id} has a missing or invalid configuration snapshot; preserve the run state and inspect it with `coterie doctor`"
    )]
    InvalidConfigurationSnapshot { run_id: RunId },
    #[error("event `{id}` exceeds the {maximum}-byte event limit; shorten the request text", maximum = MAXIMUM_EVENT_PAGE_LENGTH)]
    EventTooLarge { id: EventId },
    /// SQLite rejected an operation.
    #[error("SQLite state error: {0}")]
    Database(#[from] rusqlite::Error),

    /// A structured value could not be encoded for storage.
    #[error("could not encode structured state: {0}")]
    EncodeJson(#[from] serde_json::Error),

    /// A previously applied migration no longer matches its embedded source.
    #[error("applied migration {version} (`{name}`) has been modified")]
    ModifiedMigration { version: i64, name: String },

    /// The database was written by a newer Coterie schema.
    #[error(
        "database schema version {found} is newer than supported version {supported}"
    )]
    UnsupportedSchema { found: i64, supported: i64 },

    /// An operation ID was previously bound to a different mutation.
    #[error("operation `{id}` is already bound to a different request")]
    OperationConflict { id: OperationId },

    /// An atomic mutation encountered an operation it cannot safely resume.
    #[error("operation `{id}` has nonterminal status `{status}`")]
    OperationIncomplete { id: OperationId, status: String },

    /// A completed operation did not contain the result required for replay.
    #[error("completed operation `{id}` has no durable result")]
    MissingOperationResult { id: OperationId },

    /// An agent record violates a lifecycle invariant.
    #[error("agent `{id}` has corrupt lifecycle state: {reason}")]
    CorruptAgentState { id: AgentId, reason: String },

    /// Related task, claim, and assignment records violate a lifecycle invariant.
    #[error("task `{id}` has corrupt lifecycle state: {reason}")]
    CorruptTaskState { id: TaskId, reason: String },

    /// An assignment cannot be associated with the requested live session.
    #[error("assignment `{id}` has corrupt lifecycle state: {reason}")]
    CorruptAssignmentState { id: AssignmentId, reason: String },

    /// A resource belongs to another run or a retired generation.
    #[error(
        "assignment `{id}` does not belong to the current run and generation"
    )]
    StaleAssignment { id: AssignmentId },

    /// A completed workspace observation conflicts with its durable result.
    #[error(
        "workspace `{assignment_id}` already records result commit `{recorded}`, not `{observed}`"
    )]
    WorkspaceResultConflict {
        assignment_id: AssignmentId,
        recorded: String,
        observed: String,
    },

    /// A completed integration observation conflicts with durable state.
    #[error(
        "workspace `{assignment_id}` already records target commit `{recorded}`, not `{observed}`"
    )]
    WorkspaceTargetConflict {
        assignment_id: AssignmentId,
        recorded: String,
        observed: String,
    },

    /// Orderly shutdown did not find the expected active run.
    #[error("run `{id}` is not active during orderly shutdown")]
    RunNotActive { id: RunId },

    /// A credential already marked inactive cannot be activated again.
    #[error("session `{session_id}` credential is already revoked")]
    CredentialAlreadyRevoked { session_id: SessionId },

    /// A terminal session or inconsistent owning agent cannot move to the observation.
    #[error(
        "session `{session_id}` cannot transition from `{current}` to `{observed}`"
    )]
    InvalidSessionTransition {
        session_id: SessionId,
        current: LifecycleState,
        observed: LifecycleState,
    },

    /// The owning agent and session disagree within one active generation.
    #[error(
        "session `{session_id}` and agent `{agent_id}` have inconsistent lifecycle state"
    )]
    InconsistentSessionLifecycle {
        session_id: SessionId,
        agent_id: AgentId,
    },
}

/// The supervisor-owned connection to one run database.
pub(crate) struct Store {
    connection: Connection,
}

/// A durable run record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RunRecord {
    pub(crate) id: RunId,
    pub(crate) status: String,
    pub(crate) created_at: i64,
    pub(crate) stopped_at: Option<i64>,
}

/// An immutable effective-configuration snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ConfigurationSnapshotRecord {
    pub(crate) id: i64,
    pub(crate) run_id: RunId,
    pub(crate) project_id: Option<ProjectId>,
    pub(crate) scope: String,
    pub(crate) schema_version: i64,
    pub(crate) fingerprint: String,
    pub(crate) document: JsonValue,
    pub(crate) created_at: i64,
}

/// A project attached to a run.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct ProjectRecord {
    pub(crate) id: ProjectId,
    pub(crate) run_id: RunId,
    pub(crate) alias: String,
    #[serde(with = "crate::project::path_bytes")]
    pub(crate) original_path: PathBuf,
    #[serde(with = "crate::project::path_bytes")]
    pub(crate) canonical_path: PathBuf,
    pub(crate) identity: ProjectIdentity,
    pub(crate) is_primary: bool,
    pub(crate) attached_at: i64,
}

/// An instantiated configured role.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AgentRecord {
    pub(crate) id: AgentId,
    pub(crate) run_id: RunId,
    pub(crate) role: String,
    pub(crate) generation: i64,
    pub(crate) state: LifecycleState,
    pub(crate) created_at: i64,
}

/// One provider execution associated with an agent.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SessionRecord {
    pub(crate) id: SessionId,
    pub(crate) run_id: RunId,
    pub(crate) agent_id: AgentId,
    pub(crate) generation: i64,
    pub(crate) provider: String,
    pub(crate) provider_session_id: Option<String>,
    pub(crate) reconciliation_state: ExternalResourceState,
    pub(crate) state: LifecycleState,
    pub(crate) transcript_path: PathBuf,
    pub(crate) created_at: i64,
    pub(crate) ended_at: Option<i64>,
    pub(crate) reconciled_at: Option<i64>,
    pub(crate) process_owner: SessionProcessOwner,
}

/// The Coterie process responsible for controlling one provider execution.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SessionProcessOwner {
    Supervisor,
    Foreground,
}

impl SessionProcessOwner {
    #[must_use]
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Supervisor => "supervisor",
            Self::Foreground => "foreground",
        }
    }
}

impl std::str::FromStr for SessionProcessOwner {
    type Err = InvalidSessionProcessOwner;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "supervisor" => Ok(Self::Supervisor),
            "foreground" => Ok(Self::Foreground),
            _ => Err(InvalidSessionProcessOwner(value.to_owned())),
        }
    }
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error("unknown session process owner `{0}`")]
pub(crate) struct InvalidSessionProcessOwner(String);

/// Durable knowledge about an external side effect owned by Coterie.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ExternalResourceState {
    Desired,
    Observed,
    Lost,
    Unknown,
}

impl ExternalResourceState {
    #[must_use]
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Desired => "desired",
            Self::Observed => "observed",
            Self::Lost => "lost",
            Self::Unknown => "unknown",
        }
    }
}

impl std::fmt::Display for ExternalResourceState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl std::str::FromStr for ExternalResourceState {
    type Err = InvalidExternalResourceState;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "desired" => Ok(Self::Desired),
            "observed" => Ok(Self::Observed),
            "lost" => Ok(Self::Lost),
            "unknown" => Ok(Self::Unknown),
            _ => Err(InvalidExternalResourceState(value.to_owned())),
        }
    }
}

/// A reconciliation state read from durable storage is not recognized.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error("unknown external-resource state `{0}`")]
pub(crate) struct InvalidExternalResourceState(String);

/// The durable, non-secret verifier for one provider session credential.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SessionCredentialRecord {
    pub(crate) session_id: SessionId,
    pub(crate) run_id: RunId,
    pub(crate) agent_id: AgentId,
    pub(crate) generation: i64,
    pub(crate) token_verifier: TokenVerifier,
    pub(crate) created_at: i64,
    pub(crate) revoked_at: Option<i64>,
}

/// The result of applying a generation-fenced provider observation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SessionTransitionOutcome {
    Applied,
    Unchanged,
    Stale,
}

/// The result of changing durable reconciliation knowledge for one resource.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ResourceTransitionOutcome {
    Applied,
    Unchanged,
    Stale,
}

/// A lightweight group of related tasks.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TaskGroupRecord {
    pub(crate) id: i64,
    pub(crate) run_id: RunId,
    pub(crate) name: Option<String>,
    pub(crate) created_at: i64,
}

/// A durable unit of work.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TaskRecord {
    pub(crate) id: TaskId,
    pub(crate) run_id: RunId,
    pub(crate) project_id: ProjectId,
    pub(crate) group_id: Option<i64>,
    pub(crate) title: String,
    pub(crate) description: String,
    pub(crate) status: TaskStatus,
    pub(crate) result: Option<JsonValue>,
    pub(crate) created_at: i64,
    pub(crate) updated_at: i64,
}

/// A directed edge from one task to a prerequisite task.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DependencyRecord {
    pub(crate) run_id: RunId,
    pub(crate) task_id: TaskId,
    pub(crate) dependency_task_id: TaskId,
    pub(crate) created_at: i64,
}

/// An append-only task comment.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CommentRecord {
    pub(crate) id: i64,
    pub(crate) run_id: RunId,
    pub(crate) task_id: TaskId,
    pub(crate) author_agent_id: Option<AgentId>,
    pub(crate) body: String,
    pub(crate) created_at: i64,
}

/// A caller-supplied identity and durable state for one mutation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct OperationRecord {
    pub(crate) id: OperationId,
    pub(crate) run_id: RunId,
    pub(crate) kind: String,
    pub(crate) actor_agent_id: Option<AgentId>,
    pub(crate) status: String,
    pub(crate) request: JsonValue,
    pub(crate) result: Option<JsonValue>,
    pub(crate) attempt_count: i64,
    pub(crate) reconciliation_state: Option<ExternalResourceState>,
    pub(crate) reconciliation_attempt_count: i64,
    pub(crate) reconciliation_error: Option<String>,
    pub(crate) reconciled_at: Option<i64>,
    pub(crate) created_at: i64,
    pub(crate) updated_at: i64,
}

/// The identity and request bound to one idempotent database mutation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Mutation {
    pub(crate) id: OperationId,
    pub(crate) run_id: RunId,
    pub(crate) kind: String,
    pub(crate) actor_agent_id: Option<AgentId>,
    pub(crate) request: JsonValue,
    pub(crate) created_at: i64,
}

/// Whether a mutation was applied now or replayed from durable state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum MutationOutcome<T> {
    Applied(T),
    Replayed(T),
}

impl<T: Clone> MutationOutcome<T> {
    #[must_use]
    pub(crate) fn as_replayed(&self) -> Self {
        match self {
            Self::Applied(result) | Self::Replayed(result) => {
                Self::Replayed(result.clone())
            }
        }
    }
}

/// The complete input to an atomic task-claim mutation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ClaimTaskMutation {
    pub(crate) operation_id: OperationId,
    pub(crate) run_id: RunId,
    pub(crate) actor_agent_id: Option<AgentId>,
    pub(crate) task_id: TaskId,
    pub(crate) agent_id: AgentId,
    pub(crate) assignment_id: AssignmentId,
    pub(crate) claimed_at: i64,
}

/// The complete input to an idempotent task-lifecycle mutation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TaskTransitionMutation {
    pub(crate) operation_id: OperationId,
    pub(crate) run_id: RunId,
    pub(crate) actor_agent_id: Option<AgentId>,
    pub(crate) task_id: TaskId,
    pub(crate) transition: TaskTransition,
    pub(crate) result: Option<JsonValue>,
    pub(crate) summary: Option<String>,
    pub(crate) transitioned_at: i64,
}

/// The durable result of trying to change a task's lifecycle state.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "status", content = "reason", rename_all = "snake_case")]
pub(crate) enum TaskTransitionResult {
    Transitioned {
        previous_status: TaskStatus,
        status: TaskStatus,
    },
    Rejected(TaskTransitionRejection),
}

/// Why a task-lifecycle mutation could not be applied.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TaskTransitionRejection {
    TaskNotFound,
    InvalidStatus { status: TaskStatus },
    AcceptanceNotMet,
}

/// The durable result of trying to claim a task.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "status", content = "reason", rename_all = "snake_case")]
pub(crate) enum ClaimTaskResult {
    Claimed {
        claim_id: i64,
        assignment_id: AssignmentId,
    },
    Rejected(ClaimRejection),
}

/// Why a task claim did not satisfy its compare-and-set preconditions.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ClaimRejection {
    TaskNotFound,
    TaskNotOpen,
    Blocked,
    AlreadyClaimed,
    AgentNotFound,
    AgentBusy,
}

/// A durable claim on a task.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ClaimRecord {
    pub(crate) id: i64,
    pub(crate) run_id: RunId,
    pub(crate) task_id: TaskId,
    pub(crate) agent_id: AgentId,
    pub(crate) operation_id: OperationId,
    pub(crate) state: String,
    pub(crate) claimed_at: i64,
    pub(crate) released_at: Option<i64>,
}

/// The durable association between a task, agent, and eventual workspace.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AssignmentRecord {
    pub(crate) id: AssignmentId,
    pub(crate) run_id: RunId,
    pub(crate) task_id: TaskId,
    pub(crate) agent_id: AgentId,
    pub(crate) session_id: Option<SessionId>,
    pub(crate) claim_id: i64,
    pub(crate) generation: i64,
    pub(crate) state: String,
    pub(crate) summary: Option<String>,
    pub(crate) created_at: i64,
    pub(crate) completed_at: Option<i64>,
}

/// A durable inbox message.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct MessageRecord {
    pub(crate) id: MessageId,
    pub(crate) run_id: RunId,
    pub(crate) sender_agent_id: Option<AgentId>,
    pub(crate) recipient_agent_id: AgentId,
    pub(crate) sequence: i64,
    pub(crate) body: String,
    pub(crate) created_at: i64,
    pub(crate) acknowledged_at: Option<i64>,
}

/// The complete input to an idempotent inbox acknowledgement.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AcknowledgeMessagesMutation {
    pub(crate) operation_id: OperationId,
    pub(crate) run_id: RunId,
    pub(crate) agent_id: AgentId,
    pub(crate) through: i64,
    pub(crate) acknowledged_at: i64,
}

/// The durable result of acknowledging a recipient-local message cursor.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(crate) enum AcknowledgeMessagesResult {
    Acknowledged {
        acknowledged_through: i64,
        acknowledged_count: usize,
    },
    CursorNotFound {
        highest_cursor: i64,
    },
}

/// Immutable ownership shared by an assignment and its workspace.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct AssignmentScope {
    pub(crate) run_id: RunId,
    pub(crate) assignment_id: AssignmentId,
    pub(crate) generation: i64,
}

impl AssignmentRecord {
    pub(crate) const fn scope(&self) -> AssignmentScope {
        AssignmentScope {
            run_id: self.run_id,
            assignment_id: self.id,
            generation: self.generation,
        }
    }
}

impl WorkspaceRecord {
    pub(crate) const fn scope(&self) -> AssignmentScope {
        AssignmentScope {
            run_id: self.run_id,
            assignment_id: self.assignment_id,
            generation: self.generation,
        }
    }
}

/// Durable ownership and integration metadata for an assignment workspace.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct WorkspaceRecord {
    pub(crate) assignment_id: AssignmentId,
    pub(crate) generation: i64,
    pub(crate) run_id: RunId,
    pub(crate) project_id: ProjectId,
    pub(crate) kind: String,
    pub(crate) path: PathBuf,
    pub(crate) state: ExternalResourceState,
    pub(crate) base_commit: Option<String>,
    pub(crate) result_commit: Option<String>,
    pub(crate) target_commit: Option<String>,
    pub(crate) created_at: i64,
    pub(crate) reconciled_at: Option<i64>,
}

/// One immutable entry in a run's typed event stream.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct EventRecord {
    pub(crate) id: EventId,
    pub(crate) run_id: RunId,
    pub(crate) sequence: i64,
    pub(crate) event_type: String,
    pub(crate) actor: String,
    pub(crate) subject: String,
    pub(crate) project_id: Option<ProjectId>,
    pub(crate) agent_id: Option<AgentId>,
    pub(crate) task_id: Option<TaskId>,
    pub(crate) operation_id: Option<OperationId>,
    pub(crate) correlation_id: Option<EventId>,
    pub(crate) causation_id: Option<EventId>,
    pub(crate) payload: JsonValue,
    pub(crate) summary: String,
    pub(crate) created_at: i64,
}

/// A stable normalized event type stored in the run event stream.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum EventKind {
    RunStarted,
    RunStopped,
    RunShutdownChanged,
    SessionControlChanged,
    SessionRestartLimited,
    ProjectAttached,
    AgentCreated,
    AgentLifecycleChanged,
    SessionStarted,
    SessionLifecycleChanged,
    SessionReconciliationChanged,
    OperationReconciliationChanged,
    TaskCreated,
    TaskClaimed,
    TaskLifecycleChanged,
    AssignmentCreated,
    AssignmentSessionAssociated,
    AssignmentLifecycleChanged,
    ClaimReleased,
    MessageSent,
    MessageAcknowledged,
    WorkspaceDesired,
    WorkspaceReconciliationChanged,
    WorkspaceIntegrationDesired,
    WorkspaceIntegrated,
}

impl EventKind {
    #[must_use]
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::RunStarted => "run.started",
            Self::RunStopped => "run.stopped",
            Self::RunShutdownChanged => "run.shutdown_changed",
            Self::SessionControlChanged => "session.control_changed",
            Self::SessionRestartLimited => "session.restart_limited",
            Self::ProjectAttached => "project.attached",
            Self::AgentCreated => "agent.created",
            Self::AgentLifecycleChanged => "agent.lifecycle_changed",
            Self::SessionStarted => "session.started",
            Self::SessionLifecycleChanged => "session.lifecycle_changed",
            Self::SessionReconciliationChanged => {
                "session.reconciliation_changed"
            }
            Self::OperationReconciliationChanged => {
                "operation.reconciliation_changed"
            }
            Self::TaskCreated => "task.created",
            Self::TaskClaimed => "task.claimed",
            Self::TaskLifecycleChanged => "task.lifecycle_changed",
            Self::AssignmentCreated => "assignment.created",
            Self::AssignmentSessionAssociated => {
                "assignment.session_associated"
            }
            Self::AssignmentLifecycleChanged => "assignment.lifecycle_changed",
            Self::ClaimReleased => "claim.released",
            Self::MessageSent => "message.sent",
            Self::MessageAcknowledged => "message.acknowledged",
            Self::WorkspaceDesired => "workspace.desired",
            Self::WorkspaceReconciliationChanged => {
                "workspace.reconciliation_changed"
            }
            Self::WorkspaceIntegrationDesired => {
                "workspace.integration_desired"
            }
            Self::WorkspaceIntegrated => "workspace.integrated",
        }
    }
}

/// The context and version-independent data for a new normalized event.
pub(crate) struct NewEvent {
    pub(crate) run_id: RunId,
    pub(crate) kind: EventKind,
    pub(crate) actor: String,
    pub(crate) subject: String,
    pub(crate) project_id: Option<ProjectId>,
    pub(crate) agent_id: Option<AgentId>,
    pub(crate) task_id: Option<TaskId>,
    pub(crate) operation_id: Option<OperationId>,
    pub(crate) correlation_id: Option<EventId>,
    pub(crate) causation_id: Option<EventId>,
    pub(crate) data: JsonValue,
    pub(crate) summary: String,
    pub(crate) created_at: i64,
}

/// Repositories scoped to a single SQLite transaction.
pub(crate) struct Repositories<'transaction, 'connection> {
    transaction: &'transaction Transaction<'connection>,
}

impl Store {
    /// Opens a run database and applies every pending migration in order.
    pub(crate) fn open(path: &std::path::Path) -> Result<Self, StoreError> {
        let connection = Connection::open(path)?;
        Self::from_connection(connection)
    }

    #[cfg(test)]
    pub(crate) fn open_in_memory() -> Result<Self, StoreError> {
        let connection = Connection::open_in_memory()?;
        Self::from_connection(connection)
    }

    fn from_connection(connection: Connection) -> Result<Self, StoreError> {
        connection.busy_timeout(BUSY_TIMEOUT)?;
        connection.pragma_update(None, "foreign_keys", true)?;

        // In-memory databases retain `memory`; filesystems without WAL support
        // retain their current mode. The returned mode is therefore advisory.
        let _journal_mode = connection.pragma_update_and_check(
            None,
            "journal_mode",
            "wal",
            |row| row.get::<_, String>(0),
        )?;

        let mut store = Self { connection };
        // Reserving SQLite's writer slot at transaction start makes mutation
        // ordering explicit and prevents deferred transactions from racing.
        store
            .connection
            .set_transaction_behavior(TransactionBehavior::Immediate);
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&mut self) -> Result<(), StoreError> {
        self.connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_migrations (\
                 version INTEGER PRIMARY KEY,\
                 name TEXT NOT NULL,\
                 source TEXT NOT NULL,\
                 applied_at INTEGER NOT NULL DEFAULT (unixepoch())\
             ) STRICT;\
             CREATE TRIGGER IF NOT EXISTS schema_migrations_cannot_be_updated \
             BEFORE UPDATE ON schema_migrations BEGIN \
                 SELECT RAISE(ABORT, 'schema migrations are append-only');\
             END;\
             CREATE TRIGGER IF NOT EXISTS schema_migrations_cannot_be_deleted \
             BEFORE DELETE ON schema_migrations BEGIN \
                 SELECT RAISE(ABORT, 'schema migrations are append-only');\
             END;",
        )?;

        let applied = {
            let mut statement = self.connection.prepare(
                "SELECT version, name, source FROM schema_migrations ORDER BY version",
            )?;
            let rows = statement.query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?;
            rows.collect::<Result<Vec<_>, _>>()?
        };

        let supported =
            MIGRATIONS.last().map_or(0, |migration| migration.version);
        if let Some((found, _, _)) = applied.last()
            && *found > supported
        {
            return Err(StoreError::UnsupportedSchema {
                found: *found,
                supported,
            });
        }

        for (version, name, source) in &applied {
            let Some(migration) = MIGRATIONS
                .iter()
                .find(|migration| migration.version == *version)
            else {
                return Err(StoreError::UnsupportedSchema {
                    found: *version,
                    supported,
                });
            };
            if migration.name != name || migration.sql != source {
                return Err(StoreError::ModifiedMigration {
                    version: *version,
                    name: name.clone(),
                });
            }
        }

        for migration in &MIGRATIONS[applied.len()..] {
            let transaction = self.connection.transaction()?;
            crate::fault::point("db.migration.before");
            transaction.execute_batch(migration.sql)?;
            transaction.execute(
                "INSERT INTO schema_migrations (version, name, source) VALUES (?1, ?2, ?3)",
                (migration.version, migration.name, migration.sql),
            )?;
            crate::fault::point("db.migration.written");
            transaction.commit()?;
            crate::fault::point("db.migration.committed");
        }

        Ok(())
    }

    /// Commits all repository changes together, or rolls all of them back.
    pub(crate) fn transaction<T>(
        &mut self,
        operation: impl FnOnce(&Repositories<'_, '_>) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        let transaction = self.connection.transaction()?;
        #[cfg(test)]
        let changes = transaction.total_changes();
        let result = operation(&Repositories {
            transaction: &transaction,
        })?;
        #[cfg(test)]
        let changed = transaction.total_changes() != changes;
        #[cfg(test)]
        if changed {
            crate::fault::point("db.transaction.written");
        }
        transaction.commit()?;
        #[cfg(test)]
        if changed {
            crate::fault::point("db.transaction.committed");
        }
        Ok(result)
    }

    /// Applies a database-only mutation once and replays its durable result.
    pub(crate) fn mutate<T>(
        &mut self,
        mutation: &Mutation,
        apply: impl FnOnce(&Repositories<'_, '_>) -> Result<T, StoreError>,
    ) -> Result<MutationOutcome<T>, StoreError>
    where
        T: DeserializeOwned + Serialize,
    {
        self.mutate_with_fingerprint(mutation, None, apply)
    }

    /// Compares original request identities independently of stored redacted text.
    pub(crate) fn mutate_with_fingerprint<T>(
        &mut self,
        mutation: &Mutation,
        request_fingerprint: Option<&str>,
        apply: impl FnOnce(&Repositories<'_, '_>) -> Result<T, StoreError>,
    ) -> Result<MutationOutcome<T>, StoreError>
    where
        T: DeserializeOwned + Serialize,
    {
        let transaction = self.connection.transaction()?;
        let repositories = Repositories {
            transaction: &transaction,
        };

        if let Some(existing) = repositories.operation(mutation.id)? {
            let stored_fingerprint: Option<String> = transaction.query_row(
                "SELECT request_fingerprint FROM operations WHERE id = ?1",
                [mutation.id],
                |row| row.get(0),
            )?;
            let same_request = match stored_fingerprint.as_deref() {
                Some(stored) => request_fingerprint == Some(stored),
                None => existing.request == mutation.request,
            };
            if existing.run_id != mutation.run_id
                || existing.kind != mutation.kind
                || existing.actor_agent_id != mutation.actor_agent_id
                || !same_request
            {
                return Err(StoreError::OperationConflict { id: mutation.id });
            }
            if existing.status != "succeeded" {
                return Err(StoreError::OperationIncomplete {
                    id: mutation.id,
                    status: existing.status,
                });
            }

            let encoded =
                existing.result.ok_or(StoreError::MissingOperationResult {
                    id: mutation.id,
                })?;
            let result = serde_json::from_value(encoded)?;
            transaction.commit()?;
            return Ok(MutationOutcome::Replayed(result));
        }

        repositories.insert_operation(&OperationRecord {
            id: mutation.id,
            run_id: mutation.run_id,
            kind: mutation.kind.clone(),
            actor_agent_id: mutation.actor_agent_id,
            status: "pending".to_owned(),
            request: mutation.request.clone(),
            result: None,
            attempt_count: 1,
            reconciliation_state: None,
            reconciliation_attempt_count: 0,
            reconciliation_error: None,
            reconciled_at: None,
            created_at: mutation.created_at,
            updated_at: mutation.created_at,
        })?;

        transaction.execute(
            "UPDATE operations SET request_fingerprint = ?2 WHERE id = ?1",
            params![mutation.id, request_fingerprint],
        )?;
        crate::fault::point("db.mutation.intent_written");
        let result = apply(&repositories)?;
        let encoded = serde_json::to_string(&result)?;
        repositories.transaction.execute(
            "UPDATE operations SET status = 'succeeded', result_json = ?2, updated_at = ?3 \
             WHERE id = ?1",
            params![mutation.id, encoded, mutation.created_at],
        )?;
        crate::fault::point("db.mutation.written");
        transaction.commit()?;
        crate::fault::point("db.mutation.committed");
        Ok(MutationOutcome::Applied(result))
    }

    /// Atomically claims a ready task and creates its active assignment.
    pub(crate) fn claim_task(
        &mut self,
        claim: &ClaimTaskMutation,
    ) -> Result<MutationOutcome<ClaimTaskResult>, StoreError> {
        let mutation = Mutation {
            id: claim.operation_id,
            run_id: claim.run_id,
            kind: "task.claim".to_owned(),
            actor_agent_id: claim.actor_agent_id,
            // Retries may allocate fresh bookkeeping values before finding the
            // durable result, so only caller intent belongs to request identity.
            request: json!({
                "agent_id": claim.agent_id,
                "task_id": claim.task_id,
            }),
            created_at: claim.claimed_at,
        };

        self.mutate(&mutation, |repositories| {
            let result = repositories.compare_and_set_claim(claim)?;
            repositories.append_task_claim_events(claim, &result)?;
            Ok(result)
        })
    }

    /// Applies a non-claim task transition and replays its durable result.
    #[cfg(test)]
    pub(crate) fn transition_task(
        &mut self,
        transition: &TaskTransitionMutation,
    ) -> Result<MutationOutcome<TaskTransitionResult>, StoreError> {
        self.transition_task_with_fingerprint(transition, None)
    }

    pub(crate) fn transition_task_with_fingerprint(
        &mut self,
        transition: &TaskTransitionMutation,
        request_fingerprint: Option<&str>,
    ) -> Result<MutationOutcome<TaskTransitionResult>, StoreError> {
        let mutation = Mutation {
            id: transition.operation_id,
            run_id: transition.run_id,
            kind: format!("task.{}", transition.transition),
            actor_agent_id: transition.actor_agent_id,
            request: json!({
                "result": transition.result,
                "summary": transition.summary,
                "task_id": transition.task_id,
                "transition": transition.transition,
            }),
            created_at: transition.transitioned_at,
        };

        self.mutate_with_fingerprint(
            &mutation,
            request_fingerprint,
            |repositories| {
                let active_claim = repositories.active_claim_for_task(
                    transition.run_id,
                    transition.task_id,
                )?;
                let active_assignment = repositories
                    .active_assignment_for_task(
                        transition.run_id,
                        transition.task_id,
                    )?;
                let result = repositories.apply_task_transition(transition)?;
                if let TaskTransitionResult::Transitioned {
                    previous_status,
                    status,
                } = result
                {
                    let task = repositories
                        .task(transition.task_id)?
                        .ok_or_else(|| StoreError::CorruptTaskState {
                            id: transition.task_id,
                            reason: "the transitioned task disappeared"
                                .to_owned(),
                        })?;
                    let task_event = repositories.append_event(&NewEvent {
                    run_id: transition.run_id,
                    kind: EventKind::TaskLifecycleChanged,
                    actor: mutation_actor(transition.actor_agent_id),
                    subject: transition.task_id.to_string(),
                    project_id: Some(task.project_id),
                    agent_id: transition.actor_agent_id,
                    task_id: Some(transition.task_id),
                    operation_id: Some(transition.operation_id),
                    correlation_id: None,
                    causation_id: None,
                    data: json!({
                        "previous_status": previous_status,
                        "status": status,
                        "transition": transition.transition,
                    }),
                    summary: format!(
                        "Task {} changed from {previous_status} to {status}.",
                        transition.task_id
                    ),
                    created_at: transition.transitioned_at,
                })?;
                    if previous_status == TaskStatus::InProgress {
                        let assignment =
                            active_assignment.ok_or_else(|| {
                                StoreError::CorruptTaskState {
                            id: transition.task_id,
                            reason:
                                "the transitioned task had no active assignment"
                                    .to_owned(),
                        }
                            })?;
                        let claim = active_claim.ok_or_else(|| {
                            StoreError::CorruptTaskState {
                                id: transition.task_id,
                                reason:
                                    "the transitioned task had no active claim"
                                        .to_owned(),
                            }
                        })?;
                        let assignment_state = match transition.transition {
                            TaskTransition::Reopen => "released",
                            TaskTransition::Submit => "completed",
                            TaskTransition::Cancel => "canceled",
                            TaskTransition::Close => unreachable!(
                                "an in-progress task cannot close directly"
                            ),
                        };
                        repositories.append_event(&NewEvent {
                            run_id: transition.run_id,
                            kind: EventKind::AssignmentLifecycleChanged,
                            actor: mutation_actor(transition.actor_agent_id),
                            subject: assignment.id.to_string(),
                            project_id: Some(task.project_id),
                            agent_id: Some(assignment.agent_id),
                            task_id: Some(transition.task_id),
                            operation_id: Some(transition.operation_id),
                            correlation_id: Some(task_event.id),
                            causation_id: Some(task_event.id),
                            data: json!({
                                "previous_state": assignment.state,
                                "state": assignment_state,
                            }),
                            summary: format!(
                                "Assignment {} changed to {assignment_state}.",
                                assignment.id
                            ),
                            created_at: transition.transitioned_at,
                        })?;
                        repositories.append_event(&NewEvent {
                            run_id: transition.run_id,
                            kind: EventKind::ClaimReleased,
                            actor: mutation_actor(transition.actor_agent_id),
                            subject: transition.task_id.to_string(),
                            project_id: Some(task.project_id),
                            agent_id: Some(claim.agent_id),
                            task_id: Some(transition.task_id),
                            operation_id: Some(transition.operation_id),
                            correlation_id: Some(task_event.id),
                            causation_id: Some(task_event.id),
                            data: json!({"claim_id": claim.id}),
                            summary: format!(
                                "Released the claim on task {}.",
                                transition.task_id
                            ),
                            created_at: transition.transitioned_at,
                        })?;
                    }
                }
                Ok(result)
            },
        )
    }

    /// Explicitly acknowledges a recipient-local cursor without allowing regressions.
    pub(crate) fn acknowledge_messages(
        &mut self,
        acknowledgement: &AcknowledgeMessagesMutation,
    ) -> Result<MutationOutcome<AcknowledgeMessagesResult>, StoreError> {
        let mutation = Mutation {
            id: acknowledgement.operation_id,
            run_id: acknowledgement.run_id,
            kind: "message.acknowledge".to_owned(),
            actor_agent_id: Some(acknowledgement.agent_id),
            request: json!({
                "agent_id": acknowledgement.agent_id,
                "through": acknowledgement.through,
            }),
            created_at: acknowledgement.acknowledged_at,
        };

        self.mutate(&mutation, |repositories| {
            let result = repositories
                .apply_message_acknowledgement(acknowledgement)?;
            if let AcknowledgeMessagesResult::Acknowledged {
                acknowledged_through,
                acknowledged_count,
            } = result
                && acknowledged_count > 0
            {
                repositories.append_event(&NewEvent {
                    run_id: acknowledgement.run_id,
                    kind: EventKind::MessageAcknowledged,
                    actor: acknowledgement.agent_id.to_string(),
                    subject: acknowledgement.agent_id.to_string(),
                    project_id: None,
                    agent_id: Some(acknowledgement.agent_id),
                    task_id: None,
                    operation_id: Some(acknowledgement.operation_id),
                    correlation_id: None,
                    causation_id: None,
                    data: json!({
                        "acknowledged_count": acknowledged_count,
                        "acknowledged_through": acknowledged_through,
                    }),
                    summary: format!(
                        "Acknowledged {acknowledged_count} message(s) through cursor {acknowledged_through}."
                    ),
                    created_at: acknowledgement.acknowledged_at,
                })?;
            }
            Ok(result)
        })
    }

    #[cfg(test)]
    fn table_names(&self) -> Result<BTreeSet<String>, StoreError> {
        let mut statement = self.connection.prepare(
            "SELECT name FROM sqlite_schema \
             WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
        )?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        Ok(rows.collect::<Result<BTreeSet<_>, _>>()?)
    }
}

impl Repositories<'_, '_> {
    pub(crate) fn insert_run(&self, run: &RunRecord) -> Result<(), StoreError> {
        self.transaction.execute(
            "INSERT INTO runs (id, status, created_at, stopped_at) \
             VALUES (?1, ?2, ?3, ?4)",
            params![run.id, run.status, run.created_at, run.stopped_at],
        )?;
        Ok(())
    }

    pub(crate) fn run(
        &self,
        id: RunId,
    ) -> Result<Option<RunRecord>, StoreError> {
        Ok(self
            .transaction
            .query_row(
                "SELECT id, status, created_at, stopped_at FROM runs WHERE id = ?1",
                [id],
                |row| {
                    Ok(RunRecord {
                        id: row.get(0)?,
                        status: row.get(1)?,
                        created_at: row.get(2)?,
                        stopped_at: row.get(3)?,
                    })
                },
            )
            .optional()?)
    }

    /// Marks an active run stopped during orderly supervisor shutdown.
    pub(crate) fn stop_run(
        &self,
        id: RunId,
        stopped_at: i64,
    ) -> Result<(), StoreError> {
        let changed = self.transaction.execute(
            "UPDATE runs SET status = 'stopped', stopped_at = ?2 \
             WHERE id = ?1 AND status = 'active' AND stopped_at IS NULL",
            params![id, stopped_at],
        )?;
        if changed == 1 {
            Ok(())
        } else {
            Err(StoreError::RunNotActive { id })
        }
    }

    pub(crate) fn insert_configuration_snapshot(
        &self,
        snapshot: &ConfigurationSnapshotRecord,
    ) -> Result<(), StoreError> {
        let document = serde_json::to_string(&snapshot.document)?;
        self.transaction.execute(
            "INSERT INTO configuration_snapshots (\
                 id, run_id, project_id, scope, schema_version, fingerprint, document_json, created_at\
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                snapshot.id,
                snapshot.run_id,
                snapshot.project_id,
                snapshot.scope,
                snapshot.schema_version,
                snapshot.fingerprint,
                document,
                snapshot.created_at,
            ],
        )?;
        Ok(())
    }

    pub(crate) fn configuration_snapshot(
        &self,
        id: i64,
    ) -> Result<Option<ConfigurationSnapshotRecord>, StoreError> {
        Ok(self
            .transaction
            .query_row(
                "SELECT id, run_id, project_id, scope, schema_version, fingerprint, \
                        document_json, created_at \
                 FROM configuration_snapshots WHERE id = ?1",
                [id],
                |row| {
                    Ok(ConfigurationSnapshotRecord {
                        id: row.get(0)?,
                        run_id: row.get(1)?,
                        project_id: row.get(2)?,
                        scope: row.get(3)?,
                        schema_version: row.get(4)?,
                        fingerprint: row.get(5)?,
                        document: decode_json(row, 6)?,
                        created_at: row.get(7)?,
                    })
                },
            )
            .optional()?)
    }

    pub(crate) fn insert_project(
        &self,
        project: &ProjectRecord,
    ) -> Result<(), StoreError> {
        let identity = serde_json::to_string(&project.identity)?;
        self.transaction.execute(
            "INSERT INTO projects (\
                 id, run_id, alias, original_path, canonical_path, identity_json, \
                 is_primary, attached_at\
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                project.id,
                project.run_id,
                project.alias,
                path_bytes(&project.original_path),
                path_bytes(&project.canonical_path),
                identity,
                project.is_primary,
                project.attached_at,
            ],
        )?;
        Ok(())
    }

    pub(crate) fn project(
        &self,
        id: ProjectId,
    ) -> Result<Option<ProjectRecord>, StoreError> {
        Ok(self
            .transaction
            .query_row(
                "SELECT id, run_id, alias, original_path, canonical_path, identity_json, \
                        is_primary, attached_at \
                 FROM projects WHERE id = ?1",
                [id],
                |row| {
                    Ok(ProjectRecord {
                        id: row.get(0)?,
                        run_id: row.get(1)?,
                        alias: row.get(2)?,
                        original_path: decode_path(row, 3)?,
                        canonical_path: decode_path(row, 4)?,
                        identity: decode_json(row, 5)?,
                        is_primary: row.get(6)?,
                        attached_at: row.get(7)?,
                    })
                },
            )
            .optional()?)
    }

    pub(crate) fn project_by_alias(
        &self,
        run_id: RunId,
        alias: &str,
    ) -> Result<Option<ProjectRecord>, StoreError> {
        Ok(self
            .transaction
            .query_row(
                "SELECT id, run_id, alias, original_path, canonical_path, identity_json, \
                        is_primary, attached_at \
                 FROM projects WHERE run_id = ?1 AND alias = ?2",
                params![run_id, alias],
                |row| {
                    Ok(ProjectRecord {
                        id: row.get(0)?,
                        run_id: row.get(1)?,
                        alias: row.get(2)?,
                        original_path: decode_path(row, 3)?,
                        canonical_path: decode_path(row, 4)?,
                        identity: decode_json(row, 5)?,
                        is_primary: row.get(6)?,
                        attached_at: row.get(7)?,
                    })
                },
            )
            .optional()?)
    }

    pub(crate) fn projects(
        &self,
        run_id: RunId,
    ) -> Result<Vec<ProjectRecord>, StoreError> {
        let mut statement = self.transaction.prepare(
            "SELECT id, run_id, alias, original_path, canonical_path, identity_json, \
                    is_primary, attached_at \
             FROM projects WHERE run_id = ?1 ORDER BY is_primary DESC, alias, id",
        )?;
        let rows = statement.query_map([run_id], |row| {
            Ok(ProjectRecord {
                id: row.get(0)?,
                run_id: row.get(1)?,
                alias: row.get(2)?,
                original_path: decode_path(row, 3)?,
                canonical_path: decode_path(row, 4)?,
                identity: decode_json(row, 5)?,
                is_primary: row.get(6)?,
                attached_at: row.get(7)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub(crate) fn insert_agent(
        &self,
        agent: &AgentRecord,
    ) -> Result<(), StoreError> {
        self.transaction.execute(
            "INSERT INTO agents (id, run_id, role, generation, state, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                agent.id,
                agent.run_id,
                agent.role,
                agent.generation,
                agent.state.as_str(),
                agent.created_at,
            ],
        )?;
        Ok(())
    }

    pub(crate) fn agent(
        &self,
        id: AgentId,
    ) -> Result<Option<AgentRecord>, StoreError> {
        Ok(self
            .transaction
            .query_row(
                "SELECT id, run_id, role, generation, state, created_at \
                 FROM agents WHERE id = ?1",
                [id],
                |row| {
                    Ok(AgentRecord {
                        id: row.get(0)?,
                        run_id: row.get(1)?,
                        role: row.get(2)?,
                        generation: row.get(3)?,
                        state: decode_lifecycle(row, 4)?,
                        created_at: row.get(5)?,
                    })
                },
            )
            .optional()?)
    }

    pub(crate) fn agents(
        &self,
        run_id: RunId,
    ) -> Result<Vec<AgentRecord>, StoreError> {
        let mut statement = self.transaction.prepare(
            "SELECT id, run_id, role, generation, state, created_at \
             FROM agents WHERE run_id = ?1 ORDER BY created_at, rowid",
        )?;
        let rows = statement.query_map([run_id], |row| {
            Ok(AgentRecord {
                id: row.get(0)?,
                run_id: row.get(1)?,
                role: row.get(2)?,
                generation: row.get(3)?,
                state: decode_lifecycle(row, 4)?,
                created_at: row.get(5)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Starts a replacement session generation only after the previous one is terminal.
    pub(crate) fn start_agent_generation(
        &self,
        run_id: RunId,
        agent_id: AgentId,
        current_generation: i64,
        next_generation: i64,
    ) -> Result<bool, StoreError> {
        let Some(agent) = self.agent(agent_id)? else {
            return Ok(false);
        };
        if agent.run_id != run_id
            || agent.generation != current_generation
            || !agent.state.is_terminal()
            || next_generation <= current_generation
        {
            return Ok(false);
        }
        let changed = self.transaction.execute(
            "UPDATE agents SET generation = ?3, state = 'starting' \
             WHERE id = ?1 AND run_id = ?2 AND generation = ?4",
            params![agent_id, run_id, next_generation, current_generation],
        )?;
        Ok(changed == 1)
    }

    pub(crate) fn insert_session(
        &self,
        session: &SessionRecord,
    ) -> Result<(), StoreError> {
        self.transaction.execute(
            "INSERT INTO sessions (\
                 id, run_id, agent_id, generation, provider, provider_session_id, \
                 reconciliation_state, state, transcript_path, created_at, ended_at, \
                 reconciled_at, process_owner\
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                session.id,
                session.run_id,
                session.agent_id,
                session.generation,
                session.provider,
                session.provider_session_id,
                session.reconciliation_state.as_str(),
                session.state.as_str(),
                path_bytes(&session.transcript_path),
                session.created_at,
                session.ended_at,
                session.reconciled_at,
                session.process_owner.as_str(),
            ],
        )?;
        Ok(())
    }

    pub(crate) fn session(
        &self,
        id: SessionId,
    ) -> Result<Option<SessionRecord>, StoreError> {
        Ok(self
            .transaction
            .query_row(
                "SELECT id, run_id, agent_id, generation, provider, provider_session_id, \
                        reconciliation_state, state, transcript_path, created_at, ended_at, \
                        reconciled_at, process_owner \
                 FROM sessions WHERE id = ?1",
                [id],
                |row| {
                    Ok(SessionRecord {
                        id: row.get(0)?,
                        run_id: row.get(1)?,
                        agent_id: row.get(2)?,
                        generation: row.get(3)?,
                        provider: row.get(4)?,
                        provider_session_id: row.get(5)?,
                        reconciliation_state: decode_external_resource_state(
                            row, 6,
                        )?,
                        state: decode_lifecycle(row, 7)?,
                        transcript_path: decode_path(row, 8)?,
                        created_at: row.get(9)?,
                        ended_at: row.get(10)?,
                        reconciled_at: row.get(11)?,
                        process_owner: decode_session_process_owner(row, 12)?,
                    })
                },
            )
            .optional()?)
    }

    pub(crate) fn latest_session_for_agent(
        &self,
        run_id: RunId,
        agent_id: AgentId,
    ) -> Result<Option<SessionRecord>, StoreError> {
        Ok(self
            .transaction
            .query_row(
                "SELECT id, run_id, agent_id, generation, provider, provider_session_id, \
                        reconciliation_state, state, transcript_path, created_at, ended_at, \
                        reconciled_at, process_owner \
                 FROM sessions WHERE run_id = ?1 AND agent_id = ?2 \
                 ORDER BY generation DESC LIMIT 1",
                params![run_id, agent_id],
                |row| {
                    Ok(SessionRecord {
                        id: row.get(0)?,
                        run_id: row.get(1)?,
                        agent_id: row.get(2)?,
                        generation: row.get(3)?,
                        provider: row.get(4)?,
                        provider_session_id: row.get(5)?,
                        reconciliation_state: decode_external_resource_state(
                            row, 6,
                        )?,
                        state: decode_lifecycle(row, 7)?,
                        transcript_path: decode_path(row, 8)?,
                        created_at: row.get(9)?,
                        ended_at: row.get(10)?,
                        reconciled_at: row.get(11)?,
                        process_owner: decode_session_process_owner(row, 12)?,
                    })
                },
            )
            .optional()?)
    }

    pub(crate) fn sessions(
        &self,
        run_id: RunId,
    ) -> Result<Vec<SessionRecord>, StoreError> {
        let mut statement = self.transaction.prepare(
            "SELECT id, run_id, agent_id, generation, provider, provider_session_id, \
                    reconciliation_state, state, transcript_path, created_at, ended_at, \
                    reconciled_at, process_owner \
             FROM sessions WHERE run_id = ?1 ORDER BY created_at, rowid",
        )?;
        let rows = statement.query_map([run_id], |row| {
            Ok(SessionRecord {
                id: row.get(0)?,
                run_id: row.get(1)?,
                agent_id: row.get(2)?,
                generation: row.get(3)?,
                provider: row.get(4)?,
                provider_session_id: row.get(5)?,
                reconciliation_state: decode_external_resource_state(row, 6)?,
                state: decode_lifecycle(row, 7)?,
                transcript_path: decode_path(row, 8)?,
                created_at: row.get(9)?,
                ended_at: row.get(10)?,
                reconciled_at: row.get(11)?,
                process_owner: decode_session_process_owner(row, 12)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Checks durable ownership before accepting provider data or control.
    pub(crate) fn session_scope_is_current(
        &self,
        scope: crate::auth::SessionScope,
    ) -> Result<bool, StoreError> {
        Ok(self.transaction.query_row(
            "SELECT EXISTS (SELECT 1 FROM sessions AS session \
             JOIN agents AS agent ON agent.id = session.agent_id AND agent.run_id = session.run_id \
             JOIN runs AS run ON run.id = session.run_id \
             WHERE session.id = ?1 AND session.run_id = ?2 AND session.agent_id = ?3 \
               AND session.generation = ?4 AND agent.generation = ?4 AND run.status = 'active')",
            params![scope.session_id, scope.run_id, scope.agent_id, scope.generation],
            |row| row.get(0),
        )?)
    }

    /// Records the provider identity observed after a launch side effect.
    pub(crate) fn record_session_launch_observation(
        &self,
        scope: crate::auth::SessionScope,
        provider_session_id: &str,
        observed_at: i64,
    ) -> Result<SessionTransitionOutcome, StoreError> {
        let Some(session) = self.session(scope.session_id)? else {
            return Ok(SessionTransitionOutcome::Stale);
        };
        if !self.session_scope_is_current(scope)? {
            return Ok(SessionTransitionOutcome::Stale);
        }
        if session.reconciliation_state == ExternalResourceState::Observed
            && session.provider_session_id.as_deref()
                == Some(provider_session_id)
        {
            return Ok(SessionTransitionOutcome::Unchanged);
        }
        self.transaction.execute(
            "UPDATE sessions \
             SET provider_session_id = ?2, reconciliation_state = 'observed', \
                 reconciled_at = ?3 \
             WHERE id = ?1 AND run_id = ?4 AND agent_id = ?5 AND generation = ?6",
            params![
                scope.session_id,
                provider_session_id,
                observed_at,
                scope.run_id,
                scope.agent_id,
                scope.generation,
            ],
        )?;
        Ok(SessionTransitionOutcome::Applied)
    }

    /// Records conservative knowledge about a durable session intent.
    pub(crate) fn record_session_reconciliation_state(
        &self,
        scope: crate::auth::SessionScope,
        state: ExternalResourceState,
        reconciled_at: i64,
    ) -> Result<SessionTransitionOutcome, StoreError> {
        let Some(session) = self.session(scope.session_id)? else {
            return Ok(SessionTransitionOutcome::Stale);
        };
        if !self.session_scope_is_current(scope)? {
            return Ok(SessionTransitionOutcome::Stale);
        }
        if session.reconciliation_state == state {
            return Ok(SessionTransitionOutcome::Unchanged);
        }
        self.transaction.execute(
            "UPDATE sessions SET reconciliation_state = ?2, reconciled_at = ?3 \
             WHERE id = ?1 AND run_id = ?4 AND agent_id = ?5 AND generation = ?6",
            params![
                scope.session_id,
                state.as_str(),
                reconciled_at,
                scope.run_id,
                scope.agent_id,
                scope.generation,
            ],
        )?;
        Ok(SessionTransitionOutcome::Applied)
    }

    /// Applies an observation only to the session and agent generation it owns.
    pub(crate) fn record_session_lifecycle(
        &self,
        scope: crate::auth::SessionScope,
        observed: LifecycleState,
        observed_at: i64,
    ) -> Result<SessionTransitionOutcome, StoreError> {
        let Some(session) = self.session(scope.session_id)? else {
            return Ok(SessionTransitionOutcome::Stale);
        };
        if !self.session_scope_is_current(scope)? {
            return Ok(SessionTransitionOutcome::Stale);
        }

        let Some(agent) = self.agent(scope.agent_id)? else {
            return Ok(SessionTransitionOutcome::Stale);
        };
        if agent.run_id != scope.run_id || agent.generation != scope.generation
        {
            return Ok(SessionTransitionOutcome::Stale);
        }
        if agent.state != session.state {
            return Err(StoreError::InconsistentSessionLifecycle {
                session_id: session.id,
                agent_id: agent.id,
            });
        }
        if session.state == observed {
            return Ok(SessionTransitionOutcome::Unchanged);
        }
        if !session.state.allows(observed) {
            return Err(StoreError::InvalidSessionTransition {
                session_id: session.id,
                current: session.state,
                observed,
            });
        }

        let ended_at = observed.is_terminal().then_some(observed_at);
        self.transaction.execute(
            "UPDATE sessions SET state = ?2, ended_at = ?3 \
             WHERE id = ?1 AND run_id = ?4 AND agent_id = ?5 AND generation = ?6",
            params![
                session.id,
                observed.as_str(),
                ended_at,
                scope.run_id,
                scope.agent_id,
                scope.generation,
            ],
        )?;
        self.transaction.execute(
            "UPDATE agents SET state = ?2 \
             WHERE id = ?1 AND run_id = ?3 AND generation = ?4",
            params![
                agent.id,
                observed.as_str(),
                scope.run_id,
                scope.generation,
            ],
        )?;
        if observed.is_terminal() {
            self.transaction.execute(
                "UPDATE session_credentials SET revoked_at = ?2 \
                 WHERE session_id = ?1 AND revoked_at IS NULL",
                params![session.id, observed_at],
            )?;
        }
        Ok(SessionTransitionOutcome::Applied)
    }

    /// Replaces an agent's active credential with the verifier for a new session.
    pub(crate) fn activate_session_credential(
        &self,
        credential: &SessionCredentialRecord,
    ) -> Result<(), StoreError> {
        if credential.revoked_at.is_some() {
            return Err(StoreError::CredentialAlreadyRevoked {
                session_id: credential.session_id,
            });
        }
        if !self.session_scope_is_current(crate::auth::SessionScope {
            run_id: credential.run_id,
            agent_id: credential.agent_id,
            session_id: credential.session_id,
            generation: credential.generation,
        })? {
            return Err(StoreError::InconsistentSessionLifecycle {
                session_id: credential.session_id,
                agent_id: credential.agent_id,
            });
        }
        self.transaction.execute(
            "UPDATE session_credentials SET revoked_at = ?3 \
             WHERE run_id = ?1 AND agent_id = ?2 AND revoked_at IS NULL",
            params![
                credential.run_id,
                credential.agent_id,
                credential.created_at,
            ],
        )?;
        self.transaction.execute(
            "INSERT INTO session_credentials (\
                 session_id, run_id, agent_id, generation, token_verifier, created_at, revoked_at\
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL)",
            params![
                credential.session_id,
                credential.run_id,
                credential.agent_id,
                credential.generation,
                credential.token_verifier.as_bytes().as_slice(),
                credential.created_at,
            ],
        )?;
        Ok(())
    }

    /// Rotates a verifier only while its launch remains a known desired intent.
    pub(crate) fn replace_desired_session_credential(
        &self,
        credential: &SessionCredentialRecord,
    ) -> Result<(), StoreError> {
        if credential.revoked_at.is_some() {
            return Err(StoreError::CredentialAlreadyRevoked {
                session_id: credential.session_id,
            });
        }
        let updated = self.transaction.execute(
            "UPDATE session_credentials \
             SET token_verifier = ?5, created_at = ?6 \
             WHERE session_id = ?1 AND run_id = ?2 AND agent_id = ?3 \
               AND generation = ?4 AND revoked_at IS NULL \
               AND EXISTS (\
                   SELECT 1 FROM sessions \
                   WHERE sessions.id = session_credentials.session_id \
                     AND sessions.reconciliation_state = 'desired'\
               )",
            params![
                credential.session_id,
                credential.run_id,
                credential.agent_id,
                credential.generation,
                credential.token_verifier.as_bytes().as_slice(),
                credential.created_at,
            ],
        )?;
        if updated != 1 {
            return Err(StoreError::InconsistentSessionLifecycle {
                session_id: credential.session_id,
                agent_id: credential.agent_id,
            });
        }
        Ok(())
    }

    pub(crate) fn session_credential(
        &self,
        session_id: SessionId,
    ) -> Result<Option<SessionCredentialRecord>, StoreError> {
        Ok(self
            .transaction
            .query_row(
                "SELECT session_id, run_id, agent_id, generation, token_verifier, \
                        created_at, revoked_at \
                 FROM session_credentials WHERE session_id = ?1",
                [session_id],
                decode_session_credential,
            )
            .optional()?)
    }

    /// Resolves only the unrevoked credential for the agent's current live generation.
    pub(crate) fn active_session_credential(
        &self,
        run_id: RunId,
        agent_id: AgentId,
        session_id: SessionId,
    ) -> Result<Option<SessionCredentialRecord>, StoreError> {
        Ok(self
            .transaction
            .query_row(
                "SELECT credential.session_id, credential.run_id, credential.agent_id, \
                        credential.generation, credential.token_verifier, \
                        credential.created_at, credential.revoked_at \
                 FROM session_credentials AS credential \
                 JOIN sessions AS session \
                   ON session.run_id = credential.run_id \
                  AND session.agent_id = credential.agent_id \
                  AND session.id = credential.session_id \
                  AND session.generation = credential.generation \
                 JOIN agents AS agent \
                   ON agent.run_id = credential.run_id \
                  AND agent.id = credential.agent_id \
                  AND agent.generation = credential.generation \
                 JOIN runs AS run ON run.id = credential.run_id \
                 WHERE credential.run_id = ?1 \
                   AND credential.agent_id = ?2 \
                   AND credential.session_id = ?3 \
                   AND credential.revoked_at IS NULL \
                   AND session.ended_at IS NULL \
                   AND run.status = 'active'",
                params![run_id, agent_id, session_id],
                decode_session_credential,
            )
            .optional()?)
    }

    pub(crate) fn insert_task_group(
        &self,
        group: &TaskGroupRecord,
    ) -> Result<(), StoreError> {
        self.transaction.execute(
            "INSERT INTO task_groups (id, run_id, name, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![group.id, group.run_id, group.name, group.created_at],
        )?;
        Ok(())
    }

    pub(crate) fn task_group(
        &self,
        id: i64,
    ) -> Result<Option<TaskGroupRecord>, StoreError> {
        Ok(self
            .transaction
            .query_row(
                "SELECT id, run_id, name, created_at FROM task_groups WHERE id = ?1",
                [id],
                |row| {
                    Ok(TaskGroupRecord {
                        id: row.get(0)?,
                        run_id: row.get(1)?,
                        name: row.get(2)?,
                        created_at: row.get(3)?,
                    })
                },
            )
            .optional()?)
    }

    pub(crate) fn task_group_by_name(
        &self,
        run_id: RunId,
        name: &str,
    ) -> Result<Option<TaskGroupRecord>, StoreError> {
        Ok(self
            .transaction
            .query_row(
                "SELECT id, run_id, name, created_at FROM task_groups \
                 WHERE run_id = ?1 AND name = ?2 ORDER BY id LIMIT 1",
                params![run_id, name],
                |row| {
                    Ok(TaskGroupRecord {
                        id: row.get(0)?,
                        run_id: row.get(1)?,
                        name: row.get(2)?,
                        created_at: row.get(3)?,
                    })
                },
            )
            .optional()?)
    }

    pub(crate) fn insert_named_task_group(
        &self,
        run_id: RunId,
        name: &str,
        created_at: i64,
    ) -> Result<i64, StoreError> {
        self.transaction.execute(
            "INSERT INTO task_groups (run_id, name, created_at) VALUES (?1, ?2, ?3)",
            params![run_id, name, created_at],
        )?;
        Ok(self.transaction.last_insert_rowid())
    }

    pub(crate) fn insert_task(
        &self,
        task: &TaskRecord,
    ) -> Result<(), StoreError> {
        let result = task
            .result
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        self.transaction.execute(
            "INSERT INTO tasks (\
                 id, run_id, project_id, group_id, title, description, status, result_json, \
                 created_at, updated_at\
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                task.id,
                task.run_id,
                task.project_id,
                task.group_id,
                task.title,
                task.description,
                task.status,
                result,
                task.created_at,
                task.updated_at,
            ],
        )?;
        Ok(())
    }

    pub(crate) fn task(
        &self,
        id: TaskId,
    ) -> Result<Option<TaskRecord>, StoreError> {
        Ok(self
            .transaction
            .query_row(
                "SELECT id, run_id, project_id, group_id, title, description, status, \
                        result_json, created_at, updated_at \
                 FROM tasks WHERE id = ?1",
                [id],
                |row| {
                    Ok(TaskRecord {
                        id: row.get(0)?,
                        run_id: row.get(1)?,
                        project_id: row.get(2)?,
                        group_id: row.get(3)?,
                        title: row.get(4)?,
                        description: row.get(5)?,
                        status: row.get(6)?,
                        result: decode_optional_json(row, 7)?,
                        created_at: row.get(8)?,
                        updated_at: row.get(9)?,
                    })
                },
            )
            .optional()?)
    }

    /// Reads the current facts that determine whether a task is ready.
    pub(crate) fn task_readiness(
        &self,
        id: TaskId,
    ) -> Result<Option<TaskReadiness>, StoreError> {
        let status = self
            .transaction
            .query_row("SELECT status FROM tasks WHERE id = ?1", [id], |row| {
                row.get::<_, TaskStatus>(0)
            })
            .optional()?;
        let Some(status) = status else {
            return Ok(None);
        };

        let unresolved_dependencies = {
            let mut statement = self.transaction.prepare(
                "SELECT edge.dependency_task_id \
                 FROM task_dependencies AS edge \
                 JOIN tasks AS dependency \
                   ON dependency.id = edge.dependency_task_id \
                  AND dependency.run_id = edge.run_id \
                 WHERE edge.task_id = ?1 AND dependency.status <> 'closed' \
                 ORDER BY edge.dependency_task_id",
            )?;
            let rows =
                statement.query_map([id], |row| row.get::<_, TaskId>(0))?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        let has_active_claim = self.transaction.query_row(
            "SELECT EXISTS (\
                 SELECT 1 FROM claims WHERE task_id = ?1 AND released_at IS NULL\
             )",
            [id],
            |row| row.get::<_, bool>(0),
        )?;

        Ok(Some(TaskReadiness {
            status,
            unresolved_dependencies,
            has_active_claim,
        }))
    }

    /// Lists ready tasks in stable creation order.
    pub(crate) fn ready_tasks(
        &self,
        run_id: RunId,
    ) -> Result<Vec<TaskRecord>, StoreError> {
        let mut statement = self.transaction.prepare(
            "SELECT candidate.id, candidate.run_id, candidate.project_id, \
                    candidate.group_id, candidate.title, candidate.description, \
                    candidate.status, candidate.result_json, candidate.created_at, \
                    candidate.updated_at \
             FROM tasks AS candidate \
             WHERE candidate.run_id = ?1 AND candidate.status = 'open' \
               AND NOT EXISTS ( \
                   SELECT 1 FROM claims AS active_claim \
                   WHERE active_claim.task_id = candidate.id \
                     AND active_claim.run_id = candidate.run_id \
                     AND active_claim.released_at IS NULL \
               ) \
               AND NOT EXISTS ( \
                   SELECT 1 \
                   FROM task_dependencies AS edge \
                   JOIN tasks AS dependency \
                     ON dependency.id = edge.dependency_task_id \
                    AND dependency.run_id = edge.run_id \
                   WHERE edge.task_id = candidate.id \
                     AND edge.run_id = candidate.run_id \
                     AND dependency.status <> 'closed' \
               ) \
             ORDER BY candidate.created_at, candidate.id",
        )?;
        let rows = statement.query_map([run_id], |row| {
            Ok(TaskRecord {
                id: row.get(0)?,
                run_id: row.get(1)?,
                project_id: row.get(2)?,
                group_id: row.get(3)?,
                title: row.get(4)?,
                description: row.get(5)?,
                status: row.get(6)?,
                result: decode_optional_json(row, 7)?,
                created_at: row.get(8)?,
                updated_at: row.get(9)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub(crate) fn tasks(
        &self,
        run_id: RunId,
    ) -> Result<Vec<TaskRecord>, StoreError> {
        let mut statement = self.transaction.prepare(
            "SELECT id, run_id, project_id, group_id, title, description, status, \
                    result_json, created_at, updated_at \
             FROM tasks WHERE run_id = ?1 ORDER BY created_at, id",
        )?;
        let rows = statement.query_map([run_id], |row| {
            Ok(TaskRecord {
                id: row.get(0)?,
                run_id: row.get(1)?,
                project_id: row.get(2)?,
                group_id: row.get(3)?,
                title: row.get(4)?,
                description: row.get(5)?,
                status: row.get(6)?,
                result: decode_optional_json(row, 7)?,
                created_at: row.get(8)?,
                updated_at: row.get(9)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    fn apply_task_transition(
        &self,
        mutation: &TaskTransitionMutation,
    ) -> Result<TaskTransitionResult, StoreError> {
        let task = self.task(mutation.task_id)?;
        let Some(task) = task.filter(|task| task.run_id == mutation.run_id)
        else {
            return Ok(TaskTransitionResult::Rejected(
                TaskTransitionRejection::TaskNotFound,
            ));
        };
        let Some(status) = task.status.transition(mutation.transition) else {
            return Ok(TaskTransitionResult::Rejected(
                TaskTransitionRejection::InvalidStatus {
                    status: task.status,
                },
            ));
        };

        if mutation.transition == TaskTransition::Close
            && task.status == TaskStatus::Submitted
            && let Some(assignment) = self
                .latest_assignment_for_task(mutation.run_id, mutation.task_id)?
            && let Some(workspace) = self.workspace(assignment.id)?
            && workspace.kind == "worktree"
            && workspace.target_commit.is_none()
        {
            return Ok(TaskTransitionResult::Rejected(
                TaskTransitionRejection::AcceptanceNotMet,
            ));
        }

        if task.status == TaskStatus::InProgress {
            self.release_task_ownership(mutation)?;
        }

        let result = match mutation.transition {
            TaskTransition::Reopen => None,
            TaskTransition::Submit => mutation.result.clone(),
            TaskTransition::Close => mutation.result.clone().or(task.result),
            TaskTransition::Cancel => task.result,
        }
        .map(|result| serde_json::to_string(&result))
        .transpose()?;
        self.transaction.execute(
            "UPDATE tasks SET status = ?2, result_json = ?3, updated_at = ?4 \
             WHERE id = ?1 AND run_id = ?5",
            params![
                mutation.task_id,
                status,
                result,
                mutation.transitioned_at,
                mutation.run_id,
            ],
        )?;

        Ok(TaskTransitionResult::Transitioned {
            previous_status: task.status,
            status,
        })
    }

    fn release_task_ownership(
        &self,
        mutation: &TaskTransitionMutation,
    ) -> Result<(), StoreError> {
        let assignment_state = match mutation.transition {
            TaskTransition::Reopen => "released",
            TaskTransition::Submit => "completed",
            TaskTransition::Cancel => "canceled",
            TaskTransition::Close => {
                return Err(StoreError::CorruptTaskState {
                    id: mutation.task_id,
                    reason: "an in-progress task cannot close directly"
                        .to_owned(),
                });
            }
        };
        let assignments = self.transaction.execute(
            "UPDATE assignments \
             SET state = ?3, summary = COALESCE(?4, summary), completed_at = ?5 \
             WHERE task_id = ?1 AND run_id = ?2 AND completed_at IS NULL",
            params![
                mutation.task_id,
                mutation.run_id,
                assignment_state,
                mutation.summary,
                mutation.transitioned_at,
            ],
        )?;
        let claims = self.transaction.execute(
            "UPDATE claims SET state = 'released', released_at = ?3 \
             WHERE task_id = ?1 AND run_id = ?2 AND released_at IS NULL",
            params![
                mutation.task_id,
                mutation.run_id,
                mutation.transitioned_at,
            ],
        )?;
        if assignments != 1 || claims != 1 {
            return Err(StoreError::CorruptTaskState {
                id: mutation.task_id,
                reason: format!(
                    "expected one active assignment and claim, found {assignments} and {claims}"
                ),
            });
        }
        Ok(())
    }

    pub(crate) fn insert_dependency(
        &self,
        dependency: &DependencyRecord,
    ) -> Result<(), StoreError> {
        self.transaction.execute(
            "INSERT INTO task_dependencies (\
                 run_id, task_id, dependency_task_id, created_at\
             ) VALUES (?1, ?2, ?3, ?4)",
            params![
                dependency.run_id,
                dependency.task_id,
                dependency.dependency_task_id,
                dependency.created_at,
            ],
        )?;
        Ok(())
    }

    pub(crate) fn dependency(
        &self,
        task_id: TaskId,
        dependency_task_id: TaskId,
    ) -> Result<Option<DependencyRecord>, StoreError> {
        Ok(self
            .transaction
            .query_row(
                "SELECT run_id, task_id, dependency_task_id, created_at \
                 FROM task_dependencies WHERE task_id = ?1 AND dependency_task_id = ?2",
                params![task_id, dependency_task_id],
                |row| {
                    Ok(DependencyRecord {
                        run_id: row.get(0)?,
                        task_id: row.get(1)?,
                        dependency_task_id: row.get(2)?,
                        created_at: row.get(3)?,
                    })
                },
            )
            .optional()?)
    }

    pub(crate) fn insert_comment(
        &self,
        comment: &CommentRecord,
    ) -> Result<(), StoreError> {
        self.transaction.execute(
            "INSERT INTO comments (id, run_id, task_id, author_agent_id, body, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                comment.id,
                comment.run_id,
                comment.task_id,
                comment.author_agent_id,
                comment.body,
                comment.created_at,
            ],
        )?;
        Ok(())
    }

    pub(crate) fn comment(
        &self,
        id: i64,
    ) -> Result<Option<CommentRecord>, StoreError> {
        Ok(self
            .transaction
            .query_row(
                "SELECT id, run_id, task_id, author_agent_id, body, created_at \
                 FROM comments WHERE id = ?1",
                [id],
                |row| {
                    Ok(CommentRecord {
                        id: row.get(0)?,
                        run_id: row.get(1)?,
                        task_id: row.get(2)?,
                        author_agent_id: row.get(3)?,
                        body: row.get(4)?,
                        created_at: row.get(5)?,
                    })
                },
            )
            .optional()?)
    }

    pub(crate) fn insert_operation(
        &self,
        operation: &OperationRecord,
    ) -> Result<(), StoreError> {
        let request = serde_json::to_string(&operation.request)?;
        let result = operation
            .result
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        self.transaction.execute(
            "INSERT INTO operations (\
                 id, run_id, kind, actor_agent_id, status, request_json, result_json, \
                 attempt_count, reconciliation_state, reconciliation_attempt_count, \
                 reconciliation_error, reconciled_at, created_at, updated_at\
             ) VALUES (\
                 ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14\
             )",
            params![
                operation.id,
                operation.run_id,
                operation.kind,
                operation.actor_agent_id,
                operation.status,
                request,
                result,
                operation.attempt_count,
                operation.reconciliation_state.map(ExternalResourceState::as_str),
                operation.reconciliation_attempt_count,
                operation.reconciliation_error,
                operation.reconciled_at,
                operation.created_at,
                operation.updated_at,
            ],
        )?;
        Ok(())
    }

    pub(crate) fn operation(
        &self,
        id: OperationId,
    ) -> Result<Option<OperationRecord>, StoreError> {
        Ok(self
            .transaction
            .query_row(
                "SELECT id, run_id, kind, actor_agent_id, status, request_json, result_json, \
                        attempt_count, reconciliation_state, reconciliation_attempt_count, \
                        reconciliation_error, reconciled_at, created_at, updated_at \
                 FROM operations WHERE id = ?1",
                [id],
                decode_operation,
            )
            .optional()?)
    }

    pub(crate) fn operations_requiring_reconciliation(
        &self,
        run_id: RunId,
    ) -> Result<Vec<OperationRecord>, StoreError> {
        let mut statement = self.transaction.prepare(
            "SELECT id, run_id, kind, actor_agent_id, status, request_json, result_json, \
                    attempt_count, reconciliation_state, reconciliation_attempt_count, \
                    reconciliation_error, reconciled_at, created_at, updated_at \
             FROM operations \
             WHERE run_id = ?1 AND (\
                 reconciliation_state = 'desired' \
                 OR (\
                     reconciliation_state = 'unknown' \
                     AND (\
                         kind IN ('agent.spawn', 'workspace.integrate') \
                         OR reconciled_at IS NULL\
                     )\
                 )\
             ) \
             ORDER BY created_at, id",
        )?;
        let rows = statement.query_map([run_id], decode_operation)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub(crate) fn foreground_launch_operation_for_session(
        &self,
        run_id: RunId,
        session_id: SessionId,
    ) -> Result<Option<OperationRecord>, StoreError> {
        Ok(self
            .transaction
            .query_row(
                "SELECT id, run_id, kind, actor_agent_id, status, request_json, \
                        result_json, attempt_count, reconciliation_state, \
                        reconciliation_attempt_count, reconciliation_error, \
                        reconciled_at, created_at, updated_at \
                 FROM operations \
                 WHERE run_id = ?1 \
                   AND kind = 'agent.launch_foreground' \
                   AND json_extract(result_json, '$.session_id') = ?2 \
                   AND json_extract(result_json, '$.already_active') = 0",
                params![run_id, session_id],
                decode_operation,
            )
            .optional()?)
    }

    pub(crate) fn mark_operation_reconciliation_desired(
        &self,
        operation_id: OperationId,
    ) -> Result<(), StoreError> {
        let updated = self.transaction.execute(
            "UPDATE operations SET reconciliation_state = 'desired', \
                 reconciliation_error = NULL, reconciled_at = NULL \
             WHERE id = ?1 AND reconciliation_state IS NULL",
            [operation_id],
        )?;
        if updated == 1 {
            let operation = self.operation(operation_id)?.ok_or(
                StoreError::OperationIncomplete {
                    id: operation_id,
                    status: "external reconciliation intent disappeared"
                        .to_owned(),
                },
            )?;
            self.append_operation_reconciliation_event(
                &operation,
                None,
                ExternalResourceState::Desired,
                None,
                operation.created_at,
            )?;
            Ok(())
        } else {
            let operation = self.operation(operation_id)?;
            if operation.as_ref().is_some_and(|operation| {
                operation.reconciliation_state
                    == Some(ExternalResourceState::Desired)
            }) {
                Ok(())
            } else {
                Err(StoreError::OperationIncomplete {
                    id: operation_id,
                    status: "missing external reconciliation intent".to_owned(),
                })
            }
        }
    }

    pub(crate) fn record_operation_reconciliation(
        &self,
        operation_id: OperationId,
        state: ExternalResourceState,
        error: Option<&str>,
        reconciled_at: i64,
    ) -> Result<ResourceTransitionOutcome, StoreError> {
        let redacted_error = error.map(crate::redaction::text);
        let error = redacted_error.as_deref();
        let Some(operation) = self.operation(operation_id)? else {
            return Ok(ResourceTransitionOutcome::Stale);
        };
        if operation.reconciliation_state.is_none() {
            return Ok(ResourceTransitionOutcome::Stale);
        }
        let outcome = if operation.reconciliation_state == Some(state)
            && operation.reconciliation_error.as_deref() == error
        {
            ResourceTransitionOutcome::Unchanged
        } else {
            ResourceTransitionOutcome::Applied
        };
        self.transaction.execute(
            "UPDATE operations \
             SET reconciliation_state = ?2, \
                 reconciliation_attempt_count = reconciliation_attempt_count + 1, \
                 reconciliation_error = ?3, reconciled_at = ?4 \
             WHERE id = ?1 AND reconciliation_state IS NOT NULL",
            params![operation_id, state.as_str(), error, reconciled_at],
        )?;
        if outcome == ResourceTransitionOutcome::Applied {
            self.append_operation_reconciliation_event(
                &operation,
                operation.reconciliation_state,
                state,
                error,
                reconciled_at,
            )?;
        }
        Ok(outcome)
    }

    fn append_operation_reconciliation_event(
        &self,
        operation: &OperationRecord,
        previous_state: Option<ExternalResourceState>,
        state: ExternalResourceState,
        error: Option<&str>,
        created_at: i64,
    ) -> Result<(), StoreError> {
        self.append_event(&NewEvent {
            run_id: operation.run_id,
            kind: EventKind::OperationReconciliationChanged,
            actor: "reconciler".to_owned(),
            subject: operation.id.to_string(),
            project_id: None,
            agent_id: operation.actor_agent_id,
            task_id: None,
            operation_id: Some(operation.id),
            correlation_id: None,
            causation_id: None,
            data: json!({
                "error": error,
                "previous_state": previous_state.map(ExternalResourceState::as_str),
                "state": state.as_str(),
            }),
            summary: format!(
                "Operation {} reconciliation changed to {}.",
                operation.id, state
            ),
            created_at,
        })?;
        Ok(())
    }

    pub(crate) fn insert_claim(
        &self,
        claim: &ClaimRecord,
    ) -> Result<(), StoreError> {
        self.transaction.execute(
            "INSERT INTO claims (\
                 id, run_id, task_id, agent_id, operation_id, state, claimed_at, released_at\
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                claim.id,
                claim.run_id,
                claim.task_id,
                claim.agent_id,
                claim.operation_id,
                claim.state,
                claim.claimed_at,
                claim.released_at,
            ],
        )?;
        Ok(())
    }

    pub(crate) fn claim(
        &self,
        id: i64,
    ) -> Result<Option<ClaimRecord>, StoreError> {
        Ok(self
            .transaction
            .query_row(
                "SELECT id, run_id, task_id, agent_id, operation_id, state, claimed_at, \
                        released_at \
                 FROM claims WHERE id = ?1",
                [id],
                |row| {
                    Ok(ClaimRecord {
                        id: row.get(0)?,
                        run_id: row.get(1)?,
                        task_id: row.get(2)?,
                        agent_id: row.get(3)?,
                        operation_id: row.get(4)?,
                        state: row.get(5)?,
                        claimed_at: row.get(6)?,
                        released_at: row.get(7)?,
                    })
                },
            )
            .optional()?)
    }

    pub(crate) fn compare_and_set_claim(
        &self,
        claim: &ClaimTaskMutation,
    ) -> Result<ClaimTaskResult, StoreError> {
        let generation = self
            .transaction
            .query_row(
                "SELECT generation FROM agents WHERE id = ?1 AND run_id = ?2",
                params![claim.agent_id, claim.run_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        let Some(generation) = generation else {
            return Ok(ClaimTaskResult::Rejected(
                ClaimRejection::AgentNotFound,
            ));
        };

        let changed = self.transaction.execute(
            "UPDATE tasks AS candidate \
             SET status = 'in_progress', updated_at = ?3 \
             WHERE candidate.id = ?1 AND candidate.run_id = ?2 \
               AND candidate.status = 'open' \
               AND NOT EXISTS ( \
                   SELECT 1 FROM claims AS active_claim \
                   WHERE active_claim.task_id = candidate.id \
                     AND active_claim.run_id = candidate.run_id \
                     AND active_claim.released_at IS NULL \
               ) \
               AND NOT EXISTS ( \
                   SELECT 1 \
                   FROM task_dependencies AS edge \
                   JOIN tasks AS dependency \
                     ON dependency.id = edge.dependency_task_id \
                    AND dependency.run_id = edge.run_id \
                   WHERE edge.task_id = candidate.id \
                     AND edge.run_id = candidate.run_id \
                     AND dependency.status <> 'closed' \
               ) \
               AND NOT EXISTS ( \
                   SELECT 1 FROM assignments AS active_assignment \
                   WHERE active_assignment.agent_id = ?4 \
                     AND active_assignment.run_id = candidate.run_id \
                     AND active_assignment.completed_at IS NULL \
               )",
            params![
                claim.task_id,
                claim.run_id,
                claim.claimed_at,
                claim.agent_id,
            ],
        )?;

        if changed == 0 {
            return Ok(ClaimTaskResult::Rejected(self.claim_rejection(claim)?));
        }

        self.transaction.execute(
            "INSERT INTO claims (\
                 run_id, task_id, agent_id, operation_id, state, claimed_at, released_at\
             ) VALUES (?1, ?2, ?3, ?4, 'active', ?5, NULL)",
            params![
                claim.run_id,
                claim.task_id,
                claim.agent_id,
                claim.operation_id,
                claim.claimed_at,
            ],
        )?;
        let claim_id = self.transaction.last_insert_rowid();

        self.insert_assignment(&AssignmentRecord {
            id: claim.assignment_id,
            run_id: claim.run_id,
            task_id: claim.task_id,
            agent_id: claim.agent_id,
            session_id: None,
            claim_id,
            generation,
            state: "active".to_owned(),
            summary: None,
            created_at: claim.claimed_at,
            completed_at: None,
        })?;

        Ok(ClaimTaskResult::Claimed {
            claim_id,
            assignment_id: claim.assignment_id,
        })
    }

    /// Emits the normalized state changes produced by a successful task claim.
    pub(crate) fn append_task_claim_events(
        &self,
        claim: &ClaimTaskMutation,
        result: &ClaimTaskResult,
    ) -> Result<(), StoreError> {
        let ClaimTaskResult::Claimed {
            claim_id,
            assignment_id,
        } = result
        else {
            return Ok(());
        };
        let task = self.task(claim.task_id)?.ok_or_else(|| {
            StoreError::CorruptTaskState {
                id: claim.task_id,
                reason: "the claimed task disappeared".to_owned(),
            }
        })?;
        let task_event = self.append_event(&NewEvent {
            run_id: claim.run_id,
            kind: EventKind::TaskLifecycleChanged,
            actor: mutation_actor(claim.actor_agent_id),
            subject: claim.task_id.to_string(),
            project_id: Some(task.project_id),
            agent_id: Some(claim.agent_id),
            task_id: Some(claim.task_id),
            operation_id: Some(claim.operation_id),
            correlation_id: None,
            causation_id: None,
            data: json!({
                "previous_status": TaskStatus::Open,
                "status": TaskStatus::InProgress,
                "transition": "claim",
            }),
            summary: format!(
                "Task {} changed from open to in_progress.",
                claim.task_id
            ),
            created_at: claim.claimed_at,
        })?;
        self.append_event(&NewEvent {
            run_id: claim.run_id,
            kind: EventKind::TaskClaimed,
            actor: mutation_actor(claim.actor_agent_id),
            subject: claim.task_id.to_string(),
            project_id: Some(task.project_id),
            agent_id: Some(claim.agent_id),
            task_id: Some(claim.task_id),
            operation_id: Some(claim.operation_id),
            correlation_id: Some(task_event.id),
            causation_id: Some(task_event.id),
            data: json!({
                "assignment_id": assignment_id,
                "claim_id": claim_id,
            }),
            summary: format!(
                "Agent {} claimed task {}.",
                claim.agent_id, claim.task_id
            ),
            created_at: claim.claimed_at,
        })?;
        self.append_event(&NewEvent {
            run_id: claim.run_id,
            kind: EventKind::AssignmentCreated,
            actor: mutation_actor(claim.actor_agent_id),
            subject: assignment_id.to_string(),
            project_id: Some(task.project_id),
            agent_id: Some(claim.agent_id),
            task_id: Some(claim.task_id),
            operation_id: Some(claim.operation_id),
            correlation_id: Some(task_event.id),
            causation_id: Some(task_event.id),
            data: json!({
                "claim_id": claim_id,
                "generation": self.agent(claim.agent_id)?.map(|agent| agent.generation),
                "state": "active",
            }),
            summary: format!("Created assignment {assignment_id}."),
            created_at: claim.claimed_at,
        })?;
        Ok(())
    }

    fn active_claim_for_task(
        &self,
        run_id: RunId,
        task_id: TaskId,
    ) -> Result<Option<ClaimRecord>, StoreError> {
        Ok(self
            .transaction
            .query_row(
                "SELECT id, run_id, task_id, agent_id, operation_id, state, claimed_at, \
                        released_at FROM claims \
                 WHERE run_id = ?1 AND task_id = ?2 AND released_at IS NULL",
                params![run_id, task_id],
                |row| {
                    Ok(ClaimRecord {
                        id: row.get(0)?,
                        run_id: row.get(1)?,
                        task_id: row.get(2)?,
                        agent_id: row.get(3)?,
                        operation_id: row.get(4)?,
                        state: row.get(5)?,
                        claimed_at: row.get(6)?,
                        released_at: row.get(7)?,
                    })
                },
            )
            .optional()?)
    }

    fn claim_rejection(
        &self,
        claim: &ClaimTaskMutation,
    ) -> Result<ClaimRejection, StoreError> {
        let already_claimed = self.transaction.query_row(
            "SELECT EXISTS (\
                 SELECT 1 FROM claims \
                 WHERE task_id = ?1 AND run_id = ?2 AND released_at IS NULL\
             )",
            params![claim.task_id, claim.run_id],
            |row| row.get::<_, bool>(0),
        )?;
        if already_claimed {
            return Ok(ClaimRejection::AlreadyClaimed);
        }

        let status = self
            .transaction
            .query_row(
                "SELECT status FROM tasks WHERE id = ?1 AND run_id = ?2",
                params![claim.task_id, claim.run_id],
                |row| row.get::<_, TaskStatus>(0),
            )
            .optional()?;
        let Some(status) = status else {
            return Ok(ClaimRejection::TaskNotFound);
        };
        if status != TaskStatus::Open {
            return Ok(ClaimRejection::TaskNotOpen);
        }

        let blocked = self.transaction.query_row(
            "SELECT EXISTS (\
                 SELECT 1 \
                 FROM task_dependencies AS edge \
                 JOIN tasks AS dependency \
                   ON dependency.id = edge.dependency_task_id \
                  AND dependency.run_id = edge.run_id \
                 WHERE edge.task_id = ?1 AND edge.run_id = ?2 \
                   AND dependency.status <> 'closed'\
             )",
            params![claim.task_id, claim.run_id],
            |row| row.get::<_, bool>(0),
        )?;
        if blocked {
            return Ok(ClaimRejection::Blocked);
        }

        let agent_busy = self.transaction.query_row(
            "SELECT EXISTS (\
                 SELECT 1 FROM assignments \
                 WHERE agent_id = ?1 AND run_id = ?2 AND completed_at IS NULL\
             )",
            params![claim.agent_id, claim.run_id],
            |row| row.get::<_, bool>(0),
        )?;
        if agent_busy {
            return Ok(ClaimRejection::AgentBusy);
        }

        Ok(ClaimRejection::TaskNotOpen)
    }

    pub(crate) fn insert_assignment(
        &self,
        assignment: &AssignmentRecord,
    ) -> Result<(), StoreError> {
        let agent = self.agent(assignment.agent_id)?;
        if !agent.is_some_and(|agent| {
            agent.run_id == assignment.run_id
                && agent.generation == assignment.generation
        }) {
            return Err(StoreError::StaleAssignment { id: assignment.id });
        }
        if let Some(session_id) = assignment.session_id {
            let session = self.session(session_id)?;
            if !session.is_some_and(|session| {
                session.run_id == assignment.run_id
                    && session.agent_id == assignment.agent_id
                    && session.generation == assignment.generation
            }) {
                return Err(StoreError::StaleAssignment { id: assignment.id });
            }
        }
        self.transaction.execute(
            "INSERT INTO assignments (\
                 id, run_id, task_id, agent_id, session_id, claim_id, generation, state, \
                 summary, created_at, completed_at\
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                assignment.id,
                assignment.run_id,
                assignment.task_id,
                assignment.agent_id,
                assignment.session_id,
                assignment.claim_id,
                assignment.generation,
                assignment.state,
                assignment.summary,
                assignment.created_at,
                assignment.completed_at,
            ],
        )?;
        Ok(())
    }

    pub(crate) fn assignment(
        &self,
        id: AssignmentId,
    ) -> Result<Option<AssignmentRecord>, StoreError> {
        Ok(self
            .transaction
            .query_row(
                "SELECT id, run_id, task_id, agent_id, session_id, claim_id, generation, \
                        state, summary, created_at, completed_at \
                 FROM assignments WHERE id = ?1",
                [id],
                |row| {
                    Ok(AssignmentRecord {
                        id: row.get(0)?,
                        run_id: row.get(1)?,
                        task_id: row.get(2)?,
                        agent_id: row.get(3)?,
                        session_id: row.get(4)?,
                        claim_id: row.get(5)?,
                        generation: row.get(6)?,
                        state: row.get(7)?,
                        summary: row.get(8)?,
                        created_at: row.get(9)?,
                        completed_at: row.get(10)?,
                    })
                },
            )
            .optional()?)
    }

    pub(crate) fn active_assignment_for_agent(
        &self,
        run_id: RunId,
        agent_id: AgentId,
    ) -> Result<Option<AssignmentRecord>, StoreError> {
        Ok(self
            .transaction
            .query_row(
                "SELECT id, run_id, task_id, agent_id, session_id, claim_id, generation, \
                        state, summary, created_at, completed_at \
                 FROM assignments WHERE run_id = ?1 AND agent_id = ?2 \
                   AND completed_at IS NULL",
                params![run_id, agent_id],
                |row| {
                    Ok(AssignmentRecord {
                        id: row.get(0)?,
                        run_id: row.get(1)?,
                        task_id: row.get(2)?,
                        agent_id: row.get(3)?,
                        session_id: row.get(4)?,
                        claim_id: row.get(5)?,
                        generation: row.get(6)?,
                        state: row.get(7)?,
                        summary: row.get(8)?,
                        created_at: row.get(9)?,
                        completed_at: row.get(10)?,
                    })
                },
            )
            .optional()?)
    }

    fn active_assignment_for_task(
        &self,
        run_id: RunId,
        task_id: TaskId,
    ) -> Result<Option<AssignmentRecord>, StoreError> {
        Ok(self
            .transaction
            .query_row(
                "SELECT id, run_id, task_id, agent_id, session_id, claim_id, generation, \
                        state, summary, created_at, completed_at FROM assignments \
                 WHERE run_id = ?1 AND task_id = ?2 AND completed_at IS NULL",
                params![run_id, task_id],
                |row| {
                    Ok(AssignmentRecord {
                        id: row.get(0)?,
                        run_id: row.get(1)?,
                        task_id: row.get(2)?,
                        agent_id: row.get(3)?,
                        session_id: row.get(4)?,
                        claim_id: row.get(5)?,
                        generation: row.get(6)?,
                        state: row.get(7)?,
                        summary: row.get(8)?,
                        created_at: row.get(9)?,
                        completed_at: row.get(10)?,
                    })
                },
            )
            .optional()?)
    }

    pub(crate) fn latest_assignment_for_task(
        &self,
        run_id: RunId,
        task_id: TaskId,
    ) -> Result<Option<AssignmentRecord>, StoreError> {
        Ok(self
            .transaction
            .query_row(
                "SELECT id, run_id, task_id, agent_id, session_id, claim_id, generation, \
                        state, summary, created_at, completed_at FROM assignments \
                 WHERE run_id = ?1 AND task_id = ?2 \
                 ORDER BY created_at DESC, rowid DESC LIMIT 1",
                params![run_id, task_id],
                |row| {
                    Ok(AssignmentRecord {
                        id: row.get(0)?,
                        run_id: row.get(1)?,
                        task_id: row.get(2)?,
                        agent_id: row.get(3)?,
                        session_id: row.get(4)?,
                        claim_id: row.get(5)?,
                        generation: row.get(6)?,
                        state: row.get(7)?,
                        summary: row.get(8)?,
                        created_at: row.get(9)?,
                        completed_at: row.get(10)?,
                    })
                },
            )
            .optional()?)
    }

    pub(crate) fn assignment_for_agent_task(
        &self,
        run_id: RunId,
        agent_id: AgentId,
        task_id: TaskId,
    ) -> Result<Option<AssignmentRecord>, StoreError> {
        Ok(self
            .transaction
            .query_row(
                "SELECT id, run_id, task_id, agent_id, session_id, claim_id, generation, \
                        state, summary, created_at, completed_at \
                 FROM assignments WHERE run_id = ?1 AND agent_id = ?2 AND task_id = ?3 \
                 ORDER BY created_at DESC, id DESC LIMIT 1",
                params![run_id, agent_id, task_id],
                |row| {
                    Ok(AssignmentRecord {
                        id: row.get(0)?,
                        run_id: row.get(1)?,
                        task_id: row.get(2)?,
                        agent_id: row.get(3)?,
                        session_id: row.get(4)?,
                        claim_id: row.get(5)?,
                        generation: row.get(6)?,
                        state: row.get(7)?,
                        summary: row.get(8)?,
                        created_at: row.get(9)?,
                        completed_at: row.get(10)?,
                    })
                },
            )
            .optional()?)
    }

    pub(crate) fn associate_assignment_session(
        &self,
        assignment_id: AssignmentId,
        session_id: SessionId,
    ) -> Result<(), StoreError> {
        let assignment = self.assignment(assignment_id)?;
        let session = self.session(session_id)?;
        let valid = match (&assignment, &session) {
            (Some(assignment), Some(session)) => {
                assignment.run_id == session.run_id
                    && assignment.agent_id == session.agent_id
                    && assignment.generation == session.generation
                    && !session.state.is_terminal()
                    && self.session_scope_is_current(
                        crate::auth::SessionScope {
                            run_id: session.run_id,
                            agent_id: session.agent_id,
                            session_id,
                            generation: session.generation,
                        },
                    )?
            }
            _ => false,
        };
        if !valid {
            return Err(StoreError::CorruptAssignmentState {
                id: assignment_id,
                reason: format!(
                    "session `{session_id}` does not own this generation"
                ),
            });
        }
        let changed = self.transaction.execute(
            "UPDATE assignments SET session_id = ?2 \
             WHERE id = ?1 AND session_id IS NULL AND completed_at IS NULL",
            params![assignment_id, session_id],
        )?;
        if changed == 1 {
            Ok(())
        } else {
            Err(StoreError::CorruptAssignmentState {
                id: assignment_id,
                reason: format!("cannot accept session `{session_id}`"),
            })
        }
    }

    pub(crate) fn insert_message(
        &self,
        message: &MessageRecord,
    ) -> Result<(), StoreError> {
        self.transaction.execute(
            "INSERT INTO messages (\
                 id, run_id, sender_agent_id, recipient_agent_id, sequence, body, created_at, \
                 acknowledged_at\
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                message.id,
                message.run_id,
                message.sender_agent_id,
                message.recipient_agent_id,
                message.sequence,
                message.body,
                message.created_at,
                message.acknowledged_at,
            ],
        )?;
        Ok(())
    }

    pub(crate) fn message(
        &self,
        id: MessageId,
    ) -> Result<Option<MessageRecord>, StoreError> {
        Ok(self
            .transaction
            .query_row(
                "SELECT id, run_id, sender_agent_id, recipient_agent_id, sequence, body, \
                        created_at, acknowledged_at \
                 FROM messages WHERE id = ?1",
                [id],
                |row| {
                    Ok(MessageRecord {
                        id: row.get(0)?,
                        run_id: row.get(1)?,
                        sender_agent_id: row.get(2)?,
                        recipient_agent_id: row.get(3)?,
                        sequence: row.get(4)?,
                        body: row.get(5)?,
                        created_at: row.get(6)?,
                        acknowledged_at: row.get(7)?,
                    })
                },
            )
            .optional()?)
    }

    pub(crate) fn next_message_sequence(
        &self,
        run_id: RunId,
        recipient_agent_id: AgentId,
    ) -> Result<i64, StoreError> {
        Ok(self.transaction.query_row(
            "SELECT COALESCE(MAX(sequence), 0) + 1 FROM messages \
             WHERE run_id = ?1 AND recipient_agent_id = ?2",
            params![run_id, recipient_agent_id],
            |row| row.get(0),
        )?)
    }

    pub(crate) fn messages_after(
        &self,
        run_id: RunId,
        recipient_agent_id: AgentId,
        after: i64,
    ) -> Result<Vec<MessageRecord>, StoreError> {
        let mut statement = self.transaction.prepare(
            "SELECT id, run_id, sender_agent_id, recipient_agent_id, sequence, body, \
                    created_at, acknowledged_at \
             FROM messages WHERE run_id = ?1 AND recipient_agent_id = ?2 \
               AND sequence > ?3 ORDER BY sequence",
        )?;
        let rows = statement.query_map(
            params![run_id, recipient_agent_id, after],
            |row| {
                Ok(MessageRecord {
                    id: row.get(0)?,
                    run_id: row.get(1)?,
                    sender_agent_id: row.get(2)?,
                    recipient_agent_id: row.get(3)?,
                    sequence: row.get(4)?,
                    body: row.get(5)?,
                    created_at: row.get(6)?,
                    acknowledged_at: row.get(7)?,
                })
            },
        )?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    fn apply_message_acknowledgement(
        &self,
        acknowledgement: &AcknowledgeMessagesMutation,
    ) -> Result<AcknowledgeMessagesResult, StoreError> {
        let highest_cursor = self.transaction.query_row(
            "SELECT COALESCE(MAX(sequence), 0) FROM messages \
             WHERE run_id = ?1 AND recipient_agent_id = ?2",
            params![acknowledgement.run_id, acknowledgement.agent_id],
            |row| row.get::<_, i64>(0),
        )?;
        if acknowledgement.through <= 0
            || acknowledgement.through > highest_cursor
        {
            return Ok(AcknowledgeMessagesResult::CursorNotFound {
                highest_cursor,
            });
        }
        let acknowledged_through = self.transaction.query_row(
            "SELECT COALESCE(MAX(sequence), 0) FROM messages \
             WHERE run_id = ?1 AND recipient_agent_id = ?2 \
               AND acknowledged_at IS NOT NULL",
            params![acknowledgement.run_id, acknowledgement.agent_id],
            |row| row.get::<_, i64>(0),
        )?;
        let acknowledged_count = self.transaction.execute(
            "UPDATE messages SET acknowledged_at = ?4 \
             WHERE run_id = ?1 AND recipient_agent_id = ?2 \
               AND sequence <= ?3 AND acknowledged_at IS NULL",
            params![
                acknowledgement.run_id,
                acknowledgement.agent_id,
                acknowledgement.through,
                acknowledgement.acknowledged_at,
            ],
        )?;
        Ok(AcknowledgeMessagesResult::Acknowledged {
            acknowledged_through: acknowledged_through
                .max(acknowledgement.through),
            acknowledged_count,
        })
    }

    pub(crate) fn insert_workspace(
        &self,
        workspace: &WorkspaceRecord,
    ) -> Result<(), StoreError> {
        self.transaction.execute(
            "INSERT INTO workspaces (\
                 assignment_id, run_id, project_id, kind, path, state, base_commit, \
                 result_commit, target_commit, created_at, reconciled_at, generation\
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                workspace.assignment_id,
                workspace.run_id,
                workspace.project_id,
                workspace.kind,
                path_bytes(&workspace.path),
                workspace.state.as_str(),
                workspace.base_commit,
                workspace.result_commit,
                workspace.target_commit,
                workspace.created_at,
                workspace.reconciled_at,
                workspace.generation,
            ],
        )?;
        Ok(())
    }

    pub(crate) fn workspace(
        &self,
        assignment_id: AssignmentId,
    ) -> Result<Option<WorkspaceRecord>, StoreError> {
        Ok(self
            .transaction
            .query_row(
                "SELECT assignment_id, run_id, project_id, kind, path, state, base_commit, \
                        result_commit, target_commit, created_at, reconciled_at, generation \
                 FROM workspaces WHERE assignment_id = ?1",
                [assignment_id],
                |row| {
                    Ok(WorkspaceRecord {
                        generation: row.get(11)?,
                        assignment_id: row.get(0)?,
                        run_id: row.get(1)?,
                        project_id: row.get(2)?,
                        kind: row.get(3)?,
                        path: decode_path(row, 4)?,
                        state: decode_external_resource_state(row, 5)?,
                        base_commit: row.get(6)?,
                        result_commit: row.get(7)?,
                        target_commit: row.get(8)?,
                        created_at: row.get(9)?,
                        reconciled_at: row.get(10)?,
                    })
                },
            )
            .optional()?)
    }

    pub(crate) fn workspaces(
        &self,
        run_id: RunId,
    ) -> Result<Vec<WorkspaceRecord>, StoreError> {
        let mut statement = self.transaction.prepare(
            "SELECT assignment_id, run_id, project_id, kind, path, state, base_commit, \
                    result_commit, target_commit, created_at, reconciled_at, generation \
             FROM workspaces WHERE run_id = ?1 ORDER BY created_at, rowid",
        )?;
        let rows = statement.query_map([run_id], |row| {
            Ok(WorkspaceRecord {
                generation: row.get(11)?,
                assignment_id: row.get(0)?,
                run_id: row.get(1)?,
                project_id: row.get(2)?,
                kind: row.get(3)?,
                path: decode_path(row, 4)?,
                state: decode_external_resource_state(row, 5)?,
                base_commit: row.get(6)?,
                result_commit: row.get(7)?,
                target_commit: row.get(8)?,
                created_at: row.get(9)?,
                reconciled_at: row.get(10)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub(crate) fn assignment_scope_is_current(
        &self,
        scope: AssignmentScope,
    ) -> Result<bool, StoreError> {
        Ok(self.transaction.query_row(
            "SELECT EXISTS (SELECT 1 FROM assignments AS assignment \
             JOIN agents AS agent ON agent.id = assignment.agent_id AND agent.run_id = assignment.run_id \
             JOIN runs AS run ON run.id = assignment.run_id \
             WHERE assignment.id = ?1 AND assignment.run_id = ?2 AND assignment.generation = ?3 \
               AND agent.generation = ?3 AND run.status = 'active' \
               AND (assignment.session_id IS NULL OR EXISTS (SELECT 1 FROM sessions \
                    WHERE sessions.id = assignment.session_id AND sessions.run_id = assignment.run_id \
                      AND sessions.agent_id = assignment.agent_id AND sessions.generation = assignment.generation)))",
            params![scope.assignment_id, scope.run_id, scope.generation],
            |row| row.get(0),
        )?)
    }

    /// Loads a workspace only while its immutable ownership remains current.
    pub(crate) fn workspace_for_scope(
        &self,
        scope: AssignmentScope,
    ) -> Result<WorkspaceRecord, StoreError> {
        let workspace = self.workspace(scope.assignment_id)?;
        if self.assignment_scope_is_current(scope)?
            && let Some(workspace) = workspace
            && workspace.scope() == scope
        {
            return Ok(workspace);
        }
        Err(StoreError::StaleAssignment {
            id: scope.assignment_id,
        })
    }

    pub(crate) fn record_workspace_reconciliation_state(
        &self,
        scope: AssignmentScope,
        state: ExternalResourceState,
        reconciled_at: i64,
    ) -> Result<ResourceTransitionOutcome, StoreError> {
        let assignment_id = scope.assignment_id;
        let workspace = match self.workspace_for_scope(scope) {
            Ok(workspace) => workspace,
            Err(StoreError::StaleAssignment { .. }) => {
                return Ok(ResourceTransitionOutcome::Stale);
            }
            Err(error) => return Err(error),
        };
        if workspace.state == state {
            return Ok(ResourceTransitionOutcome::Unchanged);
        }
        self.transaction.execute(
            "UPDATE workspaces SET state = ?2, reconciled_at = ?3 \
             WHERE assignment_id = ?1",
            params![assignment_id, state.as_str(), reconciled_at],
        )?;
        Ok(ResourceTransitionOutcome::Applied)
    }

    pub(crate) fn record_workspace_result_commit(
        &self,
        scope: AssignmentScope,
        result_commit: &str,
    ) -> Result<ResourceTransitionOutcome, StoreError> {
        let assignment_id = scope.assignment_id;
        let workspace = match self.workspace_for_scope(scope) {
            Ok(workspace) => workspace,
            Err(StoreError::StaleAssignment { .. }) => {
                return Ok(ResourceTransitionOutcome::Stale);
            }
            Err(error) => return Err(error),
        };
        match workspace.result_commit {
            Some(recorded) if recorded == result_commit => {
                Ok(ResourceTransitionOutcome::Unchanged)
            }
            Some(recorded) => Err(StoreError::WorkspaceResultConflict {
                assignment_id,
                recorded,
                observed: result_commit.to_owned(),
            }),
            None => {
                self.transaction.execute(
                    "UPDATE workspaces SET result_commit = ?2 WHERE assignment_id = ?1",
                    params![assignment_id, result_commit],
                )?;
                Ok(ResourceTransitionOutcome::Applied)
            }
        }
    }

    pub(crate) fn record_workspace_target_commit(
        &self,
        scope: AssignmentScope,
        target_commit: &str,
    ) -> Result<ResourceTransitionOutcome, StoreError> {
        let assignment_id = scope.assignment_id;
        let workspace = match self.workspace_for_scope(scope) {
            Ok(workspace) => workspace,
            Err(StoreError::StaleAssignment { .. }) => {
                return Ok(ResourceTransitionOutcome::Stale);
            }
            Err(error) => return Err(error),
        };
        match workspace.target_commit {
            Some(recorded) if recorded == target_commit => {
                Ok(ResourceTransitionOutcome::Unchanged)
            }
            Some(recorded) => Err(StoreError::WorkspaceTargetConflict {
                assignment_id,
                recorded,
                observed: target_commit.to_owned(),
            }),
            None => {
                self.transaction.execute(
                    "UPDATE workspaces SET target_commit = ?2 WHERE assignment_id = ?1",
                    params![assignment_id, target_commit],
                )?;
                Ok(ResourceTransitionOutcome::Applied)
            }
        }
    }

    pub(crate) fn insert_event(
        &self,
        event: &EventRecord,
    ) -> Result<(), StoreError> {
        if serde_json::to_vec(event)?.len() > MAXIMUM_EVENT_PAGE_LENGTH {
            return Err(StoreError::EventTooLarge { id: event.id });
        }
        let payload = serde_json::to_string(&event.payload)?;
        self.transaction.execute(
            "INSERT INTO events (\
                 id, run_id, sequence, event_type, actor, subject, project_id, agent_id, \
                 task_id, operation_id, correlation_id, causation_id, payload_json, summary, \
                 created_at\
             ) VALUES (\
                 ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15\
             )",
            params![
                event.id,
                event.run_id,
                event.sequence,
                event.event_type,
                event.actor,
                event.subject,
                event.project_id,
                event.agent_id,
                event.task_id,
                event.operation_id,
                event.correlation_id,
                event.causation_id,
                payload,
                event.summary,
                event.created_at,
            ],
        )?;
        Ok(())
    }

    /// Appends one normalized, versioned event at the next run-local sequence.
    pub(crate) fn append_event(
        &self,
        event: &NewEvent,
    ) -> Result<EventRecord, StoreError> {
        let sequence = self.transaction.query_row(
            "SELECT COALESCE(MAX(sequence), 0) + 1 FROM events WHERE run_id = ?1",
            [event.run_id],
            |row| row.get(0),
        )?;
        let event = EventRecord {
            id: EventId::generate(),
            run_id: event.run_id,
            sequence,
            event_type: event.kind.as_str().to_owned(),
            actor: event.actor.clone(),
            subject: event.subject.clone(),
            project_id: event.project_id,
            agent_id: event.agent_id,
            task_id: event.task_id,
            operation_id: event.operation_id,
            correlation_id: event.correlation_id,
            causation_id: event.causation_id,
            payload: json!({
                "schema_version": 1,
                "data": event.data,
            }),
            summary: event.summary.clone(),
            created_at: event.created_at,
        };
        self.insert_event(&event)?;
        Ok(event)
    }

    pub(crate) fn event(
        &self,
        id: EventId,
    ) -> Result<Option<EventRecord>, StoreError> {
        Ok(self
            .transaction
            .query_row(
                "SELECT id, run_id, sequence, event_type, actor, subject, project_id, agent_id, \
                        task_id, operation_id, correlation_id, causation_id, payload_json, \
                        summary, created_at \
                 FROM events WHERE id = ?1",
                [id],
                |row| {
                    Ok(EventRecord {
                        id: row.get(0)?,
                        run_id: row.get(1)?,
                        sequence: row.get(2)?,
                        event_type: row.get(3)?,
                        actor: row.get(4)?,
                        subject: row.get(5)?,
                        project_id: row.get(6)?,
                        agent_id: row.get(7)?,
                        task_id: row.get(8)?,
                        operation_id: row.get(9)?,
                        correlation_id: row.get(10)?,
                        causation_id: row.get(11)?,
                        payload: decode_json(row, 12)?,
                        summary: row.get(13)?,
                        created_at: row.get(14)?,
                    })
                },
            )
            .optional()?)
    }

    pub(crate) fn events_after(
        &self,
        run_id: RunId,
        after: i64,
        limit: u16,
    ) -> Result<Vec<EventRecord>, StoreError> {
        let mut statement = self.transaction.prepare(
            "SELECT id, run_id, sequence, event_type, actor, subject, project_id, agent_id, \
                    task_id, operation_id, correlation_id, causation_id, payload_json, \
                    summary, created_at \
             FROM events WHERE run_id = ?1 AND sequence > ?2 \
             ORDER BY sequence LIMIT ?3",
        )?;
        let rows =
            statement.query_map(params![run_id, after, limit], |row| {
                Ok(EventRecord {
                    id: row.get(0)?,
                    run_id: row.get(1)?,
                    sequence: row.get(2)?,
                    event_type: row.get(3)?,
                    actor: row.get(4)?,
                    subject: row.get(5)?,
                    project_id: row.get(6)?,
                    agent_id: row.get(7)?,
                    task_id: row.get(8)?,
                    operation_id: row.get(9)?,
                    correlation_id: row.get(10)?,
                    causation_id: row.get(11)?,
                    payload: decode_json(row, 12)?,
                    summary: row.get(13)?,
                    created_at: row.get(14)?,
                })
            })?;
        let mut events = Vec::new();
        let mut length = 0;
        for event in rows {
            let event = event?;
            let size = serde_json::to_vec(&event)?.len();
            // Older writers accepted larger records. Return those alone so
            // readers can advance without discarding an immutable event.
            if !events.is_empty() && length + size > MAXIMUM_EVENT_PAGE_LENGTH {
                break;
            }
            length += size;
            events.push(event);
        }
        Ok(events)
    }
}

fn mutation_actor(agent_id: Option<AgentId>) -> String {
    agent_id.map_or_else(|| "operator".to_owned(), |id| id.to_string())
}

fn path_bytes(path: &Path) -> &[u8] {
    path.as_os_str().as_bytes()
}

fn decode_path(
    row: &rusqlite::Row<'_>,
    index: usize,
) -> rusqlite::Result<PathBuf> {
    let bytes = row.get::<_, Vec<u8>>(index)?;
    Ok(PathBuf::from(OsString::from_vec(bytes)))
}

fn decode_session_credential(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<SessionCredentialRecord> {
    let verifier = row.get::<_, Vec<u8>>(4)?;
    let verifier: [u8; 32] =
        verifier.try_into().map_err(|bytes: Vec<u8>| {
            rusqlite::Error::FromSqlConversionFailure(
                4,
                Type::Blob,
                Box::new(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "credential verifier has {} bytes, expected 32",
                        bytes.len()
                    ),
                )),
            )
        })?;
    Ok(SessionCredentialRecord {
        session_id: row.get(0)?,
        run_id: row.get(1)?,
        agent_id: row.get(2)?,
        generation: row.get(3)?,
        token_verifier: TokenVerifier::from_bytes(verifier),
        created_at: row.get(5)?,
        revoked_at: row.get(6)?,
    })
}

fn decode_json<T>(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<T>
where
    T: DeserializeOwned,
{
    let encoded = row.get::<_, String>(index)?;
    serde_json::from_str(&encoded).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            Type::Text,
            Box::new(error),
        )
    })
}

fn decode_optional_json(
    row: &rusqlite::Row<'_>,
    index: usize,
) -> rusqlite::Result<Option<JsonValue>> {
    row.get::<_, Option<String>>(index)?
        .map(|encoded| {
            serde_json::from_str(&encoded).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    index,
                    Type::Text,
                    Box::new(error),
                )
            })
        })
        .transpose()
}

fn decode_operation(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<OperationRecord> {
    Ok(OperationRecord {
        id: row.get(0)?,
        run_id: row.get(1)?,
        kind: row.get(2)?,
        actor_agent_id: row.get(3)?,
        status: row.get(4)?,
        request: decode_json(row, 5)?,
        result: decode_optional_json(row, 6)?,
        attempt_count: row.get(7)?,
        reconciliation_state: decode_optional_external_resource_state(row, 8)?,
        reconciliation_attempt_count: row.get(9)?,
        reconciliation_error: row.get(10)?,
        reconciled_at: row.get(11)?,
        created_at: row.get(12)?,
        updated_at: row.get(13)?,
    })
}

fn decode_lifecycle(
    row: &rusqlite::Row<'_>,
    index: usize,
) -> rusqlite::Result<LifecycleState> {
    let encoded = row.get::<_, String>(index)?;
    encoded.parse().map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            Type::Text,
            Box::new(error),
        )
    })
}

fn decode_external_resource_state(
    row: &rusqlite::Row<'_>,
    index: usize,
) -> rusqlite::Result<ExternalResourceState> {
    let encoded = row.get::<_, String>(index)?;
    encoded.parse().map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            Type::Text,
            Box::new(error),
        )
    })
}

fn decode_optional_external_resource_state(
    row: &rusqlite::Row<'_>,
    index: usize,
) -> rusqlite::Result<Option<ExternalResourceState>> {
    row.get::<_, Option<String>>(index)?
        .map(|encoded| {
            encoded.parse().map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    index,
                    Type::Text,
                    Box::new(error),
                )
            })
        })
        .transpose()
}

fn decode_session_process_owner(
    row: &rusqlite::Row<'_>,
    index: usize,
) -> rusqlite::Result<SessionProcessOwner> {
    let encoded = row.get::<_, String>(index)?;
    encoded.parse().map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            Type::Text,
            Box::new(error),
        )
    })
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::collections::BTreeSet;
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;
    use std::path::PathBuf;

    use rusqlite::{Connection, ErrorCode};
    use serde_json::json;

    use super::{
        AcknowledgeMessagesMutation, AcknowledgeMessagesResult, AgentRecord,
        AssignmentRecord, BUSY_TIMEOUT, ClaimRecord, ClaimRejection,
        ClaimTaskMutation, ClaimTaskResult, CommentRecord,
        ConfigurationSnapshotRecord, DependencyRecord, EventRecord,
        ExternalResourceState, MIGRATIONS, MessageRecord, Mutation,
        MutationOutcome, OperationRecord, ProjectRecord,
        ResourceTransitionOutcome, RunRecord, SessionCredentialRecord,
        SessionProcessOwner, SessionRecord, SessionTransitionOutcome, Store,
        TaskGroupRecord, TaskRecord, TaskTransitionMutation,
        TaskTransitionRejection, TaskTransitionResult, WorkspaceRecord,
    };
    use crate::auth::{AgentToken, SessionScope};
    use crate::id::{
        AgentId, AssignmentId, EventId, MessageId, OperationId, ProjectId,
        RunId, SessionId, TaskId,
    };
    use crate::project::ProjectIdentity;
    use crate::providers::LifecycleState;
    use crate::tasks::{TaskStatus, TaskTransition};

    const RUN_ID: &str = "cr-01ARZ3NDEKTSV4RRFFQ69G5FAV";
    const PROJECT_ID: &str = "cp-01ARZ3NDEKTSV4RRFFQ69G5FAW";
    const AGENT_ID: &str = "cg-01ARZ3NDEKTSV4RRFFQ69G5FAX";
    const SESSION_ID: &str = "cs-01ARZ3NDEKTSV4RRFFQ69G5FAY";
    const TASK_ID: &str = "ct-01ARZ3NDEKTSV4RRFFQ69G5FAZ";
    const DEPENDENCY_TASK_ID: &str = "ct-01ARZ3NDEKTSV4RRFFQ69G5FB0";
    const ASSIGNMENT_ID: &str = "ca-01ARZ3NDEKTSV4RRFFQ69G5FB1";
    const MESSAGE_ID: &str = "cm-01ARZ3NDEKTSV4RRFFQ69G5FB2";
    const OPERATION_ID: &str = "co-01ARZ3NDEKTSV4RRFFQ69G5FB3";
    const EVENT_ID: &str = "ce-01ARZ3NDEKTSV4RRFFQ69G5FB4";
    const SECOND_ASSIGNMENT_ID: &str = "ca-01ARZ3NDEKTSV4RRFFQ69G5FB5";
    const SECOND_OPERATION_ID: &str = "co-01ARZ3NDEKTSV4RRFFQ69G5FB6";
    const SECOND_TASK_ID: &str = "ct-01ARZ3NDEKTSV4RRFFQ69G5FB7";
    const SECOND_SESSION_ID: &str = "cs-01ARZ3NDEKTSV4RRFFQ69G5FB8";
    const TOKEN: &str =
        "cot1_000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";

    #[test]
    fn a_new_store_applies_the_complete_initial_schema() {
        let store = Store::open_in_memory().expect("the store should open");

        let actual = store.table_names().expect("tables should be inspectable");
        let expected = BTreeSet::from([
            "agents",
            "assignments",
            "claims",
            "comments",
            "configuration_snapshots",
            "events",
            "messages",
            "operations",
            "projects",
            "runs",
            "run_shutdowns",
            "session_controls",
            "session_launch_attempts",
            "session_failures",
            "schema_migrations",
            "session_credentials",
            "sessions",
            "task_dependencies",
            "task_groups",
            "tasks",
            "workspaces",
        ])
        .into_iter()
        .map(str::to_owned)
        .collect();

        assert_eq!(actual, expected);
    }

    #[test]
    fn opening_a_store_enables_the_required_connection_policy() {
        let database = TestDatabase::new();
        let store = Store::open(&database.0).expect("the store should open");

        let foreign_keys = store
            .connection
            .pragma_query_value(None, "foreign_keys", |row| {
                row.get::<_, i64>(0)
            })
            .expect("the foreign-key setting should be readable");
        let busy_timeout = store
            .connection
            .pragma_query_value(None, "busy_timeout", |row| {
                row.get::<_, i64>(0)
            })
            .expect("the busy timeout should be readable");
        let journal_mode = store
            .connection
            .pragma_query_value(None, "journal_mode", |row| {
                row.get::<_, String>(0)
            })
            .expect("the journal mode should be readable");

        assert_eq!(foreign_keys, 1);
        assert_eq!(busy_timeout, BUSY_TIMEOUT.as_millis() as i64);
        assert_eq!(journal_mode, "wal");
    }

    #[test]
    fn an_in_memory_store_keeps_its_supported_journal_mode() {
        let store = Store::open_in_memory().expect("the store should open");

        let journal_mode = store
            .connection
            .pragma_query_value(None, "journal_mode", |row| {
                row.get::<_, String>(0)
            })
            .expect("the journal mode should be readable");

        assert_eq!(journal_mode, "memory");
    }

    #[test]
    fn foreign_keys_are_enforced_without_caller_configuration() {
        let mut store = Store::open_in_memory().expect("the store should open");
        let project = Records::fixture().project;

        let error = store
            .transaction(|repositories| repositories.insert_project(&project))
            .expect_err("a project without its run must be rejected");

        assert!(matches!(
            error,
            super::StoreError::Database(rusqlite::Error::SqliteFailure(
                ref failure,
                _
            )) if failure.code == ErrorCode::ConstraintViolation
        ));
    }

    #[test]
    fn orderly_shutdown_stops_an_active_run_exactly_once() {
        let mut store = Store::open_in_memory().expect("the store should open");
        let run = Records::fixture().run;
        store
            .transaction(|repositories| repositories.insert_run(&run))
            .expect("the active run should be inserted");

        store
            .transaction(|repositories| repositories.stop_run(run.id, 20))
            .expect("the active run should stop");
        store
            .transaction(|repositories| {
                let stopped = repositories
                    .run(run.id)?
                    .expect("the stopped run should remain durable");
                assert_eq!(stopped.status, "stopped");
                assert_eq!(stopped.stopped_at, Some(20));
                Ok(())
            })
            .expect("the stopped run should be readable");

        let error = store
            .transaction(|repositories| repositories.stop_run(run.id, 21))
            .expect_err("a stopped run must not be stopped again");
        assert!(
            matches!(error, super::StoreError::RunNotActive { id } if id == run.id)
        );
    }

    #[test]
    fn every_write_transaction_reserves_the_single_writer_slot_immediately() {
        let database = TestDatabase::new();
        let mut store =
            Store::open(&database.0).expect("the store should open");
        let competing_writer = Connection::open(&database.0)
            .expect("a competing connection should open");
        competing_writer
            .busy_timeout(std::time::Duration::ZERO)
            .expect("the competing timeout should be configurable");

        store
            .transaction(|_| {
                let error = competing_writer
                    .execute(
                        "INSERT INTO runs (id, status, created_at) VALUES (?1, 'active', 1)",
                        [RUN_ID],
                    )
                    .expect_err("the store transaction must already own the writer slot");
                assert!(matches!(
                    error,
                    rusqlite::Error::SqliteFailure(ref failure, _)
                        if failure.code == ErrorCode::DatabaseBusy
                ));
                Ok(())
            })
            .expect("the owning transaction should commit");
    }

    #[test]
    fn mutations_replay_the_durable_result_without_reapplying_changes() {
        let mut store = Store::open_in_memory().expect("the store should open");
        let records = Records::fixture();
        store
            .transaction(|repositories| repositories.insert_run(&records.run))
            .expect("the run should commit");
        let mutation = Mutation {
            id: records.operation.id,
            run_id: records.run.id,
            kind: "project.attach".to_owned(),
            actor_agent_id: None,
            request: json!({"alias": records.project.alias}),
            created_at: 20,
        };
        let applications = Cell::new(0);

        let first = store
            .mutate(&mutation, |repositories| {
                applications.set(applications.get() + 1);
                repositories.insert_project(&records.project)?;
                Ok(json!({"project_id": records.project.id}))
            })
            .expect("the first mutation should commit");
        let replay = store
            .mutate(&mutation, |_| -> Result<_, super::StoreError> {
                panic!("an idempotent replay must not execute its mutation")
            })
            .expect("the retry should replay its durable result");

        assert_eq!(applications.get(), 1);
        assert_eq!(
            first,
            MutationOutcome::Applied(json!({"project_id": records.project.id}))
        );
        assert_eq!(
            replay,
            MutationOutcome::Replayed(
                json!({"project_id": records.project.id})
            )
        );
        store
            .transaction(|repositories| {
                let operation = repositories
                    .operation(mutation.id)?
                    .expect("the operation should be durable");
                assert_eq!(operation.status, "succeeded");
                assert_eq!(operation.attempt_count, 1);
                Ok(())
            })
            .expect("the operation should be inspectable");
    }

    #[test]
    fn reusing_an_operation_id_for_a_different_request_is_rejected() {
        let mut store = Store::open_in_memory().expect("the store should open");
        let records = Records::fixture();
        store
            .transaction(|repositories| repositories.insert_run(&records.run))
            .expect("the run should commit");
        let mutation = Mutation {
            id: records.operation.id,
            run_id: records.run.id,
            kind: "run.stop".to_owned(),
            actor_agent_id: None,
            request: json!({"reason": "done"}),
            created_at: 20,
        };
        store
            .mutate(&mutation, |_| Ok(json!({"stopped": true})))
            .expect("the first mutation should commit");
        let conflicting = Mutation {
            request: json!({"reason": "cancel"}),
            ..mutation
        };

        let error = store
            .mutate(&conflicting, |_| Ok(json!({"stopped": true})))
            .expect_err(
                "the operation identity must bind its original request",
            );

        assert!(matches!(
            error,
            super::StoreError::OperationConflict { id } if id == mutation.id
        ));
    }

    #[test]
    fn external_operation_reconciliation_is_durable_and_inspectable() {
        let mut store = Store::open_in_memory().expect("the store should open");
        let records = Records::fixture();
        let operation = OperationRecord {
            actor_agent_id: None,
            kind: "agent.spawn".to_owned(),
            ..records.operation.clone()
        };
        store
            .transaction(|repositories| {
                repositories.insert_run(&records.run)?;
                repositories.insert_operation(&operation)?;
                repositories.mark_operation_reconciliation_desired(operation.id)
            })
            .expect("the external-operation intent should commit");

        store
            .transaction(|repositories| {
                assert_eq!(
                    repositories.record_operation_reconciliation(
                        records.operation.id,
                        ExternalResourceState::Unknown,
                        Some("provider identity could not be proved"),
                        20,
                    )?,
                    ResourceTransitionOutcome::Applied
                );
                assert_eq!(
                    repositories.record_operation_reconciliation(
                        records.operation.id,
                        ExternalResourceState::Unknown,
                        Some("provider identity could not be proved"),
                        21,
                    )?,
                    ResourceTransitionOutcome::Unchanged
                );
                Ok(())
            })
            .expect("reconciliation attempts should commit");

        store
            .transaction(|repositories| {
                let operation = repositories
                    .operation(records.operation.id)?
                    .expect("the operation should remain durable");
                assert_eq!(
                    operation.reconciliation_state,
                    Some(ExternalResourceState::Unknown)
                );
                assert_eq!(operation.reconciliation_attempt_count, 2);
                assert_eq!(
                    operation.reconciliation_error.as_deref(),
                    Some("provider identity could not be proved")
                );
                assert_eq!(operation.reconciled_at, Some(21));
                assert_eq!(
                    repositories
                        .operations_requiring_reconciliation(records.run.id)?,
                    vec![operation]
                );
                Ok(())
            })
            .expect("reconciliation state should be readable");
    }

    #[test]
    fn unknown_integration_operations_remain_reconcilable() {
        let mut store = Store::open_in_memory().expect("the store should open");
        let records = Records::fixture();
        let operation = OperationRecord {
            actor_agent_id: None,
            kind: "workspace.integrate".to_owned(),
            status: "succeeded".to_owned(),
            result: Some(json!({"assignment_id": records.assignment.id})),
            reconciliation_state: Some(ExternalResourceState::Unknown),
            reconciliation_attempt_count: 1,
            reconciliation_error: Some(
                "repository is temporarily unavailable".to_owned(),
            ),
            reconciled_at: Some(20),
            ..records.operation.clone()
        };
        store
            .transaction(|repositories| {
                repositories.insert_run(&records.run)?;
                repositories.insert_operation(&operation)
            })
            .expect("the integration operation should commit");

        store
            .transaction(|repositories| {
                assert_eq!(
                    repositories
                        .operations_requiring_reconciliation(records.run.id)?,
                    vec![operation]
                );
                Ok(())
            })
            .expect("the uncertain integration should remain reconcilable");
    }

    #[test]
    fn foreground_session_lookup_ignores_already_active_attempts() {
        let mut store = Store::open_in_memory().expect("the store should open");
        let records = Records::fixture();
        let rejected = OperationRecord {
            actor_agent_id: None,
            kind: "agent.launch_foreground".to_owned(),
            status: "succeeded".to_owned(),
            request: json!({"role": "lead"}),
            result: Some(json!({
                "agent_id": records.agent.id,
                "session_id": records.session.id,
                "generation": records.session.generation,
                "already_active": true,
            })),
            attempt_count: 1,
            ..records.operation.clone()
        };
        let creator = OperationRecord {
            id: SECOND_OPERATION_ID
                .parse()
                .expect("the operation ID should parse"),
            result: Some(json!({
                "agent_id": records.agent.id,
                "session_id": records.session.id,
                "generation": records.session.generation,
                "already_active": false,
            })),
            ..rejected.clone()
        };
        store
            .transaction(|repositories| {
                repositories.insert_run(&records.run)?;
                repositories.insert_operation(&rejected)?;
                assert_eq!(
                    repositories.foreground_launch_operation_for_session(
                        records.run.id,
                        records.session.id,
                    )?,
                    None
                );
                repositories.insert_operation(&creator)?;
                assert_eq!(
                    repositories.foreground_launch_operation_for_session(
                        records.run.id,
                        records.session.id,
                    )?,
                    Some(creator)
                );
                Ok(())
            })
            .expect("a rejected launch must not own the active session");
    }

    #[test]
    fn retries_reject_incomplete_and_corrupt_durable_results() {
        let cases = [
            ("pending", None, "incomplete"),
            ("succeeded", None, "missing"),
            ("succeeded", Some(json!({"unexpected": true})), "invalid"),
        ];

        for (status, result, expected) in cases {
            let mut store =
                Store::open_in_memory().expect("the store should open");
            let records = Records::fixture();
            let mutation = Mutation {
                id: records.operation.id,
                run_id: records.run.id,
                kind: "run.stop".to_owned(),
                actor_agent_id: None,
                request: json!({"reason": "done"}),
                created_at: 20,
            };
            store
                .transaction(|repositories| {
                    repositories.insert_run(&records.run)?;
                    repositories.insert_operation(&OperationRecord {
                        id: mutation.id,
                        run_id: mutation.run_id,
                        kind: mutation.kind.clone(),
                        actor_agent_id: mutation.actor_agent_id,
                        status: status.to_owned(),
                        request: mutation.request.clone(),
                        result: result.clone(),
                        attempt_count: 1,
                        reconciliation_state: None,
                        reconciliation_attempt_count: 0,
                        reconciliation_error: None,
                        reconciled_at: None,
                        created_at: mutation.created_at,
                        updated_at: mutation.created_at,
                    })
                })
                .expect("the corrupt operation fixture should commit");

            let retry: Result<MutationOutcome<bool>, _> =
                store.mutate(&mutation, |_| Ok(true));
            let error =
                retry.expect_err("corrupt retry state must be rejected");
            match expected {
                "incomplete" => assert!(matches!(
                    error,
                    super::StoreError::OperationIncomplete { id, ref status }
                        if id == mutation.id && status == "pending"
                )),
                "missing" => assert!(matches!(
                    error,
                    super::StoreError::MissingOperationResult { id }
                        if id == mutation.id
                )),
                "invalid" => {
                    assert!(matches!(error, super::StoreError::EncodeJson(_)));
                }
                _ => unreachable!("the test cases are exhaustive"),
            }
        }
    }

    #[test]
    fn claiming_a_task_atomically_creates_one_claim_and_assignment() {
        let mut store = Store::open_in_memory().expect("the store should open");
        let records = Records::fixture();
        insert_claim_prerequisites(&mut store, &records);
        let mutation = claim_mutation(&records);

        let outcome = store
            .claim_task(&mutation)
            .expect("the ready task should be claimed");

        assert_eq!(
            outcome,
            MutationOutcome::Applied(ClaimTaskResult::Claimed {
                claim_id: 1,
                assignment_id: records.assignment.id,
            })
        );
        store
            .transaction(|repositories| {
                assert_eq!(
                    repositories
                        .task(records.task.id)?
                        .expect("the task should exist")
                        .status,
                    TaskStatus::InProgress
                );
                assert_eq!(
                    repositories
                        .claim(1)?
                        .expect("the claim should exist")
                        .operation_id,
                    mutation.operation_id
                );
                assert_eq!(
                    repositories
                        .assignment(records.assignment.id)?
                        .expect("the assignment should exist")
                        .claim_id,
                    1
                );
                Ok(())
            })
            .expect("the claim should be inspectable");
    }

    #[test]
    fn normalized_events_commit_atomically_once_with_each_state_transition() {
        let mut store = Store::open_in_memory().expect("the store should open");
        let records = Records::fixture();
        insert_claim_prerequisites(&mut store, &records);
        let claim = claim_mutation(&records);

        let applied = store
            .claim_task(&claim)
            .expect("the claim and its events should commit");
        assert_eq!(
            store
                .claim_task(&claim)
                .expect("the claim retry should replay"),
            applied.as_replayed()
        );
        store
            .transition_task(&transition_mutation(
                &records,
                TaskTransition::Submit,
            ))
            .expect("the task transition and its events should commit");

        store
            .transaction(|repositories| {
                let events =
                    repositories.events_after(records.run.id, 0, 100)?;
                assert_eq!(
                    events
                        .iter()
                        .map(|event| event.event_type.as_str())
                        .collect::<Vec<_>>(),
                    [
                        "task.lifecycle_changed",
                        "task.claimed",
                        "assignment.created",
                        "task.lifecycle_changed",
                        "assignment.lifecycle_changed",
                        "claim.released",
                    ]
                );
                assert_eq!(
                    events
                        .iter()
                        .map(|event| event.sequence)
                        .collect::<Vec<_>>(),
                    (1..=6).collect::<Vec<_>>()
                );
                assert!(events.iter().all(|event| {
                    event.payload["schema_version"] == 1
                        && event.payload.get("data").is_some()
                }));
                Ok(())
            })
            .expect("the event stream should be inspectable");
    }

    #[test]
    fn a_failed_event_insert_rolls_back_its_state_transition() {
        let mut store = Store::open_in_memory().expect("the store should open");
        let records = Records::fixture();
        insert_claim_prerequisites(&mut store, &records);
        store
            .connection
            .execute_batch(
                "CREATE TRIGGER reject_normalized_events \
                 BEFORE INSERT ON events BEGIN \
                     SELECT RAISE(ABORT, 'injected event failure'); \
                 END;",
            )
            .expect("the failure injection should be installed");

        let mutation = claim_mutation(&records);
        assert!(matches!(
            store.claim_task(&mutation),
            Err(super::StoreError::Database(_))
        ));
        store
            .transaction(|repositories| {
                assert_eq!(
                    repositories
                        .task(records.task.id)?
                        .expect("the task should remain")
                        .status,
                    TaskStatus::Open
                );
                assert_eq!(
                    repositories.operation(mutation.operation_id)?,
                    None
                );
                assert!(
                    repositories
                        .events_after(records.run.id, 0, 100)?
                        .is_empty()
                );
                Ok(())
            })
            .expect("the rolled-back state should remain readable");
    }

    #[test]
    fn claim_retries_and_contenders_observe_the_compare_and_set_result() {
        let mut store = Store::open_in_memory().expect("the store should open");
        let records = Records::fixture();
        insert_claim_prerequisites(&mut store, &records);
        let mutation = claim_mutation(&records);
        let applied = store
            .claim_task(&mutation)
            .expect("the first claim should commit");

        let retry = ClaimTaskMutation {
            assignment_id: SECOND_ASSIGNMENT_ID
                .parse()
                .expect("the assignment ID should parse"),
            claimed_at: mutation.claimed_at + 10,
            ..mutation.clone()
        };
        let replay = store
            .claim_task(&retry)
            .expect("the same operation should replay");
        assert_eq!(replay, applied.as_replayed());

        let contender = ClaimTaskMutation {
            operation_id: SECOND_OPERATION_ID
                .parse()
                .expect("the operation ID should parse"),
            assignment_id: SECOND_ASSIGNMENT_ID
                .parse()
                .expect("the assignment ID should parse"),
            ..mutation
        };
        let rejected = store
            .claim_task(&contender)
            .expect("claim contention is a durable domain result");
        assert_eq!(
            rejected,
            MutationOutcome::Applied(ClaimTaskResult::Rejected(
                ClaimRejection::AlreadyClaimed
            ))
        );
        assert_eq!(
            store
                .claim_task(&contender)
                .expect("a rejected contention should also replay"),
            rejected.as_replayed()
        );

        let (claims, assignments) = store
            .transaction(|repositories| {
                let claims = repositories.transaction.query_row(
                    "SELECT count(*) FROM claims",
                    [],
                    |row| row.get::<_, i64>(0),
                )?;
                let assignments = repositories.transaction.query_row(
                    "SELECT count(*) FROM assignments",
                    [],
                    |row| row.get::<_, i64>(0),
                )?;
                Ok((claims, assignments))
            })
            .expect("claim counts should be readable");
        assert_eq!((claims, assignments), (1, 1));
    }

    #[test]
    fn a_task_with_an_open_dependency_cannot_be_claimed() {
        let mut store = Store::open_in_memory().expect("the store should open");
        let mut records = Records::fixture();
        records.dependency_task.status = TaskStatus::Open;
        insert_claim_prerequisites(&mut store, &records);
        let mutation = claim_mutation(&records);

        let outcome = store
            .claim_task(&mutation)
            .expect("a blocked claim should produce a durable result");

        assert_eq!(
            outcome,
            MutationOutcome::Applied(ClaimTaskResult::Rejected(
                ClaimRejection::Blocked
            ))
        );
        store
            .transaction(|repositories| {
                assert_eq!(
                    repositories
                        .task(records.task.id)?
                        .expect("the task should exist")
                        .status,
                    TaskStatus::Open
                );
                assert_eq!(repositories.claim(1)?, None);
                Ok(())
            })
            .expect("the rejected claim should not change task state");
    }

    #[test]
    fn an_agent_cannot_hold_two_active_assignments() {
        let mut store = Store::open_in_memory().expect("the store should open");
        let records = Records::fixture();
        insert_claim_prerequisites(&mut store, &records);
        store
            .claim_task(&claim_mutation(&records))
            .expect("the first claim should commit");
        let second_task = TaskRecord {
            id: SECOND_TASK_ID.parse().expect("the task ID should parse"),
            title: "Second task".to_owned(),
            ..records.task.clone()
        };
        store
            .transaction(|repositories| repositories.insert_task(&second_task))
            .expect("the second task should commit");
        let contender = ClaimTaskMutation {
            operation_id: SECOND_OPERATION_ID
                .parse()
                .expect("the operation ID should parse"),
            run_id: records.run.id,
            actor_agent_id: Some(records.agent.id),
            task_id: second_task.id,
            agent_id: records.agent.id,
            assignment_id: SECOND_ASSIGNMENT_ID
                .parse()
                .expect("the assignment ID should parse"),
            claimed_at: records.claim.claimed_at + 1,
        };

        let outcome = store
            .claim_task(&contender)
            .expect("agent contention should produce a durable result");

        assert_eq!(
            outcome,
            MutationOutcome::Applied(ClaimTaskResult::Rejected(
                ClaimRejection::AgentBusy
            ))
        );
    }

    #[test]
    fn claim_precondition_rejections_are_durable_and_idempotent() {
        let cases = [
            ("agent_not_found", ClaimRejection::AgentNotFound),
            ("task_not_found", ClaimRejection::TaskNotFound),
            ("task_not_open", ClaimRejection::TaskNotOpen),
        ];

        for (case, expected) in cases {
            let mut store =
                Store::open_in_memory().expect("the store should open");
            let mut records = Records::fixture();
            if case == "task_not_open" {
                records.task.status = TaskStatus::Submitted;
            }
            insert_claim_prerequisites(&mut store, &records);
            let mut mutation = claim_mutation(&records);
            match case {
                "agent_not_found" => mutation.agent_id = AgentId::generate(),
                "task_not_found" => mutation.task_id = TaskId::generate(),
                "task_not_open" => {}
                _ => unreachable!("the test cases are exhaustive"),
            }

            let rejected = store
                .claim_task(&mutation)
                .expect("a failed precondition should be a durable result");
            assert_eq!(
                rejected,
                MutationOutcome::Applied(ClaimTaskResult::Rejected(expected))
            );
            assert_eq!(
                store
                    .claim_task(&mutation)
                    .expect("a rejected claim should replay"),
                rejected.as_replayed()
            );
        }
    }

    #[test]
    fn assignment_failure_rolls_back_the_claim_and_operation() {
        let mut store = Store::open_in_memory().expect("the store should open");
        let records = Records::fixture();
        insert_claim_prerequisites(&mut store, &records);
        store
            .connection
            .execute_batch(
                "CREATE TEMP TRIGGER reject_assignment BEFORE INSERT ON assignments \
                 BEGIN SELECT RAISE(ABORT, 'injected assignment failure'); END;",
            )
            .expect("the failure-injection trigger should be installed");
        let mutation = claim_mutation(&records);

        let error = store
            .claim_task(&mutation)
            .expect_err("assignment failure must abort the atomic claim");
        assert!(matches!(error, super::StoreError::Database(_)));

        store
            .transaction(|repositories| {
                assert_eq!(
                    repositories
                        .task(records.task.id)?
                        .expect("the task should exist")
                        .status,
                    TaskStatus::Open
                );
                assert_eq!(repositories.claim(1)?, None);
                assert_eq!(
                    repositories.operation(mutation.operation_id)?,
                    None
                );
                Ok(())
            })
            .expect("rolled-back state should be inspectable");
    }

    #[test]
    fn readiness_is_derived_from_status_dependencies_and_claims() {
        for dependency_status in [
            TaskStatus::Open,
            TaskStatus::InProgress,
            TaskStatus::Submitted,
            TaskStatus::Closed,
            TaskStatus::Canceled,
        ] {
            let mut store =
                Store::open_in_memory().expect("the store should open");
            let mut records = Records::fixture();
            records.dependency_task.status = dependency_status;
            insert_claim_prerequisites(&mut store, &records);

            store
                .transaction(|repositories| {
                    let readiness = repositories
                        .task_readiness(records.task.id)?
                        .expect("the task should exist");
                    let ready_ids = repositories
                        .ready_tasks(records.run.id)?
                        .into_iter()
                        .map(|task| task.id)
                        .collect::<Vec<_>>();

                    assert_eq!(readiness.status, TaskStatus::Open);
                    assert_eq!(
                        readiness.unresolved_dependencies,
                        if dependency_status == TaskStatus::Closed {
                            Vec::new()
                        } else {
                            vec![records.dependency_task.id]
                        }
                    );
                    assert!(!readiness.has_active_claim);
                    assert_eq!(
                        readiness.is_ready(),
                        dependency_status == TaskStatus::Closed
                    );
                    assert_eq!(
                        ready_ids.contains(&records.task.id),
                        dependency_status == TaskStatus::Closed
                    );
                    Ok(())
                })
                .expect("readiness should be queryable");
        }

        let mut store = Store::open_in_memory().expect("the store should open");
        let records = Records::fixture();
        insert_claim_prerequisites(&mut store, &records);
        store
            .claim_task(&claim_mutation(&records))
            .expect("the task should be claimed");

        store
            .transaction(|repositories| {
                let readiness = repositories
                    .task_readiness(records.task.id)?
                    .expect("the task should exist");

                assert_eq!(readiness.status, TaskStatus::InProgress);
                assert!(readiness.has_active_claim);
                assert!(!readiness.is_ready());
                Ok(())
            })
            .expect("claimed readiness should be queryable");
    }

    #[test]
    fn lifecycle_transitions_persist_and_release_active_ownership() {
        let cases = [
            (
                TaskStatus::Open,
                TaskTransition::Cancel,
                TaskStatus::Canceled,
            ),
            (
                TaskStatus::InProgress,
                TaskTransition::Reopen,
                TaskStatus::Open,
            ),
            (
                TaskStatus::InProgress,
                TaskTransition::Submit,
                TaskStatus::Submitted,
            ),
            (
                TaskStatus::InProgress,
                TaskTransition::Cancel,
                TaskStatus::Canceled,
            ),
            (
                TaskStatus::Submitted,
                TaskTransition::Reopen,
                TaskStatus::Open,
            ),
            (
                TaskStatus::Submitted,
                TaskTransition::Close,
                TaskStatus::Closed,
            ),
            (
                TaskStatus::Submitted,
                TaskTransition::Cancel,
                TaskStatus::Canceled,
            ),
        ];

        for (from, transition, expected) in cases {
            let mut store =
                Store::open_in_memory().expect("the store should open");
            let mut records = Records::fixture();
            if from == TaskStatus::InProgress {
                insert_claim_prerequisites(&mut store, &records);
                store
                    .claim_task(&claim_mutation(&records))
                    .expect("the task should be claimed");
            } else {
                records.task.status = from;
                insert_claim_prerequisites(&mut store, &records);
            }
            let mutation = transition_mutation(&records, transition);

            let outcome = store
                .transition_task(&mutation)
                .expect("the lifecycle transition should commit");

            assert_eq!(
                outcome,
                MutationOutcome::Applied(TaskTransitionResult::Transitioned {
                    previous_status: from,
                    status: expected,
                })
            );
            store
                .transaction(|repositories| {
                    let task = repositories
                        .task(records.task.id)?
                        .expect("the task should exist");
                    assert_eq!(task.status, expected);

                    if from == TaskStatus::InProgress {
                        let claim = repositories
                            .claim(1)?
                            .expect("the claim should remain auditable");
                        let assignment = repositories
                            .assignment(records.assignment.id)?
                            .expect("the assignment should remain auditable");
                        assert_eq!(claim.state, "released");
                        assert_eq!(
                            claim.released_at,
                            Some(mutation.transitioned_at)
                        );
                        assert_eq!(
                            assignment.state,
                            match transition {
                                TaskTransition::Reopen => "released",
                                TaskTransition::Submit => "completed",
                                TaskTransition::Cancel => "canceled",
                                TaskTransition::Close => unreachable!(
                                    "an in-progress task cannot close directly"
                                ),
                            }
                        );
                        assert_eq!(
                            assignment.completed_at,
                            Some(mutation.transitioned_at)
                        );
                    }
                    Ok(())
                })
                .expect("the transition should be inspectable");
        }
    }

    #[test]
    fn closing_a_dependency_releases_its_dependents_without_stored_blocking_state()
     {
        let mut store = Store::open_in_memory().expect("the store should open");
        let mut records = Records::fixture();
        records.dependency_task.status = TaskStatus::Submitted;
        insert_claim_prerequisites(&mut store, &records);

        let before = store
            .transaction(|repositories| {
                repositories.task_readiness(records.task.id)
            })
            .expect("readiness should be queryable")
            .expect("the task should exist");
        assert!(!before.is_ready());

        let mutation = TaskTransitionMutation {
            task_id: records.dependency_task.id,
            ..transition_mutation(&records, TaskTransition::Close)
        };
        store
            .transition_task(&mutation)
            .expect("the dependency should close");

        let after = store
            .transaction(|repositories| {
                repositories.task_readiness(records.task.id)
            })
            .expect("readiness should be queryable")
            .expect("the task should exist");
        assert!(after.is_ready());
        assert!(after.unresolved_dependencies.is_empty());
    }

    #[test]
    fn invalid_and_retried_lifecycle_transitions_are_durable_results() {
        let mut store = Store::open_in_memory().expect("the store should open");
        let mut records = Records::fixture();
        records.task.status = TaskStatus::Submitted;
        insert_claim_prerequisites(&mut store, &records);
        let mutation = transition_mutation(&records, TaskTransition::Close);

        let applied = store
            .transition_task(&mutation)
            .expect("the close should commit");
        let replayed = store
            .transition_task(&mutation)
            .expect("the retry should replay");
        assert_eq!(replayed, applied.as_replayed());

        let invalid = TaskTransitionMutation {
            operation_id: OperationId::generate(),
            transition: TaskTransition::Reopen,
            ..mutation.clone()
        };
        let rejected = store
            .transition_task(&invalid)
            .expect("an invalid transition should be a domain result");
        assert_eq!(
            rejected,
            MutationOutcome::Applied(TaskTransitionResult::Rejected(
                TaskTransitionRejection::InvalidStatus {
                    status: TaskStatus::Closed,
                }
            ))
        );
        assert_eq!(
            store
                .transition_task(&invalid)
                .expect("the rejection should replay"),
            rejected.as_replayed()
        );

        let missing = TaskTransitionMutation {
            operation_id: OperationId::generate(),
            task_id: TaskId::generate(),
            ..mutation
        };
        let rejected = store
            .transition_task(&missing)
            .expect("a missing task should be a domain result");
        assert_eq!(
            rejected,
            MutationOutcome::Applied(TaskTransitionResult::Rejected(
                TaskTransitionRejection::TaskNotFound
            ))
        );
        assert_eq!(
            store
                .transition_task(&missing)
                .expect("the missing-task result should replay"),
            rejected.as_replayed()
        );
    }

    #[test]
    fn worktree_closure_requires_durable_integration_evidence() {
        let mut store = Store::open_in_memory().expect("the store should open");
        let records = Records::fixture();
        insert_claim_prerequisites(&mut store, &records);
        store
            .claim_task(&claim_mutation(&records))
            .expect("the task should be claimed");
        let submitted = TaskTransitionMutation {
            operation_id: OperationId::generate(),
            ..transition_mutation(&records, TaskTransition::Submit)
        };
        store
            .transition_task(&submitted)
            .expect("the assignment should be submitted");
        let mut workspace = records.workspace.clone();
        workspace.state = ExternalResourceState::Observed;
        workspace.result_commit =
            Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_owned());
        store
            .transaction(|repositories| {
                repositories.insert_workspace(&workspace)
            })
            .expect("the submitted worktree should be recorded");
        let closure = TaskTransitionMutation {
            operation_id: OperationId::generate(),
            ..transition_mutation(&records, TaskTransition::Close)
        };

        let rejected = store
            .transition_task(&closure)
            .expect("the acceptance rejection should be durable");

        assert_eq!(
            rejected,
            MutationOutcome::Applied(TaskTransitionResult::Rejected(
                TaskTransitionRejection::AcceptanceNotMet
            ))
        );
        store
            .transaction(|repositories| {
                repositories.record_workspace_target_commit(
                    workspace.scope(),
                    "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                )?;
                Ok(())
            })
            .expect("integration evidence should be recorded");
        assert_eq!(
            store
                .transition_task(&closure)
                .expect("the rejected operation should replay"),
            rejected.as_replayed()
        );
        let accepted = TaskTransitionMutation {
            operation_id: OperationId::generate(),
            ..closure
        };
        assert!(matches!(
            store
                .transition_task(&accepted)
                .expect("a new close should observe the integration"),
            MutationOutcome::Applied(TaskTransitionResult::Transitioned {
                status: TaskStatus::Closed,
                ..
            })
        ));
    }

    #[test]
    fn a_transition_refuses_corrupt_in_progress_ownership_atomically() {
        let mut store = Store::open_in_memory().expect("the store should open");
        let mut records = Records::fixture();
        records.task.status = TaskStatus::InProgress;
        insert_claim_prerequisites(&mut store, &records);
        let mutation = transition_mutation(&records, TaskTransition::Submit);

        let error = store
            .transition_task(&mutation)
            .expect_err("missing ownership must be reported as corrupt state");
        assert!(matches!(
            error,
            super::StoreError::CorruptTaskState { id, .. }
                if id == records.task.id
        ));

        store
            .transaction(|repositories| {
                assert_eq!(
                    repositories
                        .task(records.task.id)?
                        .expect("the task should exist")
                        .status,
                    TaskStatus::InProgress
                );
                assert_eq!(
                    repositories.operation(mutation.operation_id)?,
                    None
                );
                Ok(())
            })
            .expect("the rollback should be inspectable");
    }

    #[test]
    fn reopening_a_store_does_not_reapply_migrations() {
        let database = TestDatabase::new();
        drop(Store::open(&database.0).expect("the first open should migrate"));

        let store =
            Store::open(&database.0).expect("the migrated store should reopen");
        let applied = store
            .connection
            .query_row("SELECT count(*) FROM schema_migrations", [], |row| {
                row.get::<_, i64>(0)
            })
            .expect("the migration ledger should be readable");

        assert_eq!(applied, MIGRATIONS.len() as i64);
    }

    #[test]
    fn every_released_schema_upgrades_through_all_forward_migrations() {
        for prior_count in 1..MIGRATIONS.len() {
            let database = TestDatabase::new();
            let connection = Connection::open(&database.0)
                .expect("the prior database should open");
            connection
                .execute_batch(
                    "CREATE TABLE schema_migrations (\
                         version INTEGER PRIMARY KEY,\
                         name TEXT NOT NULL,\
                         source TEXT NOT NULL,\
                         applied_at INTEGER NOT NULL DEFAULT (unixepoch())\
                     ) STRICT;",
                )
                .expect("the migration ledger should be created");
            for migration in &MIGRATIONS[..prior_count] {
                connection
                    .execute_batch(migration.sql)
                    .expect("the prior schema should be created");
                connection
                    .execute(
                        "INSERT INTO schema_migrations (version, name, source) \
                         VALUES (?1, ?2, ?3)",
                        (migration.version, migration.name, migration.sql),
                    )
                    .expect("the prior migration should be recorded");
            }
            connection
                .execute(
                    "INSERT INTO runs (id, status, created_at) \
                     VALUES (?1, 'active', 10)",
                    [RUN_ID],
                )
                .expect("the prior run should be inserted");
            connection
                .execute(
                    "INSERT INTO agents (\
                         id, run_id, role, generation, state, created_at\
                     ) VALUES (?1, ?2, 'worker', 2, 'running', 11)",
                    rusqlite::params![AGENT_ID, RUN_ID],
                )
                .expect("the prior agent should be inserted");
            connection
                .execute(
                    "INSERT INTO sessions (\
                         id, run_id, agent_id, generation, provider, state, \
                         transcript_path, created_at\
                     ) VALUES (\
                         ?1, ?2, ?3, 2, 'fake', 'running', ?4, 12\
                     )",
                    rusqlite::params![
                        SESSION_ID,
                        RUN_ID,
                        AGENT_ID,
                        b"transcripts/prior.jsonl".as_slice(),
                    ],
                )
                .expect("the prior session should be inserted");
            if prior_count >= 5 {
                connection
                    .execute(
                        "UPDATE sessions SET provider_session_id = 'process:123' \
                         WHERE id = ?1",
                        [SESSION_ID],
                    )
                    .expect("the prior foreground process identity should be set");
            }
            if prior_count >= 6 {
                connection
                    .execute(
                        "UPDATE sessions SET process_owner = 'foreground' \
                         WHERE id = ?1",
                        [SESSION_ID],
                    )
                    .expect("the prior process owner should be set");
            }
            connection.execute(
                "INSERT INTO projects (id, run_id, alias, original_path, canonical_path, identity_json, is_primary, attached_at) \
                 VALUES (?1, ?2, 'primary', ?3, ?3, '{}', 1, 10)",
                rusqlite::params![PROJECT_ID, RUN_ID, b"/tmp/project".as_slice()],
            ).expect("legacy project");
            connection.execute(
                "INSERT INTO tasks (id, run_id, project_id, title, description, status, created_at, updated_at) \
                 VALUES (?1, ?2, ?3, 'task', 'task', 'submitted', 10, 12)",
                rusqlite::params![TASK_ID, RUN_ID, PROJECT_ID],
            ).expect("legacy task");
            connection.execute(
                "INSERT INTO operations (id, run_id, kind, status, request_json, result_json, attempt_count, created_at, updated_at) \
                 VALUES (?1, ?2, 'workspace.integrate', 'succeeded', '{}', ?3, 1, 12, 12)",
                rusqlite::params![OPERATION_ID, RUN_ID, json!({
                    "assignment_id": ASSIGNMENT_ID, "project_id": PROJECT_ID,
                    "target_reference": "refs/heads/main", "target_commit": "base", "integrated_at": 12,
                }).to_string()],
            ).expect("legacy integration plan");
            if prior_count >= 8 {
                connection.execute("UPDATE operations SET result_json = json_set(result_json, '$.run_id', ?1, '$.generation', 2) WHERE id = ?2", rusqlite::params![RUN_ID, OPERATION_ID]).unwrap();
            }
            connection.execute(
                "INSERT INTO claims (id, run_id, task_id, agent_id, operation_id, state, claimed_at, released_at) \
                 VALUES (1, ?1, ?2, ?3, ?4, 'released', 10, 12)",
                rusqlite::params![RUN_ID, TASK_ID, AGENT_ID, OPERATION_ID],
            ).expect("legacy claim");
            connection.execute(
                "INSERT INTO assignments (id, run_id, task_id, agent_id, session_id, claim_id, generation, state, created_at, completed_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, 1, 2, 'completed', 10, 12)",
                rusqlite::params![ASSIGNMENT_ID, RUN_ID, TASK_ID, AGENT_ID, SESSION_ID],
            ).expect("legacy assignment");
            let workspace_sql = if prior_count >= 8 {
                "INSERT INTO workspaces (assignment_id, run_id, project_id, kind, path, state, created_at, generation) VALUES (?1, ?2, ?3, 'worktree', ?4, 'observed', 10, 2)"
            } else {
                "INSERT INTO workspaces (assignment_id, run_id, project_id, kind, path, state, created_at) VALUES (?1, ?2, ?3, 'worktree', ?4, 'observed', 10)"
            };
            connection
                .execute(
                    workspace_sql,
                    rusqlite::params![
                        ASSIGNMENT_ID,
                        RUN_ID,
                        PROJECT_ID,
                        b"/tmp/workspace".as_slice()
                    ],
                )
                .expect("legacy workspace");
            if prior_count >= 11 {
                connection
                    .execute_batch(MIGRATIONS[10].sql)
                    .expect("the prior runtime saved its historical policy");
            }
            drop(connection);

            let store =
                Store::open(&database.0).expect("the database should upgrade");
            let mut store = store;
            let configuration = store
                .configuration(RUN_ID.parse().unwrap())
                .expect("upgrades pin the historical compiled runtime policy");
            assert_eq!(
                configuration,
                crate::config::resolve(
                    &Default::default(),
                    &Default::default(),
                    &Default::default()
                )
                .unwrap()
            );
            assert!(configuration.allowed_project_roots.is_empty());
            let document: String = store.connection.query_row("SELECT document_json FROM configuration_snapshots WHERE scope = 'run'", [], |row| row.get(0)).unwrap();
            let document: serde_json::Value =
                serde_json::from_str(&document).unwrap();
            assert_eq!(
                document["effective"]["allowed_project_roots"],
                json!([])
            );
            assert_eq!(
                document["provenance"]["allowed_project_roots"]["source"]["layer"],
                "compiled"
            );
            let applied = store
                .connection
                .query_row(
                    "SELECT count(*) FROM schema_migrations",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .expect("the migration ledger should be readable");
            let request_fingerprint: Option<String> = store
                .connection
                .query_row(
                    "SELECT request_fingerprint FROM operations WHERE id = ?1",
                    [OPERATION_ID],
                    |row| row.get(0),
                )
                .expect("the fingerprint column should exist after upgrade");
            assert!(request_fingerprint.is_none());
            let credential_table = store
                .connection
                .query_row(
                    "SELECT count(*) FROM sqlite_schema \
                     WHERE type = 'table' AND name = 'session_credentials'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .expect("the credential table should be inspectable");
            let claim_indexes = store
                .connection
                .query_row(
                    "SELECT count(*) FROM sqlite_schema \
                     WHERE type = 'index' AND name IN (\
                         'one_claim_per_operation',\
                         'one_assignment_per_claim',\
                         'one_active_assignment_per_agent'\
                     )",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .expect("the claim indexes should be inspectable");
            let message_triggers = store
                .connection
                .query_row(
                    "SELECT count(*) FROM sqlite_schema \
                     WHERE type = 'trigger' AND name IN (\
                         'messages_cannot_be_deleted',\
                         'message_content_is_immutable',\
                         'message_acknowledgements_are_final'\
                     )",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .expect("the message triggers should be inspectable");
            let preserved_sessions = store
                .connection
                .query_row("SELECT count(*) FROM sessions", [], |row| {
                    row.get::<_, i64>(0)
                })
                .expect("prior sessions should remain readable");
            let process_owner = store
                .connection
                .query_row(
                    "SELECT process_owner FROM sessions WHERE id = ?1",
                    [SESSION_ID],
                    |row| row.get::<_, String>(0),
                )
                .expect("the migrated process owner should be readable");

            let generation: i64 = store.connection.query_row(
                "SELECT generation FROM workspaces WHERE assignment_id = ?1", [ASSIGNMENT_ID], |row| row.get(0),
            ).expect("migrated workspace generation");
            let plan_json: String = store
                .connection
                .query_row(
                    "SELECT result_json FROM operations WHERE id = ?1",
                    [OPERATION_ID],
                    |row| row.get(0),
                )
                .expect("migrated integration plan");
            let plan: crate::workspace::IntegrationPlan =
                serde_json::from_str(&plan_json).expect("typed migrated plan");
            assert_eq!(generation, 2);
            assert_eq!(plan.generation, generation);
            assert_eq!(plan.run_id.to_string(), RUN_ID);
            for statement in [
                "UPDATE workspaces SET generation = 3",
                "UPDATE assignments SET generation = 3",
                "UPDATE workspaces SET run_id = 'another-run'",
                "UPDATE assignments SET run_id = 'another-run'",
            ] {
                assert!(
                    store.connection.execute(statement, []).is_err(),
                    "{statement}"
                );
            }
            assert_eq!(applied, MIGRATIONS.len() as i64);
            assert_eq!(credential_table, 1);
            assert_eq!(claim_indexes, 3);
            assert_eq!(message_triggers, 3);
            assert_eq!(preserved_sessions, 1);
            assert_eq!(
                process_owner,
                if prior_count >= 5 {
                    "foreground"
                } else {
                    "supervisor"
                }
            );
        }
    }

    #[test]
    fn operation_reconciliation_migration_classifies_external_intents() {
        let database = TestDatabase::new();
        let connection = Connection::open(&database.0)
            .expect("the prior database should open");
        connection
            .execute_batch(
                "CREATE TABLE schema_migrations (\
                     version INTEGER PRIMARY KEY,\
                     name TEXT NOT NULL,\
                     source TEXT NOT NULL,\
                     applied_at INTEGER NOT NULL DEFAULT (unixepoch())\
                 ) STRICT;",
            )
            .expect("the migration ledger should be created");
        for migration in &MIGRATIONS[..6] {
            connection
                .execute_batch(migration.sql)
                .expect("the released schema should be created");
            connection
                .execute(
                    "INSERT INTO schema_migrations (version, name, source) \
                     VALUES (?1, ?2, ?3)",
                    (migration.version, migration.name, migration.sql),
                )
                .expect("the prior migration should be recorded");
        }
        connection
            .execute(
                "INSERT INTO runs (id, status, created_at) \
                 VALUES (?1, 'active', 10)",
                [RUN_ID],
            )
            .expect("the prior run should be inserted");
        let kinds = [
            "agent.launch_foreground",
            "agent.spawn",
            "workspace.integrate",
            "task.create",
        ];
        let operation_ids = kinds
            .iter()
            .map(|_| OperationId::generate())
            .collect::<Vec<_>>();
        for (operation_id, kind) in operation_ids.iter().zip(kinds) {
            connection
                .execute(
                    "INSERT INTO operations (\
                         id, run_id, kind, status, request_json, result_json, \
                         attempt_count, created_at, updated_at\
                     ) VALUES (?1, ?2, ?3, 'succeeded', '{}', '{}', 1, 11, 11)",
                    rusqlite::params![operation_id, RUN_ID, kind],
                )
                .expect("the prior operation should be inserted");
        }
        drop(connection);

        let mut store = Store::open(&database.0)
            .expect("the operation schema should upgrade");
        store
            .transaction(|repositories| {
                for operation_id in &operation_ids[..3] {
                    let operation = repositories
                        .operation(*operation_id)?
                        .expect("the external operation should remain durable");
                    assert_eq!(
                        operation.reconciliation_state,
                        Some(ExternalResourceState::Unknown)
                    );
                    assert_eq!(operation.reconciliation_attempt_count, 0);
                    assert_eq!(operation.reconciliation_error, None);
                    assert_eq!(operation.reconciled_at, None);
                }
                assert_eq!(
                    repositories
                        .operation(operation_ids[3])?
                        .expect("the database mutation should remain durable")
                        .reconciliation_state,
                    None
                );
                Ok(())
            })
            .expect("the classified operations should be readable");
    }

    #[test]
    fn applied_migrations_are_append_only_and_verified_against_the_source() {
        let database = TestDatabase::new();
        drop(Store::open(&database.0).expect("the store should migrate"));

        let connection =
            Connection::open(&database.0).expect("the database should open");
        let update_error = connection
            .execute(
                "UPDATE schema_migrations SET source = 'changed' WHERE version = 1",
                [],
            )
            .expect_err("the ledger must reject updates");
        assert!(update_error.to_string().contains("append-only"));

        connection
            .execute_batch(
                "DROP TRIGGER schema_migrations_cannot_be_updated; \
                 UPDATE schema_migrations SET source = 'changed' WHERE version = 1;",
            )
            .expect("the test should simulate external corruption");
        drop(connection);

        let error = match Store::open(&database.0) {
            Ok(_) => panic!("a modified migration must be rejected"),
            Err(error) => error,
        };
        assert!(matches!(
            error,
            super::StoreError::ModifiedMigration {
                version: 1,
                ref name,
            } if name == "initial"
        ));
    }

    #[test]
    fn a_database_from_a_newer_schema_is_not_opened() {
        let database = TestDatabase::new();
        drop(Store::open(&database.0).expect("the store should migrate"));

        let connection =
            Connection::open(&database.0).expect("the database should open");
        let future = MIGRATIONS.len() as i64 + 1;
        connection
            .execute(
                "INSERT INTO schema_migrations (version, name, source) \
                 VALUES (?1, 'future', '-- future migration')",
                [future],
            )
            .expect("the test should simulate a newer Coterie version");
        drop(connection);

        let error = match Store::open(&database.0) {
            Ok(_) => panic!("a future schema must be rejected"),
            Err(error) => error,
        };
        assert!(matches!(
            error,
            super::StoreError::UnsupportedSchema {
                found,
                supported,
            }
            if found == future && supported == future - 1
        ));
    }

    #[test]
    fn every_repository_round_trips_records_across_transactions() {
        let database = TestDatabase::new();
        let mut store =
            Store::open(&database.0).expect("the store should open");
        store
            .connection
            .pragma_update(None, "foreign_keys", true)
            .expect("the schema should support foreign-key enforcement");
        let records = Records::fixture();

        store
            .transaction(|repositories| {
                repositories.insert_run(&records.run)?;
                repositories.insert_configuration_snapshot(
                    &records.run_configuration,
                )?;
                repositories.insert_project(&records.project)?;
                repositories
                    .insert_configuration_snapshot(&records.configuration)?;
                repositories.insert_agent(&records.agent)?;
                repositories.insert_session(&records.session)?;
                repositories
                    .activate_session_credential(&records.credential)?;
                repositories.insert_task_group(&records.group)?;
                repositories.insert_task(&records.dependency_task)?;
                repositories.insert_task(&records.task)?;
                repositories.insert_dependency(&records.dependency)?;
                repositories.insert_comment(&records.comment)?;
                repositories.insert_operation(&records.operation)?;
                repositories.insert_claim(&records.claim)?;
                repositories.insert_assignment(&records.assignment)?;
                repositories.insert_message(&records.message)?;
                repositories.insert_workspace(&records.workspace)?;
                repositories.insert_event(&records.event)?;
                Ok(())
            })
            .expect("the records should commit");

        drop(store);
        let mut store =
            Store::open(&database.0).expect("the durable store should reopen");

        store
            .transaction(|repositories| {
                assert_eq!(
                    repositories.run(records.run.id)?,
                    Some(records.run.clone())
                );
                assert_eq!(
                    repositories
                        .configuration_snapshot(records.run_configuration.id)?,
                    Some(records.run_configuration.clone())
                );
                assert_eq!(
                    repositories
                        .configuration_snapshot(records.configuration.id)?,
                    Some(records.configuration.clone())
                );
                assert_eq!(
                    repositories.project(records.project.id)?,
                    Some(records.project.clone())
                );
                assert_eq!(
                    repositories.agent(records.agent.id)?,
                    Some(records.agent.clone())
                );
                assert_eq!(
                    repositories.session(records.session.id)?,
                    Some(records.session.clone())
                );
                assert_eq!(
                    repositories.session_credential(records.session.id)?,
                    Some(records.credential.clone())
                );
                assert_eq!(
                    repositories.task_group(records.group.id)?,
                    Some(records.group.clone())
                );
                assert_eq!(
                    repositories.task(records.task.id)?,
                    Some(records.task.clone())
                );
                assert_eq!(
                    repositories.dependency(
                        records.dependency.task_id,
                        records.dependency.dependency_task_id,
                    )?,
                    Some(records.dependency.clone())
                );
                assert_eq!(
                    repositories.comment(records.comment.id)?,
                    Some(records.comment.clone())
                );
                assert_eq!(
                    repositories.operation(records.operation.id)?,
                    Some(records.operation.clone())
                );
                assert_eq!(
                    repositories.claim(records.claim.id)?,
                    Some(records.claim.clone())
                );
                assert_eq!(
                    repositories.assignment(records.assignment.id)?,
                    Some(records.assignment.clone())
                );
                assert_eq!(
                    repositories.message(records.message.id)?,
                    Some(records.message.clone())
                );
                assert_eq!(
                    repositories.workspace(records.workspace.assignment_id)?,
                    Some(records.workspace.clone())
                );
                assert_eq!(
                    repositories.event(records.event.id)?,
                    Some(records.event.clone())
                );
                Ok(())
            })
            .expect("the committed records should load");
    }

    #[test]
    fn inbox_acknowledgements_are_explicit_idempotent_and_monotonic() {
        let mut store = Store::open_in_memory().expect("the store should open");
        let records = Records::fixture();
        store
            .transaction(|repositories| {
                repositories.insert_run(&records.run)?;
                repositories.insert_agent(&records.agent)?;
                for sequence in 1..=3 {
                    repositories.insert_message(&MessageRecord {
                        id: MessageId::generate(),
                        sequence,
                        body: format!("Message {sequence}."),
                        ..records.message.clone()
                    })?;
                }
                Ok(())
            })
            .expect("the inbox should be populated");
        let operation_id = OperationId::generate();
        let acknowledgement = AcknowledgeMessagesMutation {
            operation_id,
            run_id: records.run.id,
            agent_id: records.agent.id,
            through: 2,
            acknowledged_at: 30,
        };

        let applied = store
            .acknowledge_messages(&acknowledgement)
            .expect("the cursor should be acknowledged");
        assert_eq!(
            applied,
            MutationOutcome::Applied(AcknowledgeMessagesResult::Acknowledged {
                acknowledged_through: 2,
                acknowledged_count: 2,
            })
        );
        assert_eq!(
            store
                .acknowledge_messages(&acknowledgement)
                .expect("the retry should replay"),
            applied.as_replayed()
        );

        let future = store
            .acknowledge_messages(&AcknowledgeMessagesMutation {
                operation_id: OperationId::generate(),
                through: 4,
                acknowledged_at: 31,
                ..acknowledgement.clone()
            })
            .expect("an unknown cursor should be a durable domain result");
        assert_eq!(
            future,
            MutationOutcome::Applied(
                AcknowledgeMessagesResult::CursorNotFound { highest_cursor: 3 }
            )
        );

        let lower = store
            .acknowledge_messages(&AcknowledgeMessagesMutation {
                operation_id: OperationId::generate(),
                through: 1,
                acknowledged_at: 31,
                ..acknowledgement
            })
            .expect("an older cursor should be an idempotent no-op");
        assert_eq!(
            lower,
            MutationOutcome::Applied(AcknowledgeMessagesResult::Acknowledged {
                acknowledged_through: 2,
                acknowledged_count: 0,
            })
        );
        store
            .transaction(|repositories| {
                let messages = repositories.messages_after(
                    records.run.id,
                    records.agent.id,
                    0,
                )?;
                assert_eq!(
                    messages
                        .iter()
                        .map(|message| message.acknowledged_at)
                        .collect::<Vec<_>>(),
                    vec![Some(30), Some(30), None]
                );
                assert_eq!(
                    repositories.next_message_sequence(
                        records.run.id,
                        records.agent.id,
                    )?,
                    4
                );
                Ok(())
            })
            .expect("the monotonic inbox should be inspectable");

        for statement in [
            "DELETE FROM messages WHERE sequence = 3",
            "UPDATE messages SET body = 'rewritten' WHERE sequence = 3",
        ] {
            assert!(matches!(
                store.transaction(|repositories| {
                    repositories.transaction.execute(statement, [])?;
                    Ok(())
                }),
                Err(super::StoreError::Database(_))
            ));
        }
        store
            .transaction(|repositories| {
                assert_eq!(
                    repositories.next_message_sequence(
                        records.run.id,
                        records.agent.id,
                    )?,
                    4
                );
                assert_eq!(
                    repositories
                        .messages_after(records.run.id, records.agent.id, 2)?
                        .first()
                        .map(|message| message.body.as_str()),
                    Some("Message 3.")
                );
                Ok(())
            })
            .expect("failed rewrites must not alter the inbox");
    }

    #[test]
    fn credentials_store_only_verifiers_and_fence_replaced_generations() {
        let mut store = Store::open_in_memory().expect("the store should open");
        let records = Records::fixture();
        store
            .transaction(|repositories| {
                repositories.insert_run(&records.run)?;
                repositories.insert_agent(&records.agent)?;
                repositories.insert_session(&records.session)?;
                repositories.activate_session_credential(&records.credential)
            })
            .expect("the credential prerequisites should commit");

        store
            .transaction(|repositories| {
                assert_eq!(
                    repositories.active_session_credential(
                        records.run.id,
                        records.agent.id,
                        records.session.id,
                    )?,
                    Some(records.credential.clone())
                );
                Ok(())
            })
            .expect("the active credential should be readable");

        let stored: Vec<u8> = store
            .connection
            .query_row(
                "SELECT token_verifier FROM session_credentials WHERE session_id = ?1",
                [records.session.id],
                |row| row.get(0),
            )
            .expect("the verifier should be stored");
        assert_eq!(stored, records.credential.token_verifier.as_bytes());
        assert_ne!(stored, token().expose_secret().as_bytes());

        let replacement_session = SessionRecord {
            id: SECOND_SESSION_ID.parse().expect("valid session ID"),
            generation: records.session.generation + 1,
            created_at: 30,
            ..records.session.clone()
        };
        let replacement_token =
            AgentToken::generate().expect("randomness should be available");
        let replacement_credential = SessionCredentialRecord {
            run_id: records.run.id,
            agent_id: records.agent.id,
            session_id: replacement_session.id,
            generation: replacement_session.generation,
            token_verifier: replacement_token.verifier(SessionScope {
                run_id: records.run.id,
                agent_id: records.agent.id,
                session_id: replacement_session.id,
                generation: replacement_session.generation,
            }),
            created_at: 30,
            revoked_at: None,
        };
        store
            .transaction(|repositories| {
                repositories.transaction.execute(
                    "UPDATE agents SET generation = ?2 WHERE id = ?1",
                    rusqlite::params![
                        records.agent.id,
                        replacement_session.generation
                    ],
                )?;
                repositories.insert_session(&replacement_session)?;
                repositories
                    .activate_session_credential(&replacement_credential)
            })
            .expect("the replacement credential should commit");

        store
            .transaction(|repositories| {
                assert_eq!(
                    repositories.active_session_credential(
                        records.run.id,
                        records.agent.id,
                        records.session.id,
                    )?,
                    None
                );
                assert_eq!(
                    repositories.active_session_credential(
                        records.run.id,
                        records.agent.id,
                        replacement_session.id,
                    )?,
                    Some(replacement_credential)
                );
                assert_eq!(
                    repositories
                        .session_credential(records.session.id)?
                        .expect(
                            "the replaced credential should remain auditable"
                        )
                        .revoked_at,
                    Some(30)
                );
                Ok(())
            })
            .expect("credential rotation should be inspectable");
    }

    #[test]
    fn provider_lifecycle_observations_update_the_agent_and_session_atomically()
    {
        let mut store = Store::open_in_memory().expect("the store should open");
        let mut records = Records::fixture();
        records.agent.state = LifecycleState::Starting;
        records.session.state = LifecycleState::Starting;
        store
            .transaction(|repositories| {
                repositories.insert_run(&records.run)?;
                repositories.insert_agent(&records.agent)?;
                repositories.insert_session(&records.session)?;
                repositories.activate_session_credential(&records.credential)
            })
            .expect("the starting session should commit");

        let running = store
            .transaction(|repositories| {
                repositories.record_session_lifecycle(
                    SessionScope {
                        run_id: records.run.id,
                        agent_id: records.agent.id,
                        session_id: records.session.id,
                        generation: records.session.generation,
                    },
                    LifecycleState::Running,
                    20,
                )
            })
            .expect("the running observation should commit");
        assert_eq!(running, SessionTransitionOutcome::Applied);

        let exited = store
            .transaction(|repositories| {
                repositories.record_session_lifecycle(
                    SessionScope {
                        run_id: records.run.id,
                        agent_id: records.agent.id,
                        session_id: records.session.id,
                        generation: records.session.generation,
                    },
                    LifecycleState::Exited,
                    21,
                )
            })
            .expect("the exit observation should commit");
        assert_eq!(exited, SessionTransitionOutcome::Applied);

        let invalid = store
            .transaction(|repositories| {
                repositories.record_session_lifecycle(
                    SessionScope {
                        run_id: records.run.id,
                        agent_id: records.agent.id,
                        session_id: records.session.id,
                        generation: records.session.generation,
                    },
                    LifecycleState::Running,
                    22,
                )
            })
            .expect_err("a terminal session must not become live again");
        assert!(matches!(
            invalid,
            super::StoreError::InvalidSessionTransition {
                session_id,
                current: LifecycleState::Exited,
                observed: LifecycleState::Running,
            } if session_id == records.session.id
        ));

        store
            .transaction(|repositories| {
                let agent = repositories
                    .agent(records.agent.id)?
                    .expect("the agent should remain durable");
                let session = repositories
                    .session(records.session.id)?
                    .expect("the session should remain durable");
                let credential = repositories
                    .session_credential(records.session.id)?
                    .expect("the credential should remain auditable");
                assert_eq!(agent.state, LifecycleState::Exited);
                assert_eq!(session.state, LifecycleState::Exited);
                assert_eq!(session.ended_at, Some(21));
                assert_eq!(credential.revoked_at, Some(21));
                Ok(())
            })
            .expect("the terminal lifecycle should be readable");
    }

    #[test]
    fn session_lifecycle_updates_are_idempotent_and_generation_fenced() {
        let mut store = Store::open_in_memory().expect("the store should open");
        let records = Records::fixture();
        store
            .transaction(|repositories| {
                repositories.insert_run(&records.run)?;
                repositories.insert_agent(&records.agent)?;
                repositories.insert_session(&records.session)?;
                repositories.activate_session_credential(&records.credential)
            })
            .expect("the current session should commit");
        let scope = SessionScope {
            run_id: records.run.id,
            agent_id: records.agent.id,
            session_id: records.session.id,
            generation: records.session.generation,
        };

        let unchanged = store
            .transaction(|repositories| {
                repositories.record_session_lifecycle(
                    scope,
                    LifecycleState::Running,
                    20,
                )
            })
            .expect("repeating the current state should succeed");
        let stale = store
            .transaction(|repositories| {
                repositories.record_session_lifecycle(
                    SessionScope {
                        generation: scope.generation - 1,
                        ..scope
                    },
                    LifecycleState::Exited,
                    21,
                )
            })
            .expect("a stale observation should be ignored");

        assert_eq!(unchanged, SessionTransitionOutcome::Unchanged);
        assert_eq!(stale, SessionTransitionOutcome::Stale);
        store
            .transaction(|repositories| {
                assert_eq!(
                    repositories
                        .session(records.session.id)?
                        .expect("the current session should exist")
                        .state,
                    LifecycleState::Running
                );
                Ok(())
            })
            .expect("the fenced state should remain readable");
    }

    #[test]
    fn replaced_generations_cannot_change_session_metadata() {
        let mut store = Store::open_in_memory().expect("store");
        let records = Records::fixture();
        let scope = SessionScope {
            run_id: records.run.id,
            agent_id: records.agent.id,
            session_id: records.session.id,
            generation: records.session.generation,
        };
        store
            .transaction(|repositories| {
                repositories.insert_run(&records.run)?;
                repositories.insert_agent(&records.agent)?;
                repositories.insert_session(&records.session)?;
                repositories.record_session_lifecycle(
                    scope,
                    LifecycleState::Exited,
                    30,
                )?;
                assert!(repositories.start_agent_generation(
                    scope.run_id,
                    scope.agent_id,
                    scope.generation,
                    scope.generation + 1
                )?);
                let before = repositories.session(scope.session_id)?;
                assert_eq!(
                    repositories.record_session_launch_observation(
                        scope,
                        "late-process",
                        31
                    )?,
                    SessionTransitionOutcome::Stale
                );
                assert_eq!(
                    repositories.record_session_reconciliation_state(
                        scope,
                        ExternalResourceState::Unknown,
                        31
                    )?,
                    SessionTransitionOutcome::Stale
                );
                assert_eq!(repositories.session(scope.session_id)?, before);
                Ok(())
            })
            .expect("stale metadata must be ignored");
    }

    #[test]
    fn assignments_reject_a_session_from_another_generation() {
        let mut store = Store::open_in_memory().expect("store");
        let mut records = Records::fixture();
        records.assignment.session_id = None;
        records.session.generation += 1;
        store
            .transaction(|repositories| {
                repositories.insert_run(&records.run)?;
                repositories.insert_project(&records.project)?;
                repositories.insert_agent(&records.agent)?;
                repositories.insert_task_group(&records.group)?;
                repositories.insert_task(&records.task)?;
                repositories.insert_operation(&records.operation)?;
                repositories.insert_claim(&records.claim)?;
                repositories.insert_assignment(&records.assignment)?;
                let wrong_workspace = WorkspaceRecord {
                    generation: records.workspace.generation + 1,
                    ..records.workspace.clone()
                };
                assert!(
                    repositories.insert_workspace(&wrong_workspace).is_err()
                );
                assert!(
                    repositories.workspace(records.assignment.id)?.is_none()
                );
                repositories.insert_session(&records.session)?;
                assert!(
                    repositories
                        .associate_assignment_session(
                            records.assignment.id,
                            records.session.id
                        )
                        .is_err()
                );
                assert_eq!(
                    repositories.assignment(records.assignment.id)?,
                    Some(records.assignment.clone())
                );
                Ok(())
            })
            .expect("association must preserve ownership");
    }

    #[test]
    fn replacement_generations_must_increase() {
        let mut store = Store::open_in_memory().expect("store");
        let mut records = Records::fixture();
        records.agent.state = LifecycleState::Exited;
        store
            .transaction(|repositories| {
                repositories.insert_run(&records.run)?;
                repositories.insert_agent(&records.agent)?;
                for next in
                    [records.agent.generation, records.agent.generation - 1]
                {
                    assert!(!repositories.start_agent_generation(
                        records.run.id,
                        records.agent.id,
                        records.agent.generation,
                        next
                    )?);
                }
                Ok(())
            })
            .expect("generations must never be reused");
    }

    #[test]
    fn project_reads_reject_a_malformed_typed_identity() {
        let mut store = Store::open_in_memory().expect("the store should open");
        let records = Records::fixture();
        store
            .transaction(|repositories| {
                repositories.insert_run(&records.run)?;
                repositories.insert_project(&records.project)?;
                Ok(())
            })
            .expect("the project prerequisites should commit");
        store
            .connection
            .execute(
                "UPDATE projects SET identity_json = ?1 WHERE id = ?2",
                (
                    r#"{"kind":"directory","canonical_directory":"lossy"}"#,
                    records.project.id,
                ),
            )
            .expect("the fixture should simulate a malformed identity");

        let error = store
            .transaction(|repositories| {
                repositories.project(records.project.id)?;
                Ok(())
            })
            .expect_err("malformed durable identity must not enter the domain");

        assert!(matches!(
            error,
            super::StoreError::Database(
                rusqlite::Error::FromSqlConversionFailure(..)
            )
        ));
    }

    #[test]
    fn a_repository_error_rolls_back_the_whole_transaction() {
        let mut store = Store::open_in_memory().expect("the store should open");
        let run = Records::fixture().run;

        let error = store
            .transaction(|repositories| {
                repositories.insert_run(&run)?;
                repositories.insert_run(&run)?;
                Ok(())
            })
            .expect_err("the duplicate ID should fail");
        assert!(matches!(error, super::StoreError::Database(_)));

        store
            .transaction(|repositories| {
                assert_eq!(repositories.run(run.id)?, None);
                Ok(())
            })
            .expect("the rolled-back store should remain usable");
    }

    struct Records {
        run: RunRecord,
        run_configuration: ConfigurationSnapshotRecord,
        configuration: ConfigurationSnapshotRecord,
        project: ProjectRecord,
        agent: AgentRecord,
        session: SessionRecord,
        credential: SessionCredentialRecord,
        group: TaskGroupRecord,
        task: TaskRecord,
        dependency_task: TaskRecord,
        dependency: DependencyRecord,
        comment: CommentRecord,
        operation: OperationRecord,
        claim: ClaimRecord,
        assignment: AssignmentRecord,
        message: MessageRecord,
        workspace: WorkspaceRecord,
        event: EventRecord,
    }

    struct TestDatabase(PathBuf);

    impl TestDatabase {
        fn new() -> Self {
            let path = std::env::temp_dir()
                .join(format!("coterie-test-{}.sqlite", RunId::generate()));
            Self(path)
        }
    }

    impl Drop for TestDatabase {
        fn drop(&mut self) {
            if self.0.exists() {
                std::fs::remove_file(&self.0)
                    .expect("the test database should be removable");
            }
        }
    }

    fn insert_claim_prerequisites(store: &mut Store, records: &Records) {
        store
            .transaction(|repositories| {
                repositories.insert_run(&records.run)?;
                repositories.insert_project(&records.project)?;
                repositories.insert_agent(&records.agent)?;
                repositories.insert_task_group(&records.group)?;
                repositories.insert_task(&records.dependency_task)?;
                repositories.insert_task(&records.task)?;
                repositories.insert_dependency(&records.dependency)?;
                Ok(())
            })
            .expect("the claim prerequisites should commit");
    }

    fn claim_mutation(records: &Records) -> ClaimTaskMutation {
        ClaimTaskMutation {
            operation_id: records.operation.id,
            run_id: records.run.id,
            actor_agent_id: Some(records.agent.id),
            task_id: records.task.id,
            agent_id: records.agent.id,
            assignment_id: records.assignment.id,
            claimed_at: records.claim.claimed_at,
        }
    }

    fn transition_mutation(
        records: &Records,
        transition: TaskTransition,
    ) -> TaskTransitionMutation {
        TaskTransitionMutation {
            operation_id: SECOND_OPERATION_ID
                .parse()
                .expect("the operation ID should parse"),
            run_id: records.run.id,
            actor_agent_id: Some(records.agent.id),
            task_id: records.task.id,
            transition,
            result: Some(json!({"summary": "finished"})),
            summary: Some("Finished the task.".to_owned()),
            transitioned_at: records.claim.claimed_at + 1,
        }
    }

    impl Records {
        fn fixture() -> Self {
            let run_id = RUN_ID.parse::<RunId>().expect("valid run ID");
            let project_id =
                PROJECT_ID.parse::<ProjectId>().expect("valid project ID");
            let agent_id = AGENT_ID.parse::<AgentId>().expect("valid agent ID");
            let session_id =
                SESSION_ID.parse::<SessionId>().expect("valid session ID");
            let task_id = TASK_ID.parse::<TaskId>().expect("valid task ID");
            let dependency_task_id = DEPENDENCY_TASK_ID
                .parse::<TaskId>()
                .expect("valid dependency task ID");
            let operation_id = OPERATION_ID
                .parse::<OperationId>()
                .expect("valid operation ID");
            let assignment_id = ASSIGNMENT_ID
                .parse::<AssignmentId>()
                .expect("valid assignment ID");
            let session_scope = SessionScope {
                run_id,
                agent_id,
                session_id,
                generation: 2,
            };

            Self {
                run: RunRecord {
                    id: run_id,
                    status: "active".to_owned(),
                    created_at: 10,
                    stopped_at: None,
                },
                run_configuration: ConfigurationSnapshotRecord {
                    id: 2,
                    run_id,
                    project_id: None,
                    scope: "run".to_owned(),
                    schema_version: 1,
                    fingerprint: "sha256:run-fixture".to_owned(),
                    document: json!({"archetype": "builtin:standard@1"}),
                    created_at: 10,
                },
                configuration: ConfigurationSnapshotRecord {
                    id: 1,
                    run_id,
                    project_id: Some(project_id),
                    scope: "project".to_owned(),
                    schema_version: 1,
                    fingerprint: "sha256:fixture".to_owned(),
                    document: json!({"archetype": "builtin:standard@1"}),
                    created_at: 12,
                },
                project: ProjectRecord {
                    id: project_id,
                    run_id,
                    alias: "primary".to_owned(),
                    original_path: PathBuf::from(OsString::from_vec(
                        b"/tmp/project-\xff".to_vec(),
                    )),
                    canonical_path: PathBuf::from("/tmp/project"),
                    identity: ProjectIdentity::Directory {
                        canonical_directory: PathBuf::from("/tmp/project"),
                    },
                    is_primary: true,
                    attached_at: 11,
                },
                agent: AgentRecord {
                    id: agent_id,
                    run_id,
                    role: "worker".to_owned(),
                    generation: 2,
                    state: LifecycleState::Running,
                    created_at: 13,
                },
                session: SessionRecord {
                    id: session_id,
                    run_id,
                    agent_id,
                    generation: 2,
                    provider: "codex".to_owned(),
                    provider_session_id: Some("codex-session".to_owned()),
                    reconciliation_state: ExternalResourceState::Observed,
                    state: LifecycleState::Running,
                    transcript_path: PathBuf::from("transcripts/session.jsonl"),
                    created_at: 14,
                    ended_at: None,
                    reconciled_at: Some(14),
                    process_owner: SessionProcessOwner::Supervisor,
                },
                credential: SessionCredentialRecord {
                    run_id,
                    agent_id,
                    session_id,
                    generation: 2,
                    token_verifier: token().verifier(session_scope),
                    created_at: 14,
                    revoked_at: None,
                },
                group: TaskGroupRecord {
                    id: 1,
                    run_id,
                    name: Some("request".to_owned()),
                    created_at: 15,
                },
                dependency_task: TaskRecord {
                    id: dependency_task_id,
                    run_id,
                    project_id,
                    group_id: Some(1),
                    title: "Dependency".to_owned(),
                    description: "Prepare the input.".to_owned(),
                    status: TaskStatus::Closed,
                    result: Some(json!({"summary": "ready"})),
                    created_at: 16,
                    updated_at: 17,
                },
                task: TaskRecord {
                    id: task_id,
                    run_id,
                    project_id,
                    group_id: Some(1),
                    title: "Implement".to_owned(),
                    description: "Implement the requested change.".to_owned(),
                    status: TaskStatus::Open,
                    result: None,
                    created_at: 18,
                    updated_at: 18,
                },
                dependency: DependencyRecord {
                    run_id,
                    task_id,
                    dependency_task_id,
                    created_at: 19,
                },
                comment: CommentRecord {
                    id: 1,
                    run_id,
                    task_id,
                    author_agent_id: Some(agent_id),
                    body: "A durable comment.".to_owned(),
                    created_at: 20,
                },
                operation: OperationRecord {
                    id: operation_id,
                    run_id,
                    kind: "task.claim".to_owned(),
                    actor_agent_id: Some(agent_id),
                    status: "pending".to_owned(),
                    request: json!({"task_id": TASK_ID}),
                    result: None,
                    attempt_count: 0,
                    reconciliation_state: None,
                    reconciliation_attempt_count: 0,
                    reconciliation_error: None,
                    reconciled_at: None,
                    created_at: 21,
                    updated_at: 21,
                },
                claim: ClaimRecord {
                    id: 1,
                    run_id,
                    task_id,
                    agent_id,
                    operation_id,
                    state: "active".to_owned(),
                    claimed_at: 22,
                    released_at: None,
                },
                assignment: AssignmentRecord {
                    id: assignment_id,
                    run_id,
                    task_id,
                    agent_id,
                    session_id: Some(session_id),
                    claim_id: 1,
                    generation: 2,
                    state: "active".to_owned(),
                    summary: None,
                    created_at: 23,
                    completed_at: None,
                },
                message: MessageRecord {
                    id: MESSAGE_ID
                        .parse::<MessageId>()
                        .expect("valid message ID"),
                    run_id,
                    sender_agent_id: None,
                    recipient_agent_id: agent_id,
                    sequence: 1,
                    body: "Check your inbox.".to_owned(),
                    created_at: 24,
                    acknowledged_at: None,
                },
                workspace: WorkspaceRecord {
                    generation: 2,
                    assignment_id,
                    run_id,
                    project_id,
                    kind: "worktree".to_owned(),
                    path: PathBuf::from("workspaces/assignment"),
                    state: ExternalResourceState::Desired,
                    base_commit: Some(
                        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
                    ),
                    result_commit: None,
                    target_commit: None,
                    created_at: 25,
                    reconciled_at: None,
                },
                event: EventRecord {
                    id: EVENT_ID.parse::<EventId>().expect("valid event ID"),
                    run_id,
                    sequence: 1,
                    event_type: "task.claimed".to_owned(),
                    actor: agent_id.to_string(),
                    subject: task_id.to_string(),
                    project_id: Some(project_id),
                    agent_id: Some(agent_id),
                    task_id: Some(task_id),
                    operation_id: Some(operation_id),
                    correlation_id: None,
                    causation_id: None,
                    payload: json!({
                        "schema_version": 1,
                        "data": {"assignment_id": ASSIGNMENT_ID},
                    }),
                    summary: "Task claimed.".to_owned(),
                    created_at: 26,
                },
            }
        }
    }

    fn token() -> AgentToken {
        TOKEN.parse().expect("the fixture token should parse")
    }
}

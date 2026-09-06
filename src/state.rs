//! SQLite migrations, transactions, operations, messages, and events.

#[cfg(test)]
use std::collections::BTreeSet;
use std::ffi::OsString;
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

use crate::id::{
    AgentId, AssignmentId, EventId, MessageId, OperationId, ProjectId, RunId,
    SessionId, TaskId,
};
use crate::project::ProjectIdentity;
use crate::tasks::{TaskReadiness, TaskStatus, TaskTransition};

const BUSY_TIMEOUT: Duration = Duration::from_secs(2);

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

    /// Related task, claim, and assignment records violate a lifecycle invariant.
    #[error("task `{id}` has corrupt lifecycle state: {reason}")]
    CorruptTaskState { id: TaskId, reason: String },
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
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ProjectRecord {
    pub(crate) id: ProjectId,
    pub(crate) run_id: RunId,
    pub(crate) alias: String,
    pub(crate) original_path: PathBuf,
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
    pub(crate) state: String,
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
    pub(crate) state: String,
    pub(crate) transcript_path: PathBuf,
    pub(crate) created_at: i64,
    pub(crate) ended_at: Option<i64>,
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

/// Durable ownership and integration metadata for an assignment workspace.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct WorkspaceRecord {
    pub(crate) assignment_id: AssignmentId,
    pub(crate) run_id: RunId,
    pub(crate) project_id: ProjectId,
    pub(crate) kind: String,
    pub(crate) path: PathBuf,
    pub(crate) state: String,
    pub(crate) base_commit: Option<String>,
    pub(crate) result_commit: Option<String>,
    pub(crate) target_commit: Option<String>,
    pub(crate) created_at: i64,
}

/// One immutable entry in a run's typed event stream.
#[derive(Clone, Debug, Eq, PartialEq)]
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
    fn open_in_memory() -> Result<Self, StoreError> {
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
            transaction.execute_batch(migration.sql)?;
            transaction.execute(
                "INSERT INTO schema_migrations (version, name, source) VALUES (?1, ?2, ?3)",
                (migration.version, migration.name, migration.sql),
            )?;
            transaction.commit()?;
        }

        Ok(())
    }

    /// Commits all repository changes together, or rolls all of them back.
    pub(crate) fn transaction<T>(
        &mut self,
        operation: impl FnOnce(&Repositories<'_, '_>) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        let transaction = self.connection.transaction()?;
        let result = operation(&Repositories {
            transaction: &transaction,
        })?;
        transaction.commit()?;
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
        let transaction = self.connection.transaction()?;
        let repositories = Repositories {
            transaction: &transaction,
        };

        if let Some(existing) = repositories.operation(mutation.id)? {
            if existing.run_id != mutation.run_id
                || existing.kind != mutation.kind
                || existing.actor_agent_id != mutation.actor_agent_id
                || existing.request != mutation.request
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
            created_at: mutation.created_at,
            updated_at: mutation.created_at,
        })?;

        let result = apply(&repositories)?;
        let encoded = serde_json::to_string(&result)?;
        repositories.transaction.execute(
            "UPDATE operations SET status = 'succeeded', result_json = ?2, updated_at = ?3 \
             WHERE id = ?1",
            params![mutation.id, encoded, mutation.created_at],
        )?;
        transaction.commit()?;
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
            repositories.compare_and_set_claim(claim)
        })
    }

    /// Applies a non-claim task transition and replays its durable result.
    pub(crate) fn transition_task(
        &mut self,
        transition: &TaskTransitionMutation,
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

        self.mutate(&mutation, |repositories| {
            repositories.apply_task_transition(transition)
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
                agent.state,
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
                        state: row.get(4)?,
                        created_at: row.get(5)?,
                    })
                },
            )
            .optional()?)
    }

    pub(crate) fn insert_session(
        &self,
        session: &SessionRecord,
    ) -> Result<(), StoreError> {
        self.transaction.execute(
            "INSERT INTO sessions (\
                 id, run_id, agent_id, generation, provider, state, transcript_path, \
                 created_at, ended_at\
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                session.id,
                session.run_id,
                session.agent_id,
                session.generation,
                session.provider,
                session.state,
                path_bytes(&session.transcript_path),
                session.created_at,
                session.ended_at,
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
                "SELECT id, run_id, agent_id, generation, provider, state, transcript_path, \
                        created_at, ended_at \
                 FROM sessions WHERE id = ?1",
                [id],
                |row| {
                    Ok(SessionRecord {
                        id: row.get(0)?,
                        run_id: row.get(1)?,
                        agent_id: row.get(2)?,
                        generation: row.get(3)?,
                        provider: row.get(4)?,
                        state: row.get(5)?,
                        transcript_path: decode_path(row, 6)?,
                        created_at: row.get(7)?,
                        ended_at: row.get(8)?,
                    })
                },
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
                 attempt_count, created_at, updated_at\
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                operation.id,
                operation.run_id,
                operation.kind,
                operation.actor_agent_id,
                operation.status,
                request,
                result,
                operation.attempt_count,
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
                        attempt_count, created_at, updated_at \
                 FROM operations WHERE id = ?1",
                [id],
                |row| {
                    Ok(OperationRecord {
                        id: row.get(0)?,
                        run_id: row.get(1)?,
                        kind: row.get(2)?,
                        actor_agent_id: row.get(3)?,
                        status: row.get(4)?,
                        request: decode_json(row, 5)?,
                        result: decode_optional_json(row, 6)?,
                        attempt_count: row.get(7)?,
                        created_at: row.get(8)?,
                        updated_at: row.get(9)?,
                    })
                },
            )
            .optional()?)
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

    fn compare_and_set_claim(
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

    pub(crate) fn insert_workspace(
        &self,
        workspace: &WorkspaceRecord,
    ) -> Result<(), StoreError> {
        self.transaction.execute(
            "INSERT INTO workspaces (\
                 assignment_id, run_id, project_id, kind, path, state, base_commit, \
                 result_commit, target_commit, created_at\
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                workspace.assignment_id,
                workspace.run_id,
                workspace.project_id,
                workspace.kind,
                path_bytes(&workspace.path),
                workspace.state,
                workspace.base_commit,
                workspace.result_commit,
                workspace.target_commit,
                workspace.created_at,
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
                        result_commit, target_commit, created_at \
                 FROM workspaces WHERE assignment_id = ?1",
                [assignment_id],
                |row| {
                    Ok(WorkspaceRecord {
                        assignment_id: row.get(0)?,
                        run_id: row.get(1)?,
                        project_id: row.get(2)?,
                        kind: row.get(3)?,
                        path: decode_path(row, 4)?,
                        state: row.get(5)?,
                        base_commit: row.get(6)?,
                        result_commit: row.get(7)?,
                        target_commit: row.get(8)?,
                        created_at: row.get(9)?,
                    })
                },
            )
            .optional()?)
    }

    pub(crate) fn insert_event(
        &self,
        event: &EventRecord,
    ) -> Result<(), StoreError> {
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
        AgentRecord, AssignmentRecord, BUSY_TIMEOUT, ClaimRecord,
        ClaimRejection, ClaimTaskMutation, ClaimTaskResult, CommentRecord,
        ConfigurationSnapshotRecord, DependencyRecord, EventRecord, MIGRATIONS,
        MessageRecord, Mutation, MutationOutcome, OperationRecord,
        ProjectRecord, RunRecord, SessionRecord, Store, TaskGroupRecord,
        TaskRecord, TaskTransitionMutation, TaskTransitionRejection,
        TaskTransitionResult, WorkspaceRecord,
    };
    use crate::id::{
        AgentId, AssignmentId, EventId, MessageId, OperationId, ProjectId,
        RunId, SessionId, TaskId,
    };
    use crate::project::ProjectIdentity;
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
            "schema_migrations",
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

        assert_eq!(applied, 2);
    }

    #[test]
    fn a_version_one_database_upgrades_through_the_forward_migration() {
        let database = TestDatabase::new();
        let connection = Connection::open(&database.0)
            .expect("the version-one database should open");
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
        connection
            .execute_batch(MIGRATIONS[0].sql)
            .expect("the version-one schema should be created");
        connection
            .execute(
                "INSERT INTO schema_migrations (version, name, source) VALUES (1, ?1, ?2)",
                (MIGRATIONS[0].name, MIGRATIONS[0].sql),
            )
            .expect("the version-one migration should be recorded");
        drop(connection);

        let store =
            Store::open(&database.0).expect("the database should upgrade");
        let applied = store
            .connection
            .query_row("SELECT count(*) FROM schema_migrations", [], |row| {
                row.get::<_, i64>(0)
            })
            .expect("the migration ledger should be readable");
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

        assert_eq!(applied, 2);
        assert_eq!(claim_indexes, 3);
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
        connection
            .execute(
                "INSERT INTO schema_migrations (version, name, source) \
                 VALUES (3, 'future', '-- future migration')",
                [],
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
                found: 3,
                supported: 2,
            }
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
                    state: "running".to_owned(),
                    created_at: 13,
                },
                session: SessionRecord {
                    id: session_id,
                    run_id,
                    agent_id,
                    generation: 2,
                    provider: "codex".to_owned(),
                    state: "running".to_owned(),
                    transcript_path: PathBuf::from("transcripts/session.jsonl"),
                    created_at: 14,
                    ended_at: None,
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
                    assignment_id,
                    run_id,
                    project_id,
                    kind: "worktree".to_owned(),
                    path: PathBuf::from("workspaces/assignment"),
                    state: "desired".to_owned(),
                    base_commit: Some(
                        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
                    ),
                    result_commit: None,
                    target_commit: None,
                    created_at: 25,
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
                    payload: json!({"assignment_id": ASSIGNMENT_ID}),
                    summary: "Task claimed.".to_owned(),
                    created_at: 26,
                },
            }
        }
    }
}

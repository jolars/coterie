//! SQLite migrations, transactions, operations, messages, and events.

#[cfg(test)]
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

use rusqlite::types::Type;
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde_json::Value as JsonValue;
use thiserror::Error;

use crate::id::{
    AgentId, AssignmentId, EventId, MessageId, OperationId, ProjectId, RunId,
    SessionId, TaskId,
};
use crate::tasks::TaskStatus;

const MIGRATIONS: &[Migration] = &[Migration {
    version: 1,
    name: "initial",
    sql: include_str!("state/migrations/0001_initial.sql"),
}];

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
    pub(crate) identity: JsonValue,
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
        let mut store = Self { connection };
        store.migrate()?;
        Ok(store)
    }

    #[cfg(test)]
    fn open_in_memory() -> Result<Self, StoreError> {
        let connection = Connection::open_in_memory()?;
        let mut store = Self { connection };
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

fn decode_json(
    row: &rusqlite::Row<'_>,
    index: usize,
) -> rusqlite::Result<JsonValue> {
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
    use std::collections::BTreeSet;
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;
    use std::path::PathBuf;

    use rusqlite::Connection;
    use serde_json::json;

    use super::{
        AgentRecord, AssignmentRecord, ClaimRecord, CommentRecord,
        ConfigurationSnapshotRecord, DependencyRecord, EventRecord,
        MessageRecord, OperationRecord, ProjectRecord, RunRecord,
        SessionRecord, Store, TaskGroupRecord, TaskRecord, WorkspaceRecord,
    };
    use crate::id::{
        AgentId, AssignmentId, EventId, MessageId, OperationId, ProjectId,
        RunId, SessionId, TaskId,
    };
    use crate::tasks::TaskStatus;

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

        assert_eq!(applied, 1);
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
                 VALUES (2, 'future', '-- future migration')",
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
                found: 2,
                supported: 1,
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
                    identity: json!({"kind": "directory", "key": "fixture"}),
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

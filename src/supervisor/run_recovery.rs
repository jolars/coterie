//! Operator selection of retained runs, independent of disposable active indexes.

use super::*;
use crate::protocol::run_recovery::{
    RetainedRun, RetainedRuns, RunRecoveryReport,
};

#[derive(Clone, Debug)]
pub(super) struct Intent {
    pub(super) operation_id: OperationId,
    pub(super) reason: String,
}

impl Intent {
    fn mutation(
        &self,
        run_id: RunId,
    ) -> Result<(Mutation, String), SupervisorError> {
        use sha2::{Digest, Sha256};
        if self.reason.trim().is_empty() {
            return Err(invalid_argument(
                "run recovery requires a nonempty --reason",
            )
            .into());
        }
        let request = json!({"reason": self.reason});
        let fingerprint = Sha256::digest(serde_json::to_vec(&request)?)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        Ok((
            Mutation {
                id: self.operation_id,
                run_id,
                kind: "run.recover".to_owned(),
                actor_agent_id: None,
                request: json!({"reason": crate::redaction::text(&self.reason)}),
                created_at: unix_timestamp()?,
            },
            fingerprint,
        ))
    }

    pub(super) fn replay(
        &self,
        store: &mut Store,
        run_id: RunId,
    ) -> Result<Option<RunRecoveryReport>, SupervisorError> {
        let (mutation, fingerprint) = self.mutation(run_id)?;
        if store
            .transaction(|r| r.operation(self.operation_id))?
            .is_none()
        {
            return Ok(None);
        }
        Ok(Some(mutation_value(store.mutate_with_fingerprint(
            &mutation,
            Some(&fingerprint),
            |_| unreachable!("the immutable operation exists"),
        )?)))
    }
}

fn require_operator_environment() -> Result<(), SupervisorError> {
    if [
        "COTERIE_AGENT_ID",
        "COTERIE_SESSION_ID",
        "COTERIE_TOKEN",
        "COTERIE_RUN_ID",
        "COTERIE_PROJECT_ID",
        "COTERIE_PRIMARY_PROJECT_ROOT",
        "COTERIE_SOCKET",
    ]
    .iter()
    .any(|name| env::var_os(name).is_some())
    {
        return Err(RpcFailure::new(RpcFailureCode::PermissionDenied,
            "retained-run discovery and stopped-run recovery require the operator channel; ask the operator to use `coterie run list` and `coterie run recover <run-id> --reason TEXT` outside the agent session").into());
    }
    Ok(())
}

pub(super) async fn run(
    command: crate::cli::RunCommand,
    json_output: bool,
    overrides: &crate::cli::config::Overrides,
) -> Result<crate::cli::ExitCategory, SupervisorError> {
    let operation = match &command {
        crate::cli::RunCommand::Recover(args) => Some(
            args.mutation
                .operation_id
                .unwrap_or_else(OperationId::generate),
        ),
        crate::cli::RunCommand::List => None,
    };
    let result = async {
        require_operator_environment()?;
        let project = discover_current_project()?;
        let directories = CoterieDirectories::from_environment()?;
        let value = match command {
            crate::cli::RunCommand::List => {
                serde_json::to_value(list(&project, &directories)?)?
            }
            crate::cli::RunCommand::Recover(args) => {
                let intent = Intent {
                    operation_id: operation.expect("recovery allocates an ID"),
                    reason: args.reason,
                };
                serde_json::to_value(
                    recover(
                        &project,
                        &directories,
                        args.run_id,
                        &intent,
                        overrides,
                    )
                    .await?,
                )?
            }
        };
        render_data(json_output, operation, &value)
    }
    .await;
    result.map_err(|error| command_error(error, operation))
}

fn list(
    project: &DiscoveredProject,
    directories: &CoterieDirectories,
) -> Result<RetainedRuns, SupervisorError> {
    for path in [&directories.state, &directories.runs] {
        if matches!(path.symlink_metadata(), Err(error) if error.kind() == io::ErrorKind::NotFound)
        {
            return Ok(RetainedRuns { runs: Vec::new() });
        }
        crate::private_fs::check_directory(path).map_err(|source| {
            SupervisorError::StateFileIo {
                action: "inspect retained runs in",
                path: path.clone(),
                source,
            }
        })?;
    }
    let entries = fs::read_dir(&directories.runs).map_err(|source| {
        SupervisorError::StateFileIo {
            action: "list",
            path: directories.runs.clone(),
            source,
        }
    })?;
    let mut runs = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| SupervisorError::StateFileIo {
            action: "list",
            path: directories.runs.clone(),
            source,
        })?;
        let Some(run_id) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<RunId>().ok())
        else {
            continue;
        };
        let mut store = open_configuration_store(directories, run_id)?;
        store.has_pending_migrations()?;
        let retained = store.transaction(|r| {
            let projects = r.projects(run_id)?;
            if !projects
                .iter()
                .any(|stored| stored.identity == project.identity)
            {
                return Ok(None);
            }
            let primary = projects
                .iter()
                .find(|stored| stored.is_primary)
                .ok_or(StoreError::RunNotActive { id: run_id })?;
            let run = r
                .run(run_id)?
                .ok_or(StoreError::RunNotActive { id: run_id })?;
            Ok(Some(RetainedRun {
                run_id,
                status: run.status,
                primary_root: primary
                    .canonical_path
                    .to_string_lossy()
                    .into_owned(),
                primary_root_bytes: primary
                    .canonical_path
                    .as_os_str()
                    .as_bytes()
                    .to_vec(),
                created_at: run.created_at,
                stopped_at: run.stopped_at,
                tasks: task_counts(&r.tasks(run_id)?),
            }))
        })?;
        if let Some(run) = retained {
            runs.push(run);
        }
    }
    runs.sort_by_key(|run| run.run_id);
    Ok(RetainedRuns { runs })
}

fn selected_primary(
    store: &mut Store,
    project: &DiscoveredProject,
    run_id: RunId,
) -> Result<ProjectRecord, SupervisorError> {
    let projects = store.transaction(|r| r.projects(run_id))?;
    if !projects.iter().any(|stored| {
        stored.identity == project.identity
            && stored.canonical_path == project.canonical_path
    }) {
        return Err(conflict(format!("run {run_id} is not attached to this project; use `coterie run list` from one of its project roots")).into());
    }
    projects
        .into_iter()
        .find(|stored| stored.is_primary)
        .ok_or_else(|| StoreError::RunNotActive { id: run_id }.into())
}

async fn recover(
    project: &DiscoveredProject,
    directories: &CoterieDirectories,
    run_id: RunId,
    intent: &Intent,
    overrides: &crate::cli::config::Overrides,
) -> Result<RunRecoveryReport, SupervisorError> {
    let mut store = open_configuration_store(directories, run_id)?;
    let pending_migrations = store.has_pending_migrations()?;
    let primary = selected_primary(&mut store, project, run_id)?;
    let selected = DiscoveredProject::discover(&primary.canonical_path)?;
    if selected.identity != primary.identity {
        return Err(SupervisorError::RunStateMismatch {
            run_id,
            project_id: primary.id,
        });
    }
    let entry = ActiveRunEntry::new(run_id, primary.id, primary.identity);
    let socket = checked_socket_path(directories, run_id)?;
    if !pending_migrations {
        if let Some(result) = intent.replay(&mut store, run_id)? {
            if store.transaction(|r| {
                Ok(r.run(run_id)?.is_some_and(|run| run.status == "stopped"))
            })? {
                return Ok(result);
            }
            match SupervisorClient::connect_operator_at(&socket, &entry).await {
                Ok(_) => return Ok(result),
                Err(error) if error.is_transient_connection_failure() => {}
                Err(error) => return Err(error),
            }
        } else {
            store.transaction(|r| {
                r.stopped_run_preflight(run_id, intent.operation_id)
            })?;
        }
        let current = load_configuration_with_overrides(&selected, overrides)?;
        ensure_configuration_compatible(
            run_id,
            &store.transaction(|r| r.run_configuration(run_id))?,
            &current,
        )?;
    }
    // Never let the generic startup loop substitute a newly generated run ID.
    if let Some(indexed) =
        ActiveRunIndex::new(directories).lookup(&selected.identity)?
        && indexed != entry
    {
        return Err(conflict(format!("project belongs to replacement run {}; explicitly stop it before recovering {run_id}", indexed.run_id)).into());
    }
    drop(store);
    let mut child = spawn_supervisor(
        &entry,
        &selected.canonical_path,
        overrides,
        None,
        Some(intent),
    )?;
    let deadline = Instant::now() + STARTUP_TIMEOUT;
    let result = loop {
        match SupervisorClient::connect_operator_at(&socket, &entry).await {
            Ok(_) => {
                let mut store = open_configuration_store(directories, run_id)?;
                if let Some(result) = intent.replay(&mut store, run_id)? {
                    break Ok(result);
                }
                break Err(conflict("run is already active; reconnect with `coterie` instead of stopped-run recovery").into());
            }
            Err(error) if error.is_transient_connection_failure() => {}
            Err(error) => break Err(error),
        }
        let status = child
            .child
            .try_wait()
            .map_err(SupervisorError::ChildStatus)?;
        if status.is_some_and(|status| status.success())
            && Instant::now() < deadline
        {
            // A retiring supervisor can still hold its lease after removing the socket.
            sleep(STARTUP_RETRY_INTERVAL).await;
            child = spawn_supervisor(
                &entry,
                &selected.canonical_path,
                overrides,
                None,
                Some(intent),
            )?;
        } else if status.is_some() || Instant::now() >= deadline {
            if status.is_some()
                && let Some(error) = child.recovery_failure()
            {
                break Err(error);
            }
            break Err(SupervisorError::StartupTimeout {
                project: selected.canonical_path.clone(),
                child_status: status,
                child_error: child.error_message(),
            });
        }
        sleep(STARTUP_RETRY_INTERVAL).await;
    };
    reap_child(child.child);
    result
}

pub(super) fn open_store(
    directories: &CoterieDirectories,
    active: &ActiveRunEntry,
    project: &DiscoveredProject,
) -> Result<Store, SupervisorError> {
    let mut reader = open_configuration_store(directories, active.run_id)?;
    reader.has_pending_migrations()?;
    let primary = selected_primary(&mut reader, project, active.run_id)?;
    if primary.id != active.project_id || primary.identity != project.identity {
        return Err(SupervisorError::RunStateMismatch {
            run_id: active.run_id,
            project_id: active.project_id,
        });
    }
    drop(reader);
    Ok(Store::open(
        &directories
            .runs
            .join(active.run_id.to_string())
            .join(DATABASE_FILE),
    )?)
}

pub(super) fn reactivate<P: Provider, B: WorkspaceBackend>(
    store: &mut Store,
    run_id: RunId,
    intent: &Intent,
    sessions: &mut AgentSessionSupervisor<P>,
    workspaces: &WorkspaceSupervisor<B>,
) -> Result<RunRecoveryReport, SupervisorError> {
    if let Some(result) = intent.replay(store, run_id)? {
        return Ok(result);
    }
    let retained = store.transaction(|r| {
        r.stopped_run_preflight(run_id, intent.operation_id)
    })?;
    for session in store.transaction(|r| r.sessions(run_id))? {
        let scope = SessionScope {
            run_id,
            agent_id: session.agent_id,
            session_id: session.id,
            generation: session.generation,
        };
        if !sessions.verify_recovery_exit(store, scope)? {
            return Err(conflict(format!("cannot verify exited process ownership for {}; preserve state and inspect `coterie doctor`", session.id)).into());
        }
    }
    workspaces.verify_retained(&retained).map_err(|error| {
        let mut failure = rpc_workspace_failure(error);
        failure.message = format!("retained assignment ownership could not be verified: {}; preserve the source workspace and restore its recorded identity before retrying", failure.message);
        failure
    })?;
    crate::fault::point("run.recover.validated");
    let (mutation, fingerprint) = intent.mutation(run_id)?;
    let result = mutation_value(store.mutate_with_fingerprint(
        &mutation,
        Some(&fingerprint),
        |r| r.reactivate_run(&mutation),
    )?);
    crate::fault::point("run.recover.committed");
    Ok(result)
}

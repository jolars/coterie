//! Offline shutdown recovers only the indexed run under its saved policy.

use super::*;

pub(super) async fn run(
    project: &DiscoveredProject,
    directories: &CoterieDirectories,
    entry: &ActiveRunEntry,
    operation_id: OperationId,
) -> Result<RpcResponse, SupervisorError> {
    let socket = checked_socket_path(directories, entry.run_id)?;
    match request(&socket, entry, operation_id).await {
        Ok(response) => return Ok(response),
        Err(error) if error.is_transient_connection_failure() => (),
        Err(error) => return Err(error),
    }
    let mut store = open_configuration_store(directories, entry.run_id)?;
    if !store.has_pending_migrations()?
        && let Some(result) = store.transaction(|r| {
            r.recovered_shutdown_result(entry.run_id, operation_id)
        })?
    {
        return Ok(result);
    }
    let primary = store.transaction(|repositories| {
        let caller = repositories.project(entry.project_id)?;
        if !caller.is_some_and(|stored| {
            stored.run_id == entry.run_id
                && stored.identity == project.identity
                && stored.canonical_path == project.canonical_path
        }) {
            return Err(StoreError::RunNotActive { id: entry.run_id });
        }
        repositories
            .projects(entry.run_id)?
            .into_iter()
            .find(|project| project.is_primary)
            .ok_or(StoreError::RunNotActive { id: entry.run_id })
    })?;
    drop(store);
    let primary_project = DiscoveredProject::discover(&primary.canonical_path)?;
    if primary_project.identity != primary.identity {
        return Err(SupervisorError::RunStateMismatch {
            run_id: entry.run_id,
            project_id: primary.id,
        });
    }
    let primary_entry =
        ActiveRunEntry::new(entry.run_id, primary.id, primary.identity);
    let mut child = spawn_supervisor(
        &primary_entry,
        &primary_project.canonical_path,
        &crate::cli::config::Overrides::default(),
        Some(operation_id),
        None,
    )?;
    let result = await_shutdown(
        directories,
        entry,
        &primary_project.canonical_path,
        &socket,
        operation_id,
        &mut child,
    )
    .await;
    reap_child(child.child);
    result
}

async fn request(
    socket: &Path,
    entry: &ActiveRunEntry,
    operation_id: OperationId,
) -> Result<RpcResponse, SupervisorError> {
    SupervisorClient::connect_operator_at(socket, entry)
        .await?
        .shutdown(operation_id)
        .await
}

async fn await_shutdown(
    directories: &CoterieDirectories,
    entry: &ActiveRunEntry,
    project_path: &Path,
    socket: &Path,
    operation_id: OperationId,
    child: &mut SpawnedSupervisor,
) -> Result<RpcResponse, SupervisorError> {
    let deadline = Instant::now() + STARTUP_TIMEOUT;
    loop {
        match request(socket, entry, operation_id).await {
            Ok(response) => return Ok(response),
            Err(error) if error.is_transient_connection_failure() => (),
            Err(error) => return Err(error),
        }
        // An idle run can finish shutdown before the client reconnects. Only
        // its committed operation result proves completion across that race.
        let mut store = open_configuration_store(directories, entry.run_id)?;
        if !store.has_pending_migrations()? {
            let completed = store.transaction(|repositories| {
                let run = repositories.run(entry.run_id)?;
                let operation = repositories.operation(operation_id)?;
                Ok(operation
                    .filter(|operation| {
                        run.is_some_and(|run| run.status == "stopped")
                            && operation.run_id == entry.run_id
                            && operation.kind == "run.stop"
                            && operation.status == "completed"
                    })
                    .and_then(|operation| operation.result))
            })?;
            if let Some(result) = completed {
                let response: RpcResponse = serde_json::from_value(result)?;
                if response
                    != (RpcResponse::ShuttingDown {
                        run_id: entry.run_id,
                        operation_id,
                    })
                {
                    return Err(SupervisorError::InvalidProof);
                }
                return Ok(response);
            }
        }
        if Instant::now() >= deadline {
            return Err(SupervisorError::StartupTimeout {
                project: project_path.to_owned(),
                child_status: child
                    .child
                    .try_wait()
                    .map_err(SupervisorError::ChildStatus)?,
                child_error: child.error_message(),
            });
        }
        sleep(STARTUP_RETRY_INTERVAL).await;
    }
}

pub(super) fn open_store(
    directories: &CoterieDirectories,
    active: &ActiveRunEntry,
    project: &DiscoveredProject,
) -> Result<Store, SupervisorError> {
    if ActiveRunIndex::new(directories)
        .lookup(&project.identity)?
        .as_ref()
        != Some(active)
    {
        return Err(SupervisorError::RunStateMismatch {
            run_id: active.run_id,
            project_id: active.project_id,
        });
    }
    // Validate existing state before opening a writer; shutdown never creates
    // a replacement database or adopts current configuration files.
    let reader = open_configuration_store(directories, active.run_id)?;
    reader.has_pending_migrations()?;
    drop(reader);
    let path = directories
        .runs
        .join(active.run_id.to_string())
        .join(DATABASE_FILE);
    let mut store = Store::open(&path)?;
    let valid = store.transaction(|repositories| {
        repositories.run_configuration(active.run_id)?;
        Ok(repositories.run(active.run_id)?.is_some_and(|run| {
            matches!(run.status.as_str(), "active" | "stopped")
        }) && repositories.project(active.project_id)?.is_some_and(
            |stored| {
                stored.run_id == active.run_id
                    && stored.is_primary
                    && stored.canonical_path == project.canonical_path
                    && stored.identity == project.identity
            },
        ))
    })?;
    if !valid {
        return Err(SupervisorError::RunStateMismatch {
            run_id: active.run_id,
            project_id: active.project_id,
        });
    }
    Ok(store)
}

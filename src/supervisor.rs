//! Desired-state reconciliation and process ownership.

use std::ffi::OsString;
use std::fs;
use std::io::{self, Read};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::json;
use thiserror::Error;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinSet;
use tokio::time::{Instant, sleep};

use crate::auth::{AgentToken, SessionScope};
use crate::id::{AgentId, OperationId, ProjectId, RunId, SessionId};
use crate::project::{
    ActiveRunEntry, ActiveRunIndex, CoterieDirectories, DiscoveredProject,
    LeaseAttempt, ProjectError, ProjectLease,
};
use crate::protocol::{
    ClientMessage, ConnectionChannel, FrameError, HandshakeRequest,
    HandshakeResponse, PROTOCOL_VERSION, RequestAuthentication, RpcFailure,
    RpcFailureCode, RpcRequest, RpcResponse, RpcResult, ServerMessage,
    VersionedRequest, VersionedResponse, read_frame, write_frame,
};
use crate::state::{
    Mutation, MutationOutcome, ProjectRecord, RunRecord, Store, StoreError,
};

const INTERNAL_SUPERVISOR_ARGUMENT: &str = "__supervisor";
const INTERNAL_CONNECT_ARGUMENT: &str = "__supervisor-connect";
const INTERNAL_SHUTDOWN_ARGUMENT: &str = "__supervisor-shutdown";
const STARTUP_TIMEOUT: Duration = Duration::from_secs(5);
const STARTUP_RETRY_INTERVAL: Duration = Duration::from_millis(20);
const DATABASE_FILE: &str = "state.sqlite3";
const MAXIMUM_LINUX_SOCKET_PATH_LENGTH: usize = 107;

/// Runs the foreground connector or the private supervisor process entrypoint.
pub(crate) async fn run_from_environment() -> Result<(), SupervisorError> {
    let mut arguments = std::env::args_os();
    let _executable = arguments.next();
    match arguments.next() {
        None => Ok(()),
        Some(argument) if argument == INTERNAL_CONNECT_ARGUMENT => {
            require_no_more_arguments(arguments)?;
            let project = discover_current_project()?;
            let directories = CoterieDirectories::from_environment()?;
            let mut client = connect_or_start(&project, &directories).await?;
            let response = client.ping().await?;
            if response
                != (RpcResponse::Pong {
                    run_id: client.run_id(),
                })
            {
                return Err(SupervisorError::InvalidProof);
            }
            Ok(())
        }
        Some(argument) if argument == INTERNAL_SUPERVISOR_ARGUMENT => {
            let run_id =
                parse_id_argument::<RunId>(arguments.next(), "run ID")?;
            let project_id =
                parse_id_argument::<ProjectId>(arguments.next(), "project ID")?;
            let project_path = arguments.next().map(PathBuf::from).ok_or(
                SupervisorError::InvalidInternalArguments {
                    reason: "missing project path",
                },
            )?;
            require_no_more_arguments(arguments)?;
            let project = DiscoveredProject::discover(project_path)?;
            let entry = ActiveRunEntry::new(
                run_id,
                project_id,
                project.identity.clone(),
            );
            let directories = CoterieDirectories::from_environment()?;
            serve(entry, project, directories).await
        }
        Some(argument) if argument == INTERNAL_SHUTDOWN_ARGUMENT => {
            require_no_more_arguments(arguments)?;
            let project = discover_current_project()?;
            let directories = CoterieDirectories::from_environment()?;
            directories.prepare()?;
            let entry = ActiveRunIndex::new(&directories)
                .lookup(&project.identity)?
                .ok_or(SupervisorError::NoActiveRun)?;
            let socket_path = checked_socket_path(&directories, entry.run_id)?;
            let mut client =
                SupervisorClient::connect_operator_at(&socket_path, &entry)
                    .await?;
            let operation_id = OperationId::generate();
            let response = client.shutdown(operation_id).await?;
            if response
                != (RpcResponse::ShuttingDown {
                    run_id: entry.run_id,
                    operation_id,
                })
            {
                return Err(SupervisorError::InvalidProof);
            }
            await_retirement(&directories, &project, &entry).await?;
            Ok(())
        }
        Some(_) => Ok(()),
    }
}

async fn await_retirement(
    directories: &CoterieDirectories,
    project: &DiscoveredProject,
    stopped: &ActiveRunEntry,
) -> Result<(), SupervisorError> {
    let deadline = Instant::now() + STARTUP_TIMEOUT;
    let index = ActiveRunIndex::new(directories);
    let socket_path = checked_socket_path(directories, stopped.run_id)?;
    loop {
        let indexed_run =
            index.lookup(&project.identity)?.map(|entry| entry.run_id);
        if indexed_run != Some(stopped.run_id) && !socket_path.exists() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(SupervisorError::ShutdownTimeout {
                run_id: stopped.run_id,
            });
        }
        sleep(STARTUP_RETRY_INTERVAL).await;
    }
}

fn discover_current_project() -> Result<DiscoveredProject, SupervisorError> {
    let current =
        std::env::current_dir().map_err(SupervisorError::CurrentDirectory)?;
    DiscoveredProject::discover(current).map_err(Into::into)
}

fn parse_id_argument<T>(
    argument: Option<OsString>,
    name: &'static str,
) -> Result<T, SupervisorError>
where
    T: std::str::FromStr,
{
    argument
        .and_then(|argument| argument.into_string().ok())
        .and_then(|argument| argument.parse().ok())
        .ok_or(SupervisorError::InvalidIdArgument { name })
}

fn require_no_more_arguments(
    mut arguments: impl Iterator<Item = OsString>,
) -> Result<(), SupervisorError> {
    if arguments.next().is_some() {
        Err(SupervisorError::InvalidInternalArguments {
            reason: "unexpected trailing arguments",
        })
    } else {
        Ok(())
    }
}

/// Connects to the indexed run or starts exactly one supervisor for the project.
pub(crate) async fn connect_or_start(
    project: &DiscoveredProject,
    directories: &CoterieDirectories,
) -> Result<SupervisorClient, SupervisorError> {
    directories.prepare()?;
    let index = ActiveRunIndex::new(directories);
    let indexed = index.lookup(&project.identity)?;
    if let Some(entry) = &indexed {
        match SupervisorClient::connect_operator_at(
            &checked_socket_path(directories, entry.run_id)?,
            entry,
        )
        .await
        {
            Ok(client) => return Ok(client),
            Err(error) if error.is_transient_connection_failure() => {}
            Err(error) => return Err(error),
        }
    }

    let candidate = indexed.unwrap_or_else(|| {
        ActiveRunEntry::new(
            RunId::generate(),
            ProjectId::generate(),
            project.identity.clone(),
        )
    });
    let child = spawn_supervisor(&candidate, &project.canonical_path)?;
    await_startup(project, directories, child).await
}

fn spawn_supervisor(
    entry: &ActiveRunEntry,
    project_path: &Path,
) -> Result<SpawnedSupervisor, SupervisorError> {
    let executable =
        std::env::current_exe().map_err(SupervisorError::CurrentExecutable)?;
    let mut child = Command::new(&executable)
        .arg(INTERNAL_SUPERVISOR_ARGUMENT)
        .arg(entry.run_id.to_string())
        .arg(entry.project_id.to_string())
        .arg(project_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .map_err(|source| SupervisorError::Spawn { executable, source })?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or(SupervisorError::MissingChildStderr)?;
    let error_output = Arc::new(Mutex::new(Vec::new()));
    let reader_output = Arc::clone(&error_output);
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        if stderr.read_to_end(&mut bytes).is_ok()
            && let Ok(mut output) = reader_output.lock()
        {
            *output = bytes;
        }
    });
    Ok(SpawnedSupervisor {
        child,
        error_output,
    })
}

async fn await_startup(
    project: &DiscoveredProject,
    directories: &CoterieDirectories,
    mut supervisor: SpawnedSupervisor,
) -> Result<SupervisorClient, SupervisorError> {
    let deadline = Instant::now() + STARTUP_TIMEOUT;
    let index = ActiveRunIndex::new(directories);
    let mut child_status = None;
    loop {
        if let Some(entry) = index.lookup(&project.identity)? {
            match SupervisorClient::connect_operator_at(
                &checked_socket_path(directories, entry.run_id)?,
                &entry,
            )
            .await
            {
                Ok(client) => {
                    reap_child(supervisor.child);
                    return Ok(client);
                }
                Err(error) if error.is_transient_connection_failure() => {}
                Err(error) => {
                    reap_child(supervisor.child);
                    return Err(error);
                }
            }
        }
        if child_status.is_none() {
            child_status = supervisor
                .child
                .try_wait()
                .map_err(SupervisorError::ChildStatus)?;
        }
        if Instant::now() >= deadline {
            let child_error = supervisor.error_message();
            reap_child(supervisor.child);
            return Err(SupervisorError::StartupTimeout {
                project: project.canonical_path.clone(),
                child_status,
                child_error,
            });
        }
        sleep(STARTUP_RETRY_INTERVAL).await;
    }
}

struct SpawnedSupervisor {
    child: Child,
    error_output: Arc<Mutex<Vec<u8>>>,
}

impl SpawnedSupervisor {
    fn error_message(&self) -> Option<String> {
        let output = self.error_output.lock().ok()?;
        let message = String::from_utf8_lossy(&output).trim().to_owned();
        (!message.is_empty()).then_some(message)
    }
}

fn reap_child(mut child: Child) {
    std::thread::spawn(move || {
        let _status = child.wait();
    });
}

async fn serve(
    active: ActiveRunEntry,
    project: DiscoveredProject,
    directories: CoterieDirectories,
) -> Result<(), SupervisorError> {
    directories.prepare()?;
    let lease = match ProjectLease::try_acquire(
        &directories,
        &project.identity,
        active.run_id,
    )? {
        LeaseAttempt::Acquired(lease) => lease,
        LeaseAttempt::Held => return Ok(()),
    };
    let run_directories = directories.prepare_run(active.run_id)?;
    let mut store =
        initialize_store(&run_directories.state, &active, &project)?;
    let socket_path = checked_socket_path(&directories, active.run_id)?;
    remove_stale_socket(&socket_path)?;
    let listener = UnixListener::bind(&socket_path).map_err(|source| {
        SupervisorError::SocketIo {
            action: "bind",
            path: socket_path.clone(),
            source,
        }
    })?;
    fs::set_permissions(&socket_path, fs::Permissions::from_mode(0o600))
        .map_err(|source| SupervisorError::SocketIo {
            action: "secure",
            path: socket_path.clone(),
            source,
        })?;

    let index = ActiveRunIndex::new(&directories);
    index.publish(&active)?;
    let serve_result =
        serve_listener(listener, active.clone(), &mut store).await;
    let index_result = if serve_result.is_ok() {
        index.retire(&project.identity, active.run_id)
    } else {
        Ok(())
    };
    let socket_result = remove_owned_socket(&socket_path);
    drop(lease);

    serve_result?;
    index_result?;
    socket_result?;
    Ok(())
}

async fn serve_listener(
    listener: UnixListener,
    active: ActiveRunEntry,
    store: &mut Store,
) -> Result<(), SupervisorError> {
    let (shutdown_tx, mut shutdown_rx) = mpsc::channel(1);
    let (command_tx, mut command_rx) = mpsc::channel(16);
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            shutdown = shutdown_rx.recv() => {
                if shutdown.is_some() {
                    break;
                }
                return Err(SupervisorError::ShutdownChannelClosed);
            }
            accepted = listener.accept() => {
                let (stream, _) = accepted.map_err(|source| {
                    SupervisorError::SocketIo {
                        action: "accept from",
                        path: PathBuf::from("<bound supervisor socket>"),
                        source,
                    }
                })?;
                connections.spawn(serve_connection(
                    stream,
                    active.clone(),
                    command_tx.clone(),
                    shutdown_tx.clone(),
                ));
            }
            Some(command) = command_rx.recv() => {
                handle_command(store, active.run_id, command);
            }
            Some(completed) = connections.join_next(), if !connections.is_empty() => {
                match completed {
                    Ok(Ok(())) | Ok(Err(_)) | Err(_) => {}
                }
            }
        }
    }
    connections.abort_all();
    while connections.join_next().await.is_some() {}
    Ok(())
}

enum SupervisorCommand {
    AuthenticateAgent {
        agent_id: AgentId,
        session_id: SessionId,
        token: AgentToken,
        response: oneshot::Sender<Result<Option<SessionScope>, RpcFailure>>,
    },
    Shutdown {
        operation_id: OperationId,
        response: oneshot::Sender<Result<RpcResponse, RpcFailure>>,
    },
}

fn handle_command(
    store: &mut Store,
    run_id: RunId,
    command: SupervisorCommand,
) {
    match command {
        SupervisorCommand::AuthenticateAgent {
            agent_id,
            session_id,
            token,
            response,
        } => {
            let result =
                authenticate_agent(store, run_id, agent_id, session_id, &token)
                    .map_err(|error| {
                        RpcFailure::new(
                            RpcFailureCode::Internal,
                            error.to_string(),
                        )
                    });
            let _request_may_have_disconnected = response.send(result);
        }
        SupervisorCommand::Shutdown {
            operation_id,
            response,
        } => {
            let result = persist_shutdown(store, run_id, operation_id).map_err(
                |error| {
                    RpcFailure::new(RpcFailureCode::Internal, error.to_string())
                },
            );
            let _request_may_have_disconnected = response.send(result);
        }
    }
}

fn authenticate_agent(
    store: &mut Store,
    run_id: RunId,
    agent_id: AgentId,
    session_id: SessionId,
    token: &AgentToken,
) -> Result<Option<SessionScope>, StoreError> {
    store.transaction(|repositories| {
        let Some(credential) = repositories
            .active_session_credential(run_id, agent_id, session_id)?
        else {
            return Ok(None);
        };
        let scope = SessionScope {
            run_id: credential.run_id,
            agent_id: credential.agent_id,
            session_id: credential.session_id,
            generation: credential.generation,
        };
        Ok(credential
            .token_verifier
            .verify(token, scope)
            .then_some(scope))
    })
}

fn persist_shutdown(
    store: &mut Store,
    run_id: RunId,
    operation_id: OperationId,
) -> Result<RpcResponse, SupervisorError> {
    let stopped_at = unix_timestamp()?;
    let mutation = Mutation {
        id: operation_id,
        run_id,
        kind: "run.stop".to_owned(),
        actor_agent_id: None,
        request: json!({}),
        created_at: stopped_at,
    };
    let outcome = store.mutate(&mutation, |repositories| {
        repositories.stop_run(run_id, stopped_at)?;
        Ok(RpcResponse::ShuttingDown {
            run_id,
            operation_id,
        })
    })?;
    Ok(match outcome {
        MutationOutcome::Applied(response)
        | MutationOutcome::Replayed(response) => response,
    })
}

fn initialize_store(
    run_state_directory: &Path,
    active: &ActiveRunEntry,
    project: &DiscoveredProject,
) -> Result<Store, SupervisorError> {
    let database_path = run_state_directory.join(DATABASE_FILE);
    let mut store = Store::open(&database_path)?;
    fs::set_permissions(&database_path, fs::Permissions::from_mode(0o600))
        .map_err(|source| SupervisorError::StateFileIo {
            action: "secure",
            path: database_path,
            source,
        })?;
    let (run, stored_project) = store.transaction(|repositories| {
        Ok((
            repositories.run(active.run_id)?,
            repositories.project(active.project_id)?,
        ))
    })?;
    match (run, stored_project) {
        (None, None) => {
            let now = unix_timestamp()?;
            store.transaction(|repositories| {
                repositories.insert_run(&RunRecord {
                    id: active.run_id,
                    status: "active".to_owned(),
                    created_at: now,
                    stopped_at: None,
                })?;
                repositories.insert_project(&ProjectRecord {
                    id: active.project_id,
                    run_id: active.run_id,
                    alias: "primary".to_owned(),
                    original_path: project.original_path.clone(),
                    canonical_path: project.canonical_path.clone(),
                    identity: project.identity.clone(),
                    is_primary: true,
                    attached_at: now,
                })
            })?;
        }
        (Some(run), Some(stored_project))
            if run.status == "active"
                && stored_project.run_id == active.run_id
                && stored_project.is_primary
                && stored_project.canonical_path == project.canonical_path
                && stored_project.identity == project.identity => {}
        _ => {
            return Err(SupervisorError::RunStateMismatch {
                run_id: active.run_id,
                project_id: active.project_id,
            });
        }
    }
    Ok(store)
}

fn unix_timestamp() -> Result<i64, SupervisorError> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(SupervisorError::SystemClock)?;
    i64::try_from(elapsed.as_secs())
        .map_err(|_| SupervisorError::TimestampOverflow)
}

fn remove_stale_socket(path: &Path) -> Result<(), SupervisorError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_socket() => {
            fs::remove_file(path).map_err(|source| SupervisorError::SocketIo {
                action: "remove stale",
                path: path.to_owned(),
                source,
            })
        }
        Ok(_) => Err(SupervisorError::UnsafeSocketPath {
            path: path.to_owned(),
        }),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(SupervisorError::SocketIo {
            action: "inspect",
            path: path.to_owned(),
            source,
        }),
    }
}

fn remove_owned_socket(path: &Path) -> Result<(), SupervisorError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(SupervisorError::SocketIo {
            action: "remove owned",
            path: path.to_owned(),
            source,
        }),
    }
}

fn checked_socket_path(
    directories: &CoterieDirectories,
    run_id: RunId,
) -> Result<PathBuf, SupervisorError> {
    validate_socket_path(directories.socket_path(run_id))
}

fn validate_socket_path(path: PathBuf) -> Result<PathBuf, SupervisorError> {
    let length = path.as_os_str().as_bytes().len();
    if length > MAXIMUM_LINUX_SOCKET_PATH_LENGTH {
        Err(SupervisorError::SocketPathTooLong {
            path,
            length,
            maximum: MAXIMUM_LINUX_SOCKET_PATH_LENGTH,
        })
    } else {
        Ok(path)
    }
}

/// A connected, handshaken client for one local run supervisor.
#[derive(Debug)]
pub(crate) struct SupervisorClient {
    stream: UnixStream,
    run_id: crate::id::RunId,
    next_request_id: u64,
    authentication: RequestAuthentication,
}

impl SupervisorClient {
    pub(crate) async fn connect_operator_at(
        socket_path: &Path,
        expected: &ActiveRunEntry,
    ) -> Result<Self, SupervisorError> {
        Self::connect_with(
            socket_path,
            expected,
            ConnectionChannel::Operator,
            RequestAuthentication::Operator,
        )
        .await
    }

    #[allow(
        dead_code,
        reason = "the next M2 provider-lifecycle item launches the first agent client"
    )]
    pub(crate) async fn connect_agent_at(
        socket_path: &Path,
        expected: &ActiveRunEntry,
        agent_id: AgentId,
        session_id: SessionId,
        token: AgentToken,
    ) -> Result<Self, SupervisorError> {
        Self::connect_with(
            socket_path,
            expected,
            ConnectionChannel::Agent,
            RequestAuthentication::Agent {
                agent_id,
                session_id,
                token,
            },
        )
        .await
    }

    async fn connect_with(
        socket_path: &Path,
        expected: &ActiveRunEntry,
        channel: ConnectionChannel,
        authentication: RequestAuthentication,
    ) -> Result<Self, SupervisorError> {
        let mut stream =
            UnixStream::connect(socket_path).await.map_err(|source| {
                SupervisorError::SocketIo {
                    action: "connect to",
                    path: socket_path.to_owned(),
                    source,
                }
            })?;
        write_frame(
            &mut stream,
            &ClientMessage::Handshake(HandshakeRequest {
                protocol_version: PROTOCOL_VERSION,
                expected_run_id: expected.run_id,
                project_key: expected.project_key.clone(),
                channel,
            }),
        )
        .await?;

        match read_frame::<_, ServerMessage>(&mut stream).await? {
            ServerMessage::Handshake(response)
                if response.protocol_version == PROTOCOL_VERSION
                    && response.run_id == expected.run_id
                    && response.project_id == expected.project_id
                    && response.project_key == expected.project_key =>
            {
                Ok(Self {
                    stream,
                    run_id: response.run_id,
                    next_request_id: 1,
                    authentication,
                })
            }
            ServerMessage::Handshake(_) => Err(SupervisorError::InvalidProof),
            ServerMessage::Rejected(failure) => Err(failure.into()),
            ServerMessage::Response(_) => {
                Err(SupervisorError::UnexpectedMessage {
                    expected: "handshake response",
                })
            }
        }
    }

    #[must_use]
    pub(crate) fn run_id(&self) -> crate::id::RunId {
        self.run_id
    }

    pub(crate) async fn ping(
        &mut self,
    ) -> Result<RpcResponse, SupervisorError> {
        self.request(RpcRequest::Ping).await
    }

    pub(crate) async fn shutdown(
        &mut self,
        operation_id: OperationId,
    ) -> Result<RpcResponse, SupervisorError> {
        self.request(RpcRequest::Shutdown { operation_id }).await
    }

    async fn request(
        &mut self,
        request: RpcRequest,
    ) -> Result<RpcResponse, SupervisorError> {
        let request_id = self.next_request_id;
        self.next_request_id = self
            .next_request_id
            .checked_add(1)
            .ok_or(SupervisorError::RequestIdExhausted)?;
        write_frame(
            &mut self.stream,
            &ClientMessage::Request(VersionedRequest {
                protocol_version: PROTOCOL_VERSION,
                request_id,
                authentication: self.authentication.clone(),
                request,
            }),
        )
        .await?;
        let ServerMessage::Response(response) =
            read_frame::<_, ServerMessage>(&mut self.stream).await?
        else {
            return Err(SupervisorError::UnexpectedMessage {
                expected: "RPC response",
            });
        };
        if response.protocol_version != PROTOCOL_VERSION
            || response.request_id != request_id
        {
            return Err(SupervisorError::InvalidResponseCorrelation);
        }
        match response.result {
            RpcResult::Ok(response) => Ok(response),
            RpcResult::Err(failure) => Err(failure.into()),
        }
    }
}

async fn serve_connection(
    mut stream: UnixStream,
    active: ActiveRunEntry,
    commands: mpsc::Sender<SupervisorCommand>,
    shutdown: mpsc::Sender<()>,
) -> Result<(), SupervisorError> {
    let handshake = match read_frame::<_, ClientMessage>(&mut stream).await? {
        ClientMessage::Handshake(handshake) => handshake,
        ClientMessage::Request(_) => {
            reject(
                &mut stream,
                RpcFailure::new(
                    RpcFailureCode::HandshakeRequired,
                    "the first message must be a handshake",
                ),
            )
            .await?;
            return Ok(());
        }
    };

    if let Some(failure) = validate_handshake(&handshake, &active) {
        reject(&mut stream, failure).await?;
        return Ok(());
    }
    write_frame(
        &mut stream,
        &ServerMessage::Handshake(HandshakeResponse {
            protocol_version: PROTOCOL_VERSION,
            run_id: active.run_id,
            project_id: active.project_id,
            project_key: active.project_key.clone(),
        }),
    )
    .await?;

    let channel = handshake.channel;
    let mut last_request_id = 0;
    loop {
        let request = match read_frame::<_, ClientMessage>(&mut stream).await? {
            ClientMessage::Handshake(_) => {
                reject(
                    &mut stream,
                    RpcFailure::new(
                        RpcFailureCode::InvalidRequestSequence,
                        "a connection may perform only one handshake",
                    ),
                )
                .await?;
                return Ok(());
            }
            ClientMessage::Request(request) => request,
        };

        let (result, shutting_down) = if request.protocol_version
            != PROTOCOL_VERSION
        {
            (
                RpcResult::Err(RpcFailure::new(
                    RpcFailureCode::ProtocolVersionMismatch,
                    format!(
                        "client requested protocol version {}, but the supervisor supports {}",
                        request.protocol_version, PROTOCOL_VERSION
                    ),
                )),
                false,
            )
        } else if request.request_id <= last_request_id {
            (
                RpcResult::Err(RpcFailure::new(
                    RpcFailureCode::InvalidRequestSequence,
                    "request IDs must increase within a connection",
                )),
                false,
            )
        } else {
            last_request_id = request.request_id;
            match authenticate_request(
                &commands,
                active.run_id,
                channel,
                request.authentication,
            )
            .await?
            {
                Ok(caller) => match request.request {
                    RpcRequest::Ping => (
                        RpcResult::Ok(RpcResponse::Pong {
                            run_id: active.run_id,
                        }),
                        false,
                    ),
                    RpcRequest::Shutdown { operation_id } => {
                        if caller.is_operator() {
                            let result =
                                request_shutdown(&commands, operation_id)
                                    .await?;
                            let shutting_down = matches!(
                                result,
                                RpcResult::Ok(RpcResponse::ShuttingDown { .. })
                            );
                            (result, shutting_down)
                        } else {
                            (
                                RpcResult::Err(RpcFailure::new(
                                    RpcFailureCode::PermissionDenied,
                                    "agent credentials do not grant operator authority",
                                )),
                                false,
                            )
                        }
                    }
                },
                Err(failure) => (RpcResult::Err(failure), false),
            }
        };
        write_frame(
            &mut stream,
            &ServerMessage::Response(VersionedResponse {
                protocol_version: PROTOCOL_VERSION,
                request_id: request.request_id,
                result,
            }),
        )
        .await?;
        if shutting_down {
            shutdown
                .send(())
                .await
                .map_err(|_| SupervisorError::ShutdownChannelClosed)?;
            return Ok(());
        }
    }
}

enum AuthenticatedCaller {
    Operator,
    Agent(SessionScope),
}

impl AuthenticatedCaller {
    fn is_operator(&self) -> bool {
        match self {
            Self::Operator => true,
            Self::Agent(scope) => {
                let _authenticated_identity = scope;
                false
            }
        }
    }
}

async fn authenticate_request(
    commands: &mpsc::Sender<SupervisorCommand>,
    run_id: RunId,
    channel: ConnectionChannel,
    authentication: RequestAuthentication,
) -> Result<Result<AuthenticatedCaller, RpcFailure>, SupervisorError> {
    match (channel, authentication) {
        (ConnectionChannel::Operator, RequestAuthentication::Operator) => {
            Ok(Ok(AuthenticatedCaller::Operator))
        }
        (
            ConnectionChannel::Agent,
            RequestAuthentication::Agent {
                agent_id,
                session_id,
                token,
            },
        ) => {
            let (response, receiver) = oneshot::channel();
            commands
                .send(SupervisorCommand::AuthenticateAgent {
                    agent_id,
                    session_id,
                    token,
                    response,
                })
                .await
                .map_err(|_| SupervisorError::CommandChannelClosed)?;
            match receiver.await {
                Ok(Ok(Some(scope))) if scope.run_id == run_id => {
                    Ok(Ok(AuthenticatedCaller::Agent(scope)))
                }
                Ok(Ok(Some(_)) | Ok(None)) => Ok(Err(RpcFailure::new(
                    RpcFailureCode::Unauthenticated,
                    "agent credentials are invalid or no longer active",
                ))),
                Ok(Err(failure)) => Ok(Err(failure)),
                Err(_) => Err(SupervisorError::CommandChannelClosed),
            }
        }
        _ => Ok(Err(RpcFailure::new(
            RpcFailureCode::Unauthenticated,
            "request credentials do not match the established channel",
        ))),
    }
}

async fn request_shutdown(
    commands: &mpsc::Sender<SupervisorCommand>,
    operation_id: OperationId,
) -> Result<RpcResult, SupervisorError> {
    let (response, receiver) = oneshot::channel();
    commands
        .send(SupervisorCommand::Shutdown {
            operation_id,
            response,
        })
        .await
        .map_err(|_| SupervisorError::CommandChannelClosed)?;
    Ok(match receiver.await {
        Ok(Ok(response)) => RpcResult::Ok(response),
        Ok(Err(failure)) => RpcResult::Err(failure),
        Err(_) => return Err(SupervisorError::CommandChannelClosed),
    })
}

fn validate_handshake(
    request: &HandshakeRequest,
    active: &ActiveRunEntry,
) -> Option<RpcFailure> {
    if request.protocol_version != PROTOCOL_VERSION {
        Some(RpcFailure::new(
            RpcFailureCode::ProtocolVersionMismatch,
            format!(
                "client requested protocol version {}, but the supervisor supports {}",
                request.protocol_version, PROTOCOL_VERSION
            ),
        ))
    } else if request.expected_run_id != active.run_id {
        Some(RpcFailure::new(
            RpcFailureCode::RunMismatch,
            format!(
                "socket belongs to run {}, not {}",
                active.run_id, request.expected_run_id
            ),
        ))
    } else if request.project_key != active.project_key {
        Some(RpcFailure::new(
            RpcFailureCode::ProjectMismatch,
            "socket does not own the expected project identity",
        ))
    } else {
        None
    }
}

async fn reject(
    stream: &mut UnixStream,
    failure: RpcFailure,
) -> Result<(), SupervisorError> {
    write_frame(stream, &ServerMessage::Rejected(failure)).await?;
    Ok(())
}

/// A failure while locating, starting, or communicating with a supervisor.
#[derive(Debug, Error)]
pub(crate) enum SupervisorError {
    #[error(transparent)]
    Project(#[from] ProjectError),
    #[error(transparent)]
    State(#[from] StoreError),
    #[error("could not {action} supervisor socket at {path:?}: {source}")]
    SocketIo {
        action: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error(transparent)]
    Frame(#[from] FrameError),
    #[error("supervisor rejected the request ({code:?}): {message}")]
    Rejected {
        code: RpcFailureCode,
        message: String,
    },
    #[error("supervisor returned an invalid ownership proof")]
    InvalidProof,
    #[error("supervisor returned an invalid response correlation")]
    InvalidResponseCorrelation,
    #[error("expected {expected} from the supervisor")]
    UnexpectedMessage { expected: &'static str },
    #[error("local RPC request IDs are exhausted")]
    RequestIdExhausted,
    #[error("could not determine the current project directory: {0}")]
    CurrentDirectory(#[source] io::Error),
    #[error("could not locate the current Coterie executable: {0}")]
    CurrentExecutable(#[source] io::Error),
    #[error(
        "could not start supervisor executable at {executable:?}: {source}"
    )]
    Spawn {
        executable: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("could not inspect the child supervisor: {0}")]
    ChildStatus(#[source] io::Error),
    #[error(
        "supervisor startup for {project:?} timed out; child status was {child_status:?}; child diagnostic was {child_error:?}"
    )]
    StartupTimeout {
        project: PathBuf,
        child_status: Option<std::process::ExitStatus>,
        child_error: Option<String>,
    },
    #[error("no active run is indexed for this project")]
    NoActiveRun,
    #[error("invalid private supervisor arguments: {reason}")]
    InvalidInternalArguments { reason: &'static str },
    #[error("invalid private supervisor {name} argument")]
    InvalidIdArgument { name: &'static str },
    #[error("run {run_id} and project {project_id} do not match durable state")]
    RunStateMismatch {
        run_id: RunId,
        project_id: ProjectId,
    },
    #[error("could not {action} supervisor state file at {path:?}: {source}")]
    StateFileIo {
        action: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("refusing to replace a non-socket runtime path at {path:?}")]
    UnsafeSocketPath { path: PathBuf },
    #[error("system clock is before the Unix epoch: {0}")]
    SystemClock(#[source] std::time::SystemTimeError),
    #[error("the current Unix timestamp does not fit durable state")]
    TimestampOverflow,
    #[error("the child supervisor did not expose its diagnostic stream")]
    MissingChildStderr,
    #[error("the supervisor command channel closed unexpectedly")]
    CommandChannelClosed,
    #[error("supervisor run {run_id} did not retire after shutdown")]
    ShutdownTimeout { run_id: RunId },
    #[error("the supervisor shutdown channel closed unexpectedly")]
    ShutdownChannelClosed,
    #[error(
        "supervisor socket path {path:?} has {length} bytes, exceeding Linux's {maximum}-byte limit"
    )]
    SocketPathTooLong {
        path: PathBuf,
        length: usize,
        maximum: usize,
    },
}

impl SupervisorError {
    fn is_transient_connection_failure(&self) -> bool {
        match self {
            Self::SocketIo { source, .. } => matches!(
                source.kind(),
                io::ErrorKind::NotFound
                    | io::ErrorKind::ConnectionRefused
                    | io::ErrorKind::ConnectionReset
            ),
            Self::Frame(FrameError::Io(source)) => matches!(
                source.kind(),
                io::ErrorKind::UnexpectedEof
                    | io::ErrorKind::BrokenPipe
                    | io::ErrorKind::ConnectionReset
            ),
            _ => false,
        }
    }
}

impl From<RpcFailure> for SupervisorError {
    fn from(failure: RpcFailure) -> Self {
        Self::Rejected {
            code: failure.code,
            message: failure.message,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::DirBuilderExt;
    use std::path::PathBuf;

    use tokio::net::UnixListener;
    use tokio::sync::mpsc;

    use super::{
        SupervisorClient, SupervisorCommand, SupervisorError, persist_shutdown,
        serve_connection, serve_listener, validate_handshake,
        validate_socket_path,
    };
    use crate::auth::{AgentToken, SessionScope};
    use crate::id::{AgentId, OperationId, ProjectId, RunId, SessionId};
    use crate::project::{ActiveRunEntry, ProjectIdentity};
    use crate::protocol::{
        ClientMessage, ConnectionChannel, HandshakeRequest,
        RequestAuthentication, RpcFailureCode, RpcResponse, ServerMessage,
        VersionedRequest, read_frame, write_frame,
    };
    use crate::state::{
        AgentRecord, RunRecord, SessionCredentialRecord, SessionRecord, Store,
        StoreError,
    };

    const RUN_ID: &str = "cr-01ARZ3NDEKTSV4RRFFQ69G5FAV";
    const PROJECT_ID: &str = "cp-01ARZ3NDEKTSV4RRFFQ69G5FAW";
    const AGENT_ID: &str = "cg-01ARZ3NDEKTSV4RRFFQ69G5FAX";
    const SESSION_ID: &str = "cs-01ARZ3NDEKTSV4RRFFQ69G5FAY";
    const TOKEN: &str =
        "cot1_000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";

    #[tokio::test]
    async fn live_handshake_and_typed_ping_prove_the_indexed_owner() {
        let fixture = TestDirectory::new();
        let socket = fixture.join("supervisor.sock");
        let entry = entry(&fixture.join("project"));
        let listener = UnixListener::bind(&socket)
            .expect("the fixture socket should bind");
        let server_entry = entry.clone();
        let (shutdown_tx, _shutdown_rx) = mpsc::channel(1);
        let (command_tx, mut command_rx) = mpsc::channel(1);
        let operation_id = "co-01ARZ3NDEKTSV4RRFFQ69G5FAX"
            .parse::<OperationId>()
            .expect("valid operation ID");
        let command_server = tokio::spawn(async move {
            let (operation_id, response) = match command_rx
                .recv()
                .await
                .expect("the shutdown command should arrive")
            {
                SupervisorCommand::Shutdown {
                    operation_id,
                    response,
                } => (operation_id, response),
                SupervisorCommand::AuthenticateAgent { .. } => {
                    panic!(
                        "the operator ping must not use agent authentication"
                    )
                }
            };
            response
                .send(Ok(RpcResponse::ShuttingDown {
                    run_id: server_entry.run_id,
                    operation_id,
                }))
                .expect("the connection should await its response");
        });
        let served_entry = entry.clone();
        let server = tokio::spawn(async move {
            let (stream, _) =
                listener.accept().await.expect("the client should connect");
            serve_connection(stream, served_entry, command_tx, shutdown_tx)
                .await
        });

        let mut client = SupervisorClient::connect_operator_at(&socket, &entry)
            .await
            .expect("the handshake should succeed");
        assert_eq!(
            client.ping().await.expect("the ping should succeed"),
            RpcResponse::Pong {
                run_id: entry.run_id
            }
        );
        assert_eq!(
            client
                .shutdown(operation_id)
                .await
                .expect("shutdown should be a typed RPC"),
            RpcResponse::ShuttingDown {
                run_id: entry.run_id,
                operation_id,
            }
        );
        server
            .await
            .expect("the server task should finish")
            .expect("the connection should remain valid");
        command_server
            .await
            .expect("the command task should finish");
    }

    #[tokio::test]
    async fn handshake_rejects_a_socket_for_another_project() {
        let fixture = TestDirectory::new();
        let socket = fixture.join("supervisor.sock");
        let server_entry = entry(&fixture.join("server-project"));
        let client_entry = ActiveRunEntry::new(
            server_entry.run_id,
            server_entry.project_id,
            ProjectIdentity::Directory {
                canonical_directory: fixture.join("different-project"),
            },
        );
        let listener = UnixListener::bind(&socket)
            .expect("the fixture socket should bind");
        let (shutdown_tx, _shutdown_rx) = mpsc::channel(1);
        let (command_tx, _command_rx) = mpsc::channel(1);
        let server = tokio::spawn(async move {
            let (stream, _) =
                listener.accept().await.expect("the client should connect");
            serve_connection(stream, server_entry, command_tx, shutdown_tx)
                .await
        });

        let error =
            SupervisorClient::connect_operator_at(&socket, &client_entry)
                .await
                .expect_err("the project mismatch must be rejected");

        assert!(matches!(
            error,
            SupervisorError::Rejected {
                code: RpcFailureCode::ProjectMismatch,
                ..
            }
        ));
        server
            .await
            .expect("the server task should finish")
            .expect("a rejected handshake is handled normally");
    }

    #[test]
    fn handshake_checks_protocol_and_run_identity() {
        let fixture = TestDirectory::new();
        let active = entry(&fixture.join("project"));
        let mut request = HandshakeRequest {
            protocol_version: crate::protocol::PROTOCOL_VERSION,
            expected_run_id: RunId::generate(),
            project_key: active.project_key.clone(),
            channel: ConnectionChannel::Operator,
        };

        assert_eq!(
            validate_handshake(&request, &active)
                .expect("the run mismatch should be rejected")
                .code,
            RpcFailureCode::RunMismatch
        );
        request.protocol_version = crate::protocol::PROTOCOL_VERSION + 1;
        assert_eq!(
            validate_handshake(&request, &active)
                .expect("the version mismatch should take precedence")
                .code,
            RpcFailureCode::ProtocolVersionMismatch
        );
    }

    #[test]
    fn overlong_linux_socket_paths_fail_before_process_startup() {
        let path = PathBuf::from("x".repeat(108));

        assert!(matches!(
            validate_socket_path(path),
            Err(SupervisorError::SocketPathTooLong {
                length: 108,
                maximum: 107,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn requests_cannot_bypass_the_handshake() {
        let fixture = TestDirectory::new();
        let socket = fixture.join("supervisor.sock");
        let server_entry = entry(&fixture.join("project"));
        let listener = UnixListener::bind(&socket)
            .expect("the fixture socket should bind");
        let (shutdown_tx, _shutdown_rx) = mpsc::channel(1);
        let (command_tx, _command_rx) = mpsc::channel(1);
        let server = tokio::spawn(async move {
            let (stream, _) =
                listener.accept().await.expect("the client should connect");
            serve_connection(stream, server_entry, command_tx, shutdown_tx)
                .await
        });
        let mut stream = tokio::net::UnixStream::connect(&socket)
            .await
            .expect("the client should connect");

        write_frame(
            &mut stream,
            &ClientMessage::Request(VersionedRequest {
                protocol_version: 1,
                request_id: 1,
                authentication: RequestAuthentication::Operator,
                request: crate::protocol::RpcRequest::Ping,
            }),
        )
        .await
        .expect("the out-of-sequence request should be sent");
        let response = read_frame::<_, ServerMessage>(&mut stream)
            .await
            .expect("the rejection should be framed");

        assert!(matches!(
            response,
            ServerMessage::Rejected(failure)
                if failure.code == RpcFailureCode::HandshakeRequired
        ));
        server
            .await
            .expect("the server task should finish")
            .expect("a rejected sequence is handled normally");
    }

    #[tokio::test]
    async fn agent_rpc_authenticates_the_current_session_without_operator_authority()
     {
        let fixture = TestDirectory::new();
        let socket = fixture.join("supervisor.sock");
        let active = entry(&fixture.join("project"));
        let listener = UnixListener::bind(&socket)
            .expect("the fixture socket should bind");
        let agent_id = AGENT_ID.parse::<AgentId>().expect("valid agent ID");
        let session_id =
            SESSION_ID.parse::<SessionId>().expect("valid session ID");
        let token = TOKEN.parse::<AgentToken>().expect("valid agent token");
        let scope = SessionScope {
            run_id: active.run_id,
            agent_id,
            session_id,
            generation: 2,
        };
        let mut store = Store::open(&fixture.join("state.sqlite3"))
            .expect("the store should open");
        store
            .transaction(|repositories| {
                repositories.insert_run(&RunRecord {
                    id: active.run_id,
                    status: "active".to_owned(),
                    created_at: 10,
                    stopped_at: None,
                })?;
                repositories.insert_agent(&AgentRecord {
                    id: agent_id,
                    run_id: active.run_id,
                    role: "worker".to_owned(),
                    generation: scope.generation,
                    state: "running".to_owned(),
                    created_at: 11,
                })?;
                repositories.insert_session(&SessionRecord {
                    id: session_id,
                    run_id: active.run_id,
                    agent_id,
                    generation: scope.generation,
                    provider: "fake".to_owned(),
                    state: "running".to_owned(),
                    transcript_path: PathBuf::from("transcripts/session.jsonl"),
                    created_at: 12,
                    ended_at: None,
                })?;
                repositories.activate_session_credential(
                    &SessionCredentialRecord {
                        session_id,
                        run_id: active.run_id,
                        agent_id,
                        generation: scope.generation,
                        token_verifier: token.verifier(scope),
                        created_at: 12,
                        revoked_at: None,
                    },
                )
            })
            .expect("the active session should be inserted");
        let served_entry = active.clone();
        let server = tokio::spawn(async move {
            serve_listener(listener, served_entry, &mut store).await
        });

        let mut wrong_token_client = SupervisorClient::connect_agent_at(
            &socket,
            &active,
            agent_id,
            session_id,
            AgentToken::generate().expect("randomness should be available"),
        )
        .await
        .expect("the agent channel handshake should succeed");
        assert!(matches!(
            wrong_token_client.ping().await,
            Err(SupervisorError::Rejected {
                code: RpcFailureCode::Unauthenticated,
                ..
            })
        ));
        wrong_token_client.authentication = RequestAuthentication::Operator;
        assert!(matches!(
            wrong_token_client.ping().await,
            Err(SupervisorError::Rejected {
                code: RpcFailureCode::Unauthenticated,
                ..
            })
        ));

        let mut agent = SupervisorClient::connect_agent_at(
            &socket, &active, agent_id, session_id, token,
        )
        .await
        .expect("the agent channel handshake should succeed");
        assert_eq!(
            agent.ping().await.expect("the token should authenticate"),
            RpcResponse::Pong {
                run_id: active.run_id,
            }
        );
        assert!(matches!(
            agent.shutdown(OperationId::generate()).await,
            Err(SupervisorError::Rejected {
                code: RpcFailureCode::PermissionDenied,
                ..
            })
        ));

        let mut operator =
            SupervisorClient::connect_operator_at(&socket, &active)
                .await
                .expect("the operator channel handshake should succeed");
        operator
            .shutdown(OperationId::generate())
            .await
            .expect("the operator should stop the run");
        server
            .await
            .expect("the server task should finish")
            .expect("the listener should shut down cleanly");
    }

    #[test]
    fn shutdown_is_a_durable_idempotent_mutation() {
        let fixture = TestDirectory::new();
        let mut store = Store::open(&fixture.join("state.sqlite3"))
            .expect("the store should open");
        let run_id = RUN_ID.parse::<RunId>().expect("valid run ID");
        store
            .transaction(|repositories| {
                repositories.insert_run(&RunRecord {
                    id: run_id,
                    status: "active".to_owned(),
                    created_at: 10,
                    stopped_at: None,
                })
            })
            .expect("the active run should be inserted");
        let operation_id = "co-01ARZ3NDEKTSV4RRFFQ69G5FAX"
            .parse::<OperationId>()
            .expect("valid operation ID");
        let expected = RpcResponse::ShuttingDown {
            run_id,
            operation_id,
        };

        assert_eq!(
            persist_shutdown(&mut store, run_id, operation_id)
                .expect("the first request should stop the run"),
            expected
        );
        assert_eq!(
            persist_shutdown(&mut store, run_id, operation_id)
                .expect("the retry should replay its result"),
            expected
        );

        let different_operation = "co-01ARZ3NDEKTSV4RRFFQ69G5FAY"
            .parse::<OperationId>()
            .expect("valid operation ID");
        assert!(matches!(
            persist_shutdown(&mut store, run_id, different_operation),
            Err(SupervisorError::State(StoreError::RunNotActive { id }))
                if id == run_id
        ));
    }

    fn entry(project: &std::path::Path) -> ActiveRunEntry {
        ActiveRunEntry::new(
            RUN_ID.parse::<RunId>().expect("valid run ID"),
            PROJECT_ID.parse::<ProjectId>().expect("valid project ID"),
            ProjectIdentity::Directory {
                canonical_directory: project.to_owned(),
            },
        )
    }

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let path = std::env::temp_dir()
                .join(format!("coterie-supervisor-test-{}", RunId::generate()));
            fs::DirBuilder::new()
                .mode(0o700)
                .create(&path)
                .expect("the test directory should be created");
            Self(path)
        }

        fn join(&self, path: impl AsRef<std::path::Path>) -> PathBuf {
            self.0.join(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            if self.0.exists() {
                fs::remove_dir_all(&self.0)
                    .expect("the test directory should be removable");
            }
        }
    }
}

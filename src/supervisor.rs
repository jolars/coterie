//! Desired-state reconciliation and process ownership.

mod session;

use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::io::{self, Read};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::os::unix::process::ExitStatusExt;
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::json;
use thiserror::Error;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinSet;
use tokio::time::{Instant, sleep};

use crate::auth::{AgentToken, SessionScope};
use crate::cli::{
    Arguments, Command as CliCommand, FinishStatus as CliFinishStatus,
    InboxCommand, TaskCommand,
};
use crate::config::{
    AuthorizationDecision, Capability, RoleMode, WorkspacePolicy,
    builtin_standard, compiled_defaults,
};
use crate::id::{
    AgentId, AssignmentId, MessageId, OperationId, ProjectId, RunId, SessionId,
    TaskId,
};
use crate::project::{
    ActiveRunEntry, ActiveRunIndex, CoterieDirectories, DiscoveredProject,
    LeaseAttempt, ProjectError, ProjectLease,
};
use crate::protocol::{
    AgentSummary, CallerChannel, CallerSummary, ClientMessage,
    ConnectionChannel, EventSummary, FinishStatus, FrameError,
    HandshakeRequest, HandshakeResponse, MessageSummary, PROTOCOL_VERSION,
    ProjectSummary, RequestAuthentication, RpcFailure, RpcFailureCode,
    RpcRequest, RpcResponse, RpcResult, ServerMessage, TaskCounts, TaskSummary,
    VersionedRequest, VersionedResponse, read_frame, write_frame,
};
use crate::providers::fake::{FakeEvent, FakeProvider, FakeScript};
use crate::providers::{
    ActivityState, CodexProvider, InteractiveEnvironment, LaunchMode,
    LaunchSpecification, LifecycleState, Provider, ProviderCapability,
    SessionObservation,
};
use crate::state::{
    AcknowledgeMessagesMutation, AcknowledgeMessagesResult, AgentRecord,
    ClaimTaskMutation, ClaimTaskResult, DependencyRecord, EventKind,
    EventRecord, ExternalResourceState, MessageRecord, Mutation,
    MutationOutcome, NewEvent, ProjectRecord, RunRecord,
    SessionCredentialRecord, SessionProcessOwner, SessionRecord,
    SessionTransitionOutcome, Store, StoreError, TaskRecord,
    TaskTransitionMutation, TaskTransitionRejection, TaskTransitionResult,
    WorkspaceRecord,
};
use crate::tasks::{TaskStatus, TaskTransition};
use crate::workspace::fake::FakeWorkspace;
use crate::workspace::{WorkspaceError, WorkspaceSupervisor};

use self::session::{
    AgentLaunch, AgentSessionError, AgentSessionSupervisor,
    validate_provider_capabilities,
};
use crate::transcript::TranscriptStore;

const INTERNAL_SUPERVISOR_ARGUMENT: &str = "__supervisor";
const STARTUP_TIMEOUT: Duration = Duration::from_secs(5);
const STARTUP_RETRY_INTERVAL: Duration = Duration::from_millis(20);
const FOREGROUND_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
const DATABASE_FILE: &str = "state.sqlite3";
const MAXIMUM_LINUX_SOCKET_PATH_LENGTH: usize = 107;

/// Runs one parsed public command or private supervisor entrypoint.
pub(crate) async fn run(
    arguments: Arguments,
) -> Result<crate::cli::ExitCategory, SupervisorError> {
    let json_output = arguments.json;
    let foreground_operation_id = arguments.operation_id;
    match arguments.command {
        Some(CliCommand::SupervisorConnect) => {
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
            Ok(crate::cli::ExitCategory::Success)
        }
        Some(CliCommand::Supervisor(arguments)) => {
            let project = DiscoveredProject::discover(arguments.project_path)?;
            let entry = ActiveRunEntry::new(
                arguments.run_id,
                arguments.project_id,
                project.identity.clone(),
            );
            let directories = CoterieDirectories::from_environment()?;
            serve(entry, project, directories).await?;
            Ok(crate::cli::ExitCategory::Success)
        }
        Some(CliCommand::SupervisorShutdown) => {
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
            Ok(crate::cli::ExitCategory::Success)
        }
        None => {
            let operation_id =
                foreground_operation_id.unwrap_or_else(OperationId::generate);
            if json_output {
                return Err(SupervisorError::ForegroundJsonOutputUnsupported
                    .for_operation(operation_id));
            }
            let project = discover_current_project()
                .map_err(|error| error.for_operation(operation_id))?;
            let directories = CoterieDirectories::from_environment()
                .map_err(SupervisorError::from)
                .map_err(|error| error.for_operation(operation_id))?;
            let mut client = connect_or_start(&project, &directories)
                .await
                .map_err(|error| error.for_operation(operation_id))?;
            launch_foreground_codex(
                &project,
                &directories,
                &mut client,
                operation_id,
            )
            .await
            .map_err(|error| error.for_operation(operation_id))
        }
        Some(command) => {
            if foreground_operation_id.is_some() {
                return Err(SupervisorError::ForegroundOperationIdWithCommand);
            }
            let (request, operation_id, stopping) = public_request(command);
            let project = discover_current_project()
                .map_err(|error| command_error(error, operation_id))?;
            let directories = CoterieDirectories::from_environment()
                .map_err(SupervisorError::from)
                .map_err(|error| command_error(error, operation_id))?;
            let entry = active_entry(&project, &directories)
                .map_err(|error| command_error(error, operation_id))?;
            let mut client = connect_for_environment(&directories, &entry)
                .await
                .map_err(|error| command_error(error, operation_id))?;
            let response = client
                .request(request)
                .await
                .map_err(|error| command_error(error, operation_id))?;
            if stopping {
                await_retirement(&directories, &project, &entry)
                    .await
                    .map_err(|error| {
                        error.for_operation(
                            operation_id.expect("stop has an operation ID"),
                        )
                    })?;
            }
            render_public_response(json_output, operation_id, &response)
        }
    }
}

fn command_error(
    error: SupervisorError,
    operation_id: Option<OperationId>,
) -> SupervisorError {
    match operation_id {
        Some(operation_id) => error.for_operation(operation_id),
        None => error,
    }
}

fn active_entry(
    project: &DiscoveredProject,
    directories: &CoterieDirectories,
) -> Result<ActiveRunEntry, SupervisorError> {
    directories.prepare()?;
    ActiveRunIndex::new(directories)
        .lookup(&project.identity)?
        .ok_or(SupervisorError::NoActiveRun)
}

async fn launch_foreground_codex(
    project: &DiscoveredProject,
    directories: &CoterieDirectories,
    client: &mut SupervisorClient,
    operation_id: OperationId,
) -> Result<crate::cli::ExitCategory, SupervisorError> {
    let defaults = compiled_defaults();
    let binding = defaults
        .providers
        .get("codex")
        .expect("the compiled default archetype binds the Codex provider");
    let mut provider = CodexProvider::new(binding.command.iter().copied());
    let probe = provider.probe().map_err(AgentSessionError::from)?;
    validate_provider_capabilities(
        &probe,
        [
            ProviderCapability::ForegroundInteractive,
            ProviderCapability::StartupInstructions,
        ],
    )?;

    let token = AgentToken::generate().map_err(AgentSessionError::from)?;
    let response = client
        .request(RpcRequest::LaunchForeground {
            operation_id,
            token: token.clone(),
        })
        .await?;
    let RpcResponse::ForegroundPrepared {
        run_id,
        agent,
        session_id,
        generation,
        bootstrap_instruction,
    } = response
    else {
        return Err(SupervisorError::UnexpectedMessage {
            expected: "a foreground launch specification",
        });
    };
    if run_id != client.run_id() {
        return Err(SupervisorError::InvalidProof);
    }
    let scope = SessionScope {
        run_id,
        agent_id: agent.id,
        session_id,
        generation,
    };
    let socket_path = checked_socket_path(directories, run_id)?;
    let specification = LaunchSpecification {
        scope,
        working_directory: project.canonical_path.clone(),
        bootstrap_instruction,
    };
    let environment = InteractiveEnvironment {
        project_id: client.project_id(),
        primary_project_root: project.canonical_path.clone(),
        role: agent.role,
        socket_path,
        token,
    };
    let session =
        match provider.launch_interactive(&specification, Some(&environment)) {
            Ok(session) => session,
            Err(error) => {
                let _observation_may_fail = client
                    .request(RpcRequest::ForegroundLaunchFailed { scope })
                    .await;
                return Err(AgentSessionError::Provider(error).into());
            }
        };
    let process_id = provider
        .foreground_process_id(&session)
        .map_err(AgentSessionError::from)?;
    if let Err(error) = client
        .request(RpcRequest::ForegroundStarted { scope, process_id })
        .await
    {
        let _child_may_already_be_gone = provider
            .wait_foreground_until_termination(&session, std::future::ready(()))
            .await;
        return Err(error);
    }
    let termination = wait_for_foreground_termination(
        project.clone(),
        directories.clone(),
        run_id,
        scope,
    );
    let (status, termination_requested) = match provider
        .wait_foreground_until_termination(&session, termination)
        .await
    {
        Ok(observation) => observation,
        Err(error) => {
            let _observation_may_fail = client
                .request(RpcRequest::ForegroundObservationLost { scope })
                .await;
            return Err(AgentSessionError::Provider(error).into());
        }
    };
    record_foreground_exit(
        project,
        directories,
        client,
        RpcRequest::ForegroundExited {
            scope,
            code: status.code(),
            signal: status.signal(),
        },
        scope,
        termination_requested,
    )
    .await?;
    Ok(if status.success() {
        crate::cli::ExitCategory::Success
    } else {
        crate::cli::ExitCategory::Unavailable
    })
}

async fn wait_for_foreground_termination(
    project: DiscoveredProject,
    directories: CoterieDirectories,
    run_id: RunId,
    scope: SessionScope,
) {
    loop {
        let Some(entry) = ActiveRunIndex::new(&directories)
            .lookup(&project.identity)
            .ok()
            .flatten()
        else {
            sleep(STARTUP_RETRY_INTERVAL).await;
            continue;
        };
        if entry.run_id != run_id {
            sleep(STARTUP_RETRY_INTERVAL).await;
            continue;
        }
        let Ok(mut control) = connect_or_start(&project, &directories).await
        else {
            sleep(STARTUP_RETRY_INTERVAL).await;
            continue;
        };
        match control
            .request(RpcRequest::WaitForegroundControl { scope })
            .await
        {
            Ok(RpcResponse::ForegroundTerminationRequested { session_id })
                if session_id == scope.session_id =>
            {
                return;
            }
            Ok(_) | Err(_) => {
                sleep(STARTUP_RETRY_INTERVAL).await;
            }
        }
    }
}

async fn record_foreground_exit(
    project: &DiscoveredProject,
    directories: &CoterieDirectories,
    client: &mut SupervisorClient,
    request: RpcRequest,
    scope: SessionScope,
    termination_requested: bool,
) -> Result<(), SupervisorError> {
    match client.request(request.clone()).await {
        Ok(response) => {
            return validate_foreground_exit_response(response, scope);
        }
        Err(error) if error.is_transient_connection_failure() => {}
        Err(error) => return Err(error),
    }

    let deadline = Instant::now() + STARTUP_TIMEOUT;
    loop {
        let indexed =
            ActiveRunIndex::new(directories).lookup(&project.identity)?;
        let Some(entry) = indexed else {
            if termination_requested {
                return Ok(());
            }
            return Err(SupervisorError::NoActiveRun);
        };
        if entry.run_id != scope.run_id {
            return Err(SupervisorError::InvalidProof);
        }
        match connect_or_start(project, directories).await {
            Ok(mut reconnected) => match reconnected
                .request(request.clone())
                .await
            {
                Ok(response) => {
                    return validate_foreground_exit_response(response, scope);
                }
                Err(error)
                    if error.is_transient_connection_failure()
                        && Instant::now() < deadline => {}
                Err(error) => return Err(error),
            },
            Err(error)
                if error.is_transient_connection_failure()
                    && Instant::now() < deadline => {}
            Err(error) => return Err(error),
        }
        sleep(STARTUP_RETRY_INTERVAL).await;
    }
}

fn validate_foreground_exit_response(
    response: RpcResponse,
    scope: SessionScope,
) -> Result<(), SupervisorError> {
    match response {
        RpcResponse::ForegroundObserved { session_id, state }
            if session_id == scope.session_id
                && state == LifecycleState::Exited.as_str() =>
        {
            Ok(())
        }
        _ => Err(SupervisorError::UnexpectedMessage {
            expected: "a foreground exit observation",
        }),
    }
}

async fn connect_for_environment(
    directories: &CoterieDirectories,
    entry: &ActiveRunEntry,
) -> Result<SupervisorClient, SupervisorError> {
    let agent_id = env::var_os("COTERIE_AGENT_ID");
    let session_id = env::var_os("COTERIE_SESSION_ID");
    let token = env::var_os("COTERIE_TOKEN");
    let socket = checked_socket_path(directories, entry.run_id)?;
    match (agent_id, session_id, token) {
        (None, None, None) => {
            SupervisorClient::connect_operator_at(&socket, entry).await
        }
        (Some(agent_id), Some(session_id), Some(token)) => {
            let agent_id =
                parse_agent_environment(agent_id, "COTERIE_AGENT_ID")?;
            let session_id =
                parse_agent_environment(session_id, "COTERIE_SESSION_ID")?;
            let token = parse_agent_environment(token, "COTERIE_TOKEN")?;
            if let Some(run_id) = env::var_os("COTERIE_RUN_ID") {
                let run_id: RunId =
                    parse_agent_environment(run_id, "COTERIE_RUN_ID")?;
                if run_id != entry.run_id {
                    return Err(SupervisorError::AgentEnvironmentRunMismatch {
                        expected: entry.run_id,
                        found: run_id,
                    });
                }
            }
            SupervisorClient::connect_agent_at(
                &socket, entry, agent_id, session_id, token,
            )
            .await
        }
        _ => Err(SupervisorError::IncompleteAgentEnvironment),
    }
}

fn parse_agent_environment<T>(
    value: std::ffi::OsString,
    variable: &'static str,
) -> Result<T, SupervisorError>
where
    T: std::str::FromStr,
{
    value
        .into_string()
        .ok()
        .and_then(|value| value.parse().ok())
        .ok_or(SupervisorError::InvalidAgentEnvironment { variable })
}

fn public_request(
    command: CliCommand,
) -> (RpcRequest, Option<OperationId>, bool) {
    match command {
        CliCommand::Status => (RpcRequest::Status, None, false),
        CliCommand::Whoami => (RpcRequest::Whoami, None, false),
        CliCommand::Prime => (RpcRequest::Prime, None, false),
        CliCommand::Task(arguments) => match arguments.command {
            TaskCommand::Create(arguments) => {
                let operation_id = arguments
                    .mutation
                    .operation_id
                    .unwrap_or_else(OperationId::generate);
                (
                    RpcRequest::TaskCreate {
                        operation_id,
                        description: arguments
                            .description
                            .unwrap_or_else(|| arguments.title.clone()),
                        title: arguments.title,
                        project: arguments.project,
                        group: arguments.group,
                        dependencies: arguments.dependencies,
                    },
                    Some(operation_id),
                    false,
                )
            }
            TaskCommand::Ready => (RpcRequest::TaskReady, None, false),
            TaskCommand::Close(arguments) => {
                let operation_id = arguments
                    .mutation
                    .operation_id
                    .unwrap_or_else(OperationId::generate);
                (
                    RpcRequest::TaskClose {
                        operation_id,
                        task_id: arguments.task_id,
                        summary: arguments.summary,
                    },
                    Some(operation_id),
                    false,
                )
            }
        },
        CliCommand::Spawn(arguments) => {
            let operation_id = arguments
                .mutation
                .operation_id
                .unwrap_or_else(OperationId::generate);
            (
                RpcRequest::Spawn {
                    operation_id,
                    role: arguments.role,
                    task_id: arguments.task,
                },
                Some(operation_id),
                false,
            )
        }
        CliCommand::Finish(arguments) => {
            let operation_id = arguments
                .mutation
                .operation_id
                .unwrap_or_else(OperationId::generate);
            let status = match arguments.status {
                CliFinishStatus::Completed => FinishStatus::Completed,
                CliFinishStatus::Failed => FinishStatus::Failed,
            };
            (
                RpcRequest::Finish {
                    operation_id,
                    status,
                    summary: arguments.summary,
                },
                Some(operation_id),
                false,
            )
        }
        CliCommand::Send(arguments) => {
            let operation_id = arguments
                .mutation
                .operation_id
                .unwrap_or_else(OperationId::generate);
            (
                RpcRequest::Send {
                    operation_id,
                    recipient: arguments.recipient,
                    message: arguments.message,
                },
                Some(operation_id),
                false,
            )
        }
        CliCommand::Inbox(arguments) => match arguments.command {
            None => (
                RpcRequest::Inbox {
                    after: arguments.after,
                },
                None,
                false,
            ),
            Some(InboxCommand::Ack(arguments)) => {
                let operation_id = arguments
                    .mutation
                    .operation_id
                    .unwrap_or_else(OperationId::generate);
                (
                    RpcRequest::InboxAcknowledge {
                        operation_id,
                        through: arguments.through,
                    },
                    Some(operation_id),
                    false,
                )
            }
        },
        CliCommand::Logs(arguments) => (
            RpcRequest::Logs {
                agent: arguments.agent,
            },
            None,
            false,
        ),
        CliCommand::Events(arguments) => (
            RpcRequest::Events {
                after: arguments.after,
                limit: arguments.limit,
            },
            None,
            false,
        ),
        CliCommand::Stop(arguments) => {
            let operation_id =
                arguments.operation_id.unwrap_or_else(OperationId::generate);
            (
                RpcRequest::Shutdown { operation_id },
                Some(operation_id),
                true,
            )
        }
        CliCommand::Supervisor(_)
        | CliCommand::SupervisorConnect
        | CliCommand::SupervisorShutdown => unreachable!(
            "private commands are dispatched before public request conversion"
        ),
    }
}

fn render_public_response(
    json_output: bool,
    operation_id: Option<OperationId>,
    response: &RpcResponse,
) -> Result<crate::cli::ExitCategory, SupervisorError> {
    let mut data = serde_json::to_value(response)?;
    if let Some(object) = data.as_object_mut() {
        object.remove("result");
        object.remove("operation_id");
        if matches!(response, RpcResponse::ShuttingDown { .. }) {
            object.insert("status".to_owned(), json!("stopped"));
        }
    }
    let mut stdout = io::stdout().lock();
    let mut stderr = io::stderr().lock();
    if json_output {
        match operation_id {
            Some(operation_id) => crate::cli::render_json_mutation_success(
                &mut stdout,
                &mut stderr,
                operation_id,
                &data,
            ),
            None => {
                crate::cli::render_json_success(&mut stdout, &mut stderr, &data)
            }
        }
    } else {
        crate::cli::render_human_success(&mut stdout, &mut stderr, &data)
    }
    .map_err(Into::into)
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
    let mut sessions = runtime_sessions(&run_directories.state);
    let mut workspaces = runtime_workspaces();
    let reconciled_at = unix_timestamp()?;
    workspaces.reconcile_after_restart(
        &mut store,
        active.run_id,
        reconciled_at,
    )?;
    sessions.reconcile_after_restart(
        &mut store,
        active.run_id,
        reconciled_at,
    )?;
    let serve_result = serve_listener(
        listener,
        active.clone(),
        &run_directories.state,
        &socket_path,
        &mut store,
        &mut sessions,
        &mut workspaces,
    )
    .await;
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
    run_state_directory: &Path,
    socket_path: &Path,
    store: &mut Store,
    sessions: &mut AgentSessionSupervisor<FakeProvider>,
    workspaces: &mut WorkspaceSupervisor<FakeWorkspace>,
) -> Result<(), SupervisorError> {
    let (shutdown_tx, mut shutdown_rx) = mpsc::channel(1);
    let (command_tx, mut command_rx) = mpsc::channel(16);
    let mut connections = JoinSet::new();
    let mut foreground = ForegroundCoordination::default();
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
                handle_command(
                    store,
                    sessions,
                    workspaces,
                    RuntimePaths {
                        run_state_directory,
                        socket_path,
                    },
                    active.run_id,
                    command,
                    &mut foreground,
                );
                if foreground.retire_without_response {
                    break;
                }
            }
            _ = sleep_until_pending_shutdown(&foreground.pending_shutdown),
                if foreground.pending_shutdown.is_some() =>
            {
                let pending = foreground.pending_shutdown
                    .take()
                    .expect("the guarded shutdown should be pending");
                let failure = RpcFailure::new(
                    RpcFailureCode::Unavailable,
                    format!(
                        "timed out waiting for foreground processes before stopping run {}",
                        active.run_id
                    ),
                );
                for response in pending.responses {
                    let _request_may_have_disconnected =
                        response.send(Err(failure.clone()));
                }
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

fn runtime_sessions(
    run_state_directory: &Path,
) -> AgentSessionSupervisor<FakeProvider> {
    let running = SessionObservation {
        lifecycle: LifecycleState::Running,
        activity: ActivityState::Idle,
        exit: None,
    };
    let scripts =
        (0..compiled_defaults().limits.max_agents_per_run).map(|_| {
            FakeScript::new([
                FakeEvent::observation(running),
                FakeEvent::output(b"{\"type\":\"session.ready\"}\n"),
            ])
        });
    AgentSessionSupervisor::new(FakeProvider::new(scripts), run_state_directory)
}

fn runtime_workspaces() -> WorkspaceSupervisor<FakeWorkspace> {
    WorkspaceSupervisor::new(FakeWorkspace::new())
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
    Dispatch {
        caller: AuthenticatedCaller,
        request: RpcRequest,
        response: oneshot::Sender<Result<RpcResponse, RpcFailure>>,
    },
}

struct ForegroundControlWaiter {
    scope: SessionScope,
    response: oneshot::Sender<Result<RpcResponse, RpcFailure>>,
}

struct PendingShutdown {
    operation_id: OperationId,
    responses: Vec<oneshot::Sender<Result<RpcResponse, RpcFailure>>>,
    deadline: Instant,
}

#[derive(Default)]
struct ForegroundCoordination {
    controls: BTreeMap<SessionId, ForegroundControlWaiter>,
    pending_shutdown: Option<PendingShutdown>,
    retire_without_response: bool,
}

async fn sleep_until_pending_shutdown(pending: &Option<PendingShutdown>) {
    let deadline = pending
        .as_ref()
        .map_or_else(Instant::now, |shutdown| shutdown.deadline);
    tokio::time::sleep_until(deadline).await;
}

fn handle_command(
    store: &mut Store,
    sessions: &mut AgentSessionSupervisor<FakeProvider>,
    workspaces: &mut WorkspaceSupervisor<FakeWorkspace>,
    paths: RuntimePaths<'_>,
    run_id: RunId,
    command: SupervisorCommand,
    foreground: &mut ForegroundCoordination,
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
            begin_shutdown(store, run_id, operation_id, response, foreground);
        }
        SupervisorCommand::Dispatch {
            caller,
            request,
            response,
        } => {
            if let RpcRequest::WaitForegroundControl { scope } = request {
                register_foreground_control(
                    store,
                    run_id,
                    &caller,
                    scope,
                    response,
                    &mut foreground.controls,
                    foreground.pending_shutdown.is_some(),
                );
                return;
            }
            let ended_scope = match &request {
                RpcRequest::ForegroundExited { scope, .. }
                | RpcRequest::ForegroundLaunchFailed { scope } => Some(*scope),
                _ => None,
            };
            let result = execute_request(
                store, sessions, workspaces, paths, run_id, &caller, request,
            );
            let succeeded = result.is_ok();
            let _request_may_have_disconnected = response.send(result);
            if succeeded && let Some(scope) = ended_scope {
                release_foreground_control(&mut foreground.controls, scope);
                finish_shutdown_if_ready(store, run_id, foreground);
            }
        }
    }
}

fn begin_shutdown(
    store: &mut Store,
    run_id: RunId,
    operation_id: OperationId,
    response: oneshot::Sender<Result<RpcResponse, RpcFailure>>,
    foreground: &mut ForegroundCoordination,
) {
    if let Some(pending) = &mut foreground.pending_shutdown {
        if pending.operation_id == operation_id {
            pending.responses.push(response);
        } else {
            let _request_may_have_disconnected = response.send(Err(conflict(
                "another run shutdown is already waiting for foreground processes",
            )));
        }
        return;
    }
    match active_foreground_scopes(store, run_id) {
        Ok(scopes) if scopes.is_empty() => {
            let result = persist_shutdown(store, run_id, operation_id)
                .map_err(supervisor_rpc_failure);
            let stopped = result.is_ok();
            if response.send(result).is_err() && stopped {
                foreground.retire_without_response = true;
            }
        }
        Ok(scopes) => {
            foreground.pending_shutdown = Some(PendingShutdown {
                operation_id,
                responses: vec![response],
                deadline: Instant::now() + FOREGROUND_SHUTDOWN_TIMEOUT,
            });
            request_foreground_termination(&mut foreground.controls, &scopes);
        }
        Err(error) => {
            let _request_may_have_disconnected =
                response.send(Err(rpc_state_failure(error)));
        }
    }
}

fn register_foreground_control(
    store: &mut Store,
    run_id: RunId,
    caller: &AuthenticatedCaller,
    scope: SessionScope,
    response: oneshot::Sender<Result<RpcResponse, RpcFailure>>,
    foreground_controls: &mut BTreeMap<SessionId, ForegroundControlWaiter>,
    shutdown_pending: bool,
) {
    let validation = require_operator(
        caller,
        "only the foreground operator can await process control",
    )
    .and_then(|()| require_foreground_scope(store, run_id, scope))
    .and_then(|()| {
        let session = store
            .transaction(|repositories| repositories.session(scope.session_id))
            .map_err(rpc_state_failure)?
            .ok_or_else(|| not_found("the foreground session does not exist"))?;
        if session.process_owner != SessionProcessOwner::Foreground {
            return Err(conflict(
                "the session process is not controlled by the foreground wrapper",
            ));
        }
        if session.state.is_terminal() {
            return Err(conflict("the foreground session has already ended"));
        }
        Ok(())
    });
    if let Err(error) = validation {
        let _request_may_have_disconnected = response.send(Err(error));
        return;
    }
    if shutdown_pending {
        let _request_may_have_disconnected =
            response.send(Ok(RpcResponse::ForegroundTerminationRequested {
                session_id: scope.session_id,
            }));
        return;
    }
    if foreground_controls
        .get(&scope.session_id)
        .is_some_and(|waiter| !waiter.response.is_closed())
    {
        let _request_may_have_disconnected = response.send(Err(conflict(
            "the foreground session already has an active control connection",
        )));
        return;
    }
    foreground_controls.insert(
        scope.session_id,
        ForegroundControlWaiter { scope, response },
    );
}

fn request_foreground_termination(
    foreground_controls: &mut BTreeMap<SessionId, ForegroundControlWaiter>,
    scopes: &[SessionScope],
) {
    for scope in scopes {
        let Some(waiter) = foreground_controls.remove(&scope.session_id) else {
            continue;
        };
        let result = if waiter.scope == *scope {
            Ok(RpcResponse::ForegroundTerminationRequested {
                session_id: scope.session_id,
            })
        } else {
            Err(conflict(
                "the foreground control connection has a stale session generation",
            ))
        };
        let _foreground_may_have_disconnected = waiter.response.send(result);
    }
}

fn release_foreground_control(
    foreground_controls: &mut BTreeMap<SessionId, ForegroundControlWaiter>,
    scope: SessionScope,
) {
    let Some(waiter) = foreground_controls.remove(&scope.session_id) else {
        return;
    };
    let _foreground_may_have_disconnected =
        waiter.response.send(Err(conflict(
            "the foreground session ended without a process-control request",
        )));
}

fn finish_shutdown_if_ready(
    store: &mut Store,
    run_id: RunId,
    foreground: &mut ForegroundCoordination,
) {
    if foreground.pending_shutdown.is_none() {
        return;
    }
    match active_foreground_scopes(store, run_id) {
        Ok(scopes) if scopes.is_empty() => {
            let pending = foreground
                .pending_shutdown
                .take()
                .expect("the checked shutdown should still be pending");
            let result = persist_shutdown(store, run_id, pending.operation_id)
                .map_err(supervisor_rpc_failure);
            let stopped = result.is_ok();
            let mut delivered = false;
            for response in pending.responses {
                delivered |= response.send(result.clone()).is_ok();
            }
            if !delivered && stopped {
                foreground.retire_without_response = true;
            }
        }
        Ok(_) => {}
        Err(error) => {
            let pending = foreground
                .pending_shutdown
                .take()
                .expect("the checked shutdown should still be pending");
            let failure = rpc_state_failure(error);
            for response in pending.responses {
                let _request_may_have_disconnected =
                    response.send(Err(failure.clone()));
            }
        }
    }
}

fn active_foreground_scopes(
    store: &mut Store,
    run_id: RunId,
) -> Result<Vec<SessionScope>, StoreError> {
    store.transaction(|repositories| {
        Ok(repositories
            .sessions(run_id)?
            .into_iter()
            .filter(|session| {
                session.process_owner == SessionProcessOwner::Foreground
                    && !session.state.is_terminal()
            })
            .map(|session| SessionScope {
                run_id: session.run_id,
                agent_id: session.agent_id,
                session_id: session.id,
                generation: session.generation,
            })
            .collect())
    })
}

fn supervisor_rpc_failure(error: SupervisorError) -> RpcFailure {
    match error {
        SupervisorError::State(error) => rpc_state_failure(error),
        error => RpcFailure::new(RpcFailureCode::Internal, error.to_string()),
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct LaunchIntent {
    agent_id: AgentId,
    session_id: SessionId,
    generation: i64,
    already_active: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct SpawnIntent {
    agent_id: AgentId,
    session_id: SessionId,
    assignment_id: AssignmentId,
    task_id: TaskId,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct MessageIntent {
    message_id: MessageId,
    sequence: i64,
}

struct SpawnRuntime<'a> {
    store: &'a mut Store,
    sessions: &'a mut AgentSessionSupervisor<FakeProvider>,
    workspaces: &'a mut WorkspaceSupervisor<FakeWorkspace>,
    run_state_directory: &'a Path,
    socket_path: &'a Path,
    run_id: RunId,
}

#[derive(Clone, Copy)]
struct RuntimePaths<'a> {
    run_state_directory: &'a Path,
    socket_path: &'a Path,
}

fn execute_request(
    store: &mut Store,
    sessions: &mut AgentSessionSupervisor<FakeProvider>,
    workspaces: &mut WorkspaceSupervisor<FakeWorkspace>,
    paths: RuntimePaths<'_>,
    run_id: RunId,
    caller: &AuthenticatedCaller,
    request: RpcRequest,
) -> Result<RpcResponse, RpcFailure> {
    match request {
        RpcRequest::Ping => Ok(RpcResponse::Pong { run_id }),
        RpcRequest::LaunchForeground {
            operation_id,
            token,
        } => launch_foreground(store, run_id, caller, operation_id, &token),
        RpcRequest::ForegroundStarted { scope, process_id } => {
            observe_foreground_started(store, run_id, caller, scope, process_id)
        }
        RpcRequest::WaitForegroundControl { .. } => Err(RpcFailure::new(
            RpcFailureCode::Internal,
            "foreground process control was routed through the wrong command path",
        )),
        RpcRequest::ForegroundExited {
            scope,
            code,
            signal,
        } => observe_foreground_ended(
            store,
            run_id,
            caller,
            scope,
            ForegroundEnd::Exited { code, signal },
        ),
        RpcRequest::ForegroundLaunchFailed { scope } => {
            observe_foreground_ended(
                store,
                run_id,
                caller,
                scope,
                ForegroundEnd::LaunchFailed,
            )
        }
        RpcRequest::ForegroundObservationLost { scope } => {
            observe_foreground_ended(
                store,
                run_id,
                caller,
                scope,
                ForegroundEnd::ObservationLost,
            )
        }
        RpcRequest::Status => status(store, run_id, caller),
        RpcRequest::Whoami => whoami(store, run_id, caller),
        RpcRequest::Prime => prime(store, run_id, caller),
        RpcRequest::TaskCreate {
            operation_id,
            title,
            description,
            project,
            group,
            dependencies,
        } => create_task(
            store,
            run_id,
            caller,
            operation_id,
            title,
            description,
            project,
            group,
            dependencies,
        ),
        RpcRequest::TaskReady => ready_tasks(store, run_id, caller),
        RpcRequest::TaskClose {
            operation_id,
            task_id,
            summary,
        } => close_task(store, run_id, caller, operation_id, task_id, summary),
        RpcRequest::Spawn {
            operation_id,
            role,
            task_id,
        } => spawn_agent(
            SpawnRuntime {
                store,
                sessions,
                workspaces,
                run_state_directory: paths.run_state_directory,
                socket_path: paths.socket_path,
                run_id,
            },
            caller,
            operation_id,
            role,
            task_id,
        ),
        RpcRequest::Finish {
            operation_id,
            status,
            summary,
        } => finish_assignment(
            store,
            run_id,
            caller,
            operation_id,
            status,
            summary,
        ),
        RpcRequest::Send {
            operation_id,
            recipient,
            message,
        } => send_message(
            store,
            run_id,
            caller,
            operation_id,
            recipient,
            message,
        ),
        RpcRequest::Inbox { after } => inbox(store, run_id, caller, after),
        RpcRequest::InboxAcknowledge {
            operation_id,
            through,
        } => acknowledge_inbox(store, run_id, caller, operation_id, through),
        RpcRequest::Logs { agent } => {
            logs(store, paths.run_state_directory, run_id, caller, &agent)
        }
        RpcRequest::Events { after, limit } => {
            events(store, run_id, caller, after, limit)
        }
        RpcRequest::Shutdown { .. } => Err(RpcFailure::new(
            RpcFailureCode::Internal,
            "shutdown was routed through the wrong command path",
        )),
    }
}

fn launch_foreground(
    store: &mut Store,
    run_id: RunId,
    caller: &AuthenticatedCaller,
    operation_id: OperationId,
    token: &AgentToken,
) -> Result<RpcResponse, RpcFailure> {
    require_operator(
        caller,
        "only the operator can launch the foreground agent",
    )?;
    let archetype = builtin_standard();
    let role = archetype.lead.to_owned();
    let role_definition = archetype.role(&role).ok_or_else(|| {
        RpcFailure::new(
            RpcFailureCode::Internal,
            "the active archetype has no designated foreground role",
        )
    })?;
    if role_launch_mode(role_definition.mode) != LaunchMode::Interactive {
        return Err(RpcFailure::new(
            RpcFailureCode::Internal,
            "the designated foreground role is not interactive",
        ));
    }
    let provider = role_definition.provider.to_owned();
    let bootstrap_instruction = bootstrap_instruction(run_id, &role);
    let now = rpc_timestamp()?;
    let agent_id = AgentId::generate();
    let session_id = SessionId::generate();
    let mutation = Mutation {
        id: operation_id,
        run_id,
        kind: "agent.launch_foreground".to_owned(),
        actor_agent_id: None,
        request: json!({"role": role}),
        created_at: now,
    };
    let outcome = store
        .mutate(&mutation, |repositories| {
            let existing = repositories
                .agents(run_id)?
                .into_iter()
                .find(|agent| agent.role == role);
            let (agent_id, generation) = if let Some(agent) = existing {
                let latest = repositories
                    .latest_session_for_agent(run_id, agent.id)?
                    .ok_or_else(|| StoreError::CorruptAgentState {
                        id: agent.id,
                        reason: "the agent has no provider session".to_owned(),
                    })?;
                if !agent.state.is_terminal() {
                    return Ok(LaunchIntent {
                        agent_id: agent.id,
                        session_id: latest.id,
                        generation: agent.generation,
                        already_active: true,
                    });
                }
                let generation =
                    agent.generation.checked_add(1).ok_or_else(|| {
                        StoreError::CorruptAgentState {
                            id: agent.id,
                            reason: "the session generation is exhausted"
                                .to_owned(),
                        }
                    })?;
                if !repositories.start_agent_generation(
                    run_id,
                    agent.id,
                    agent.generation,
                    generation,
                )? {
                    return Err(StoreError::CorruptAgentState {
                        id: agent.id,
                        reason: "the terminal generation could not be replaced"
                            .to_owned(),
                    });
                }
                (agent.id, generation)
            } else {
                repositories.insert_agent(&AgentRecord {
                    id: agent_id,
                    run_id,
                    role: role.clone(),
                    generation: 0,
                    state: LifecycleState::Starting,
                    created_at: now,
                })?;
                repositories.append_event(&NewEvent {
                    run_id,
                    kind: EventKind::AgentCreated,
                    actor: event_actor(caller),
                    subject: agent_id.to_string(),
                    project_id: None,
                    agent_id: Some(agent_id),
                    task_id: None,
                    operation_id: Some(operation_id),
                    correlation_id: None,
                    causation_id: None,
                    data: json!({
                        "generation": 0,
                        "role": role,
                        "state": LifecycleState::Starting.as_str(),
                    }),
                    summary: format!("Created foreground agent {agent_id}."),
                    created_at: now,
                })?;
                (agent_id, 0)
            };
            let scope = SessionScope {
                run_id,
                agent_id,
                session_id,
                generation,
            };
            repositories.insert_session(&SessionRecord {
                id: session_id,
                run_id,
                agent_id,
                generation,
                provider: provider.clone(),
                provider_session_id: None,
                reconciliation_state: ExternalResourceState::Desired,
                state: LifecycleState::Starting,
                transcript_path: TranscriptStore::relative_path(session_id),
                created_at: now,
                ended_at: None,
                reconciled_at: None,
                process_owner: SessionProcessOwner::Foreground,
            })?;
            repositories.activate_session_credential(
                &SessionCredentialRecord {
                    session_id,
                    run_id,
                    agent_id,
                    generation,
                    token_verifier: token.verifier(scope),
                    created_at: now,
                    revoked_at: None,
                },
            )?;
            repositories.append_event(&NewEvent {
                run_id,
                kind: EventKind::SessionStarted,
                actor: event_actor(caller),
                subject: session_id.to_string(),
                project_id: None,
                agent_id: Some(agent_id),
                task_id: None,
                operation_id: Some(operation_id),
                correlation_id: None,
                causation_id: None,
                data: json!({
                    "generation": generation,
                    "provider": provider,
                    "state": LifecycleState::Starting.as_str(),
                }),
                summary: format!(
                    "Started session {session_id} for agent {agent_id}."
                ),
                created_at: now,
            })?;
            let claim = repositories.record_session_reconciliation_state(
                scope,
                ExternalResourceState::Unknown,
                now,
            )?;
            if claim != SessionTransitionOutcome::Applied {
                return Err(StoreError::CorruptAgentState {
                    id: agent_id,
                    reason: "the foreground launch intent could not be claimed"
                        .to_owned(),
                });
            }
            repositories.append_event(&NewEvent {
                run_id,
                kind: EventKind::SessionReconciliationChanged,
                actor: event_actor(caller),
                subject: session_id.to_string(),
                project_id: None,
                agent_id: Some(agent_id),
                task_id: None,
                operation_id: Some(operation_id),
                correlation_id: None,
                causation_id: None,
                data: json!({
                    "previous_state": ExternalResourceState::Desired.as_str(),
                    "reason": "launch_claimed",
                    "state": ExternalResourceState::Unknown.as_str(),
                }),
                summary: format!(
                    "Claimed the provider launch for foreground session {session_id}."
                ),
                created_at: now,
            })?;
            Ok(LaunchIntent {
                agent_id,
                session_id,
                generation,
                already_active: false,
            })
        })
        .map_err(rpc_state_failure)?;
    let intent = match outcome {
        MutationOutcome::Applied(intent) => intent,
        MutationOutcome::Replayed(intent) => {
            let scope = SessionScope {
                run_id,
                agent_id: intent.agent_id,
                session_id: intent.session_id,
                generation: intent.generation,
            };
            let replayable = store
                .transaction(|repositories| {
                    let session = repositories.session(intent.session_id)?;
                    if matches!(
                        session,
                        Some(SessionRecord {
                            reconciliation_state:
                                ExternalResourceState::Desired,
                            state: LifecycleState::Starting,
                            ..
                        })
                    ) {
                        repositories.replace_desired_session_credential(
                            &SessionCredentialRecord {
                                session_id: intent.session_id,
                                run_id,
                                agent_id: intent.agent_id,
                                generation: intent.generation,
                                token_verifier: token.verifier(scope),
                                created_at: now,
                                revoked_at: None,
                            },
                        )?;
                        repositories.record_session_reconciliation_state(
                            scope,
                            ExternalResourceState::Unknown,
                            now,
                        )?;
                        repositories.append_event(&NewEvent {
                            run_id,
                            kind: EventKind::SessionReconciliationChanged,
                            actor: event_actor(caller),
                            subject: intent.session_id.to_string(),
                            project_id: None,
                            agent_id: Some(intent.agent_id),
                            task_id: None,
                            operation_id: Some(operation_id),
                            correlation_id: None,
                            causation_id: None,
                            data: json!({
                                "previous_state": ExternalResourceState::Desired.as_str(),
                                "reason": "launch_claimed",
                                "state": ExternalResourceState::Unknown.as_str(),
                            }),
                            summary: format!(
                                "Claimed the provider launch for foreground session {}.",
                                intent.session_id
                            ),
                            created_at: now,
                        })?;
                        Ok(true)
                    } else {
                        Ok(false)
                    }
                })
                .map_err(rpc_state_failure)?;
            if !replayable {
                return Err(conflict(format!(
                    "foreground operation `{operation_id}` has already attempted its provider launch"
                )));
            }
            intent
        }
    };
    if intent.already_active {
        return Err(conflict(format!(
            "foreground session `{}` is already active",
            intent.session_id
        )));
    }
    Ok(RpcResponse::ForegroundPrepared {
        run_id,
        agent: summary_for_agent(store, run_id, intent.agent_id)?,
        session_id: intent.session_id,
        generation: intent.generation,
        bootstrap_instruction,
    })
}

fn observe_foreground_started(
    store: &mut Store,
    run_id: RunId,
    caller: &AuthenticatedCaller,
    scope: SessionScope,
    process_id: u32,
) -> Result<RpcResponse, RpcFailure> {
    require_operator(
        caller,
        "only the foreground operator can report provider startup",
    )?;
    require_foreground_scope(store, run_id, scope)?;
    let observed_at = rpc_timestamp()?;
    let provider_session_id = format!("process:{process_id}");
    store
        .transaction(|repositories| {
            let session =
                repositories.session(scope.session_id)?.ok_or_else(|| {
                    StoreError::CorruptAgentState {
                        id: scope.agent_id,
                        reason: "the foreground session disappeared".to_owned(),
                    }
                })?;
            let reconciliation = repositories
                .record_session_launch_observation(
                    scope,
                    &provider_session_id,
                    observed_at,
                )?;
            if reconciliation == SessionTransitionOutcome::Applied {
                repositories.append_event(&NewEvent {
                    run_id,
                    kind: EventKind::SessionReconciliationChanged,
                    actor: "foreground".to_owned(),
                    subject: scope.session_id.to_string(),
                    project_id: None,
                    agent_id: Some(scope.agent_id),
                    task_id: None,
                    operation_id: None,
                    correlation_id: None,
                    causation_id: None,
                    data: json!({
                        "previous_state": session.reconciliation_state.as_str(),
                        "provider_session_id": provider_session_id,
                        "state": ExternalResourceState::Observed.as_str(),
                    }),
                    summary: format!(
                        "Observed foreground provider session {}.",
                        scope.session_id
                    ),
                    created_at: observed_at,
                })?;
            }
            record_foreground_lifecycle(
                repositories,
                &session,
                scope,
                LifecycleState::Running,
                json!({"process_id": process_id}),
                observed_at,
            )?;
            Ok(())
        })
        .map_err(rpc_state_failure)?;
    Ok(RpcResponse::ForegroundObserved {
        session_id: scope.session_id,
        state: LifecycleState::Running.as_str().to_owned(),
    })
}

enum ForegroundEnd {
    Exited {
        code: Option<i32>,
        signal: Option<i32>,
    },
    LaunchFailed,
    ObservationLost,
}

fn observe_foreground_ended(
    store: &mut Store,
    run_id: RunId,
    caller: &AuthenticatedCaller,
    scope: SessionScope,
    end: ForegroundEnd,
) -> Result<RpcResponse, RpcFailure> {
    require_operator(
        caller,
        "only the foreground operator can report provider state",
    )?;
    require_foreground_scope(store, run_id, scope)?;
    let observed_at = rpc_timestamp()?;
    let (reconciliation, lifecycle, details) = match end {
        ForegroundEnd::Exited { code, signal } => (
            ExternalResourceState::Observed,
            LifecycleState::Exited,
            json!({"code": code, "signal": signal}),
        ),
        ForegroundEnd::LaunchFailed => (
            ExternalResourceState::Lost,
            LifecycleState::Lost,
            json!({"reason": "launch_failed"}),
        ),
        ForegroundEnd::ObservationLost => (
            ExternalResourceState::Unknown,
            LifecycleState::Unknown,
            json!({"reason": "observation_failed"}),
        ),
    };
    store
        .transaction(|repositories| {
            let session = repositories
                .session(scope.session_id)?
                .ok_or_else(|| StoreError::CorruptAgentState {
                    id: scope.agent_id,
                    reason: "the foreground session disappeared".to_owned(),
                })?;
            let transition = repositories
                .record_session_reconciliation_state(
                    scope,
                    reconciliation,
                    observed_at,
                )?;
            if transition == SessionTransitionOutcome::Applied {
                repositories.append_event(&NewEvent {
                    run_id,
                    kind: EventKind::SessionReconciliationChanged,
                    actor: "foreground".to_owned(),
                    subject: scope.session_id.to_string(),
                    project_id: None,
                    agent_id: Some(scope.agent_id),
                    task_id: None,
                    operation_id: None,
                    correlation_id: None,
                    causation_id: None,
                    data: json!({
                        "previous_state": session.reconciliation_state.as_str(),
                        "state": reconciliation.as_str(),
                    }),
                    summary: format!(
                        "Foreground session {} reconciliation changed from {} to {}.",
                        scope.session_id,
                        session.reconciliation_state,
                        reconciliation,
                    ),
                    created_at: observed_at,
                })?;
            }
            record_foreground_lifecycle(
                repositories,
                &session,
                scope,
                lifecycle,
                details,
                observed_at,
            )?;
            Ok(())
        })
        .map_err(rpc_state_failure)?;
    Ok(RpcResponse::ForegroundObserved {
        session_id: scope.session_id,
        state: lifecycle.as_str().to_owned(),
    })
}

fn require_foreground_scope(
    store: &mut Store,
    run_id: RunId,
    scope: SessionScope,
) -> Result<(), RpcFailure> {
    let session = store
        .transaction(|repositories| repositories.session(scope.session_id))
        .map_err(rpc_state_failure)?
        .ok_or_else(|| not_found("the foreground session does not exist"))?;
    if scope.run_id != run_id
        || session.run_id != scope.run_id
        || session.agent_id != scope.agent_id
        || session.generation != scope.generation
        || session.provider != "codex"
    {
        return Err(conflict(
            "the foreground observation does not match its session generation",
        ));
    }
    Ok(())
}

fn record_foreground_lifecycle(
    repositories: &crate::state::Repositories<'_, '_>,
    session: &SessionRecord,
    scope: SessionScope,
    lifecycle: LifecycleState,
    details: serde_json::Value,
    observed_at: i64,
) -> Result<(), StoreError> {
    let outcome =
        repositories.record_session_lifecycle(scope, lifecycle, observed_at)?;
    if outcome != SessionTransitionOutcome::Applied {
        return Ok(());
    }
    let session_event = repositories.append_event(&NewEvent {
        run_id: scope.run_id,
        kind: EventKind::SessionLifecycleChanged,
        actor: "foreground".to_owned(),
        subject: scope.session_id.to_string(),
        project_id: None,
        agent_id: Some(scope.agent_id),
        task_id: None,
        operation_id: None,
        correlation_id: None,
        causation_id: None,
        data: json!({
            "details": details,
            "generation": scope.generation,
            "previous_state": session.state.as_str(),
            "provider": session.provider,
            "state": lifecycle.as_str(),
        }),
        summary: format!(
            "Foreground session {} changed from {} to {}.",
            scope.session_id, session.state, lifecycle
        ),
        created_at: observed_at,
    })?;
    repositories.append_event(&NewEvent {
        run_id: scope.run_id,
        kind: EventKind::AgentLifecycleChanged,
        actor: "foreground".to_owned(),
        subject: scope.agent_id.to_string(),
        project_id: None,
        agent_id: Some(scope.agent_id),
        task_id: None,
        operation_id: None,
        correlation_id: Some(session_event.id),
        causation_id: Some(session_event.id),
        data: json!({
            "generation": scope.generation,
            "previous_state": session.state.as_str(),
            "state": lifecycle.as_str(),
        }),
        summary: format!(
            "Foreground agent {} changed from {} to {}.",
            scope.agent_id, session.state, lifecycle
        ),
        created_at: observed_at,
    })?;
    Ok(())
}

fn status(
    store: &mut Store,
    run_id: RunId,
    caller: &AuthenticatedCaller,
) -> Result<RpcResponse, RpcFailure> {
    require_operator(caller, "only the operator can inspect full run status")?;
    let (run, projects, agents, tasks) = store
        .transaction(|repositories| {
            Ok((
                repositories.run(run_id)?,
                repositories.projects(run_id)?,
                repositories.agents(run_id)?,
                repositories.tasks(run_id)?,
            ))
        })
        .map_err(rpc_state_failure)?;
    let run = run.ok_or_else(|| not_found("the active run does not exist"))?;
    Ok(RpcResponse::Status {
        run_id,
        status: run.status,
        projects: projects.into_iter().map(project_summary).collect(),
        agents: summarize_agents(&agents),
        tasks: task_counts(&tasks),
    })
}

fn whoami(
    store: &mut Store,
    run_id: RunId,
    caller: &AuthenticatedCaller,
) -> Result<RpcResponse, RpcFailure> {
    let identity = caller_summary(store, run_id, caller)?;
    Ok(RpcResponse::Identity {
        run_id,
        channel: identity.channel,
        agent: identity.agent,
    })
}

fn prime(
    store: &mut Store,
    run_id: RunId,
    caller: &AuthenticatedCaller,
) -> Result<RpcResponse, RpcFailure> {
    let identity = caller_summary(store, run_id, caller)?;
    let (projects, agents, tasks, ready, active_task) = store
        .transaction(|repositories| {
            let active_task = match caller.agent_id() {
                Some(agent_id) => repositories
                    .active_assignment_for_agent(run_id, agent_id)?
                    .and_then(|assignment| {
                        repositories.task(assignment.task_id).transpose()
                    })
                    .transpose()?,
                None => None,
            };
            Ok((
                repositories.projects(run_id)?,
                repositories.agents(run_id)?,
                repositories.tasks(run_id)?,
                repositories.ready_tasks(run_id)?,
                active_task,
            ))
        })
        .map_err(rpc_state_failure)?;
    let project_map = project_aliases(&projects);
    let peers = summarize_agents(&agents)
        .into_iter()
        .filter(|agent| Some(agent.id) != caller.agent_id())
        .collect();
    let ready_tasks = summarize_tasks(store, &project_map, ready)?;
    let tasks = summarize_tasks(store, &project_map, tasks)?;
    let active_task = active_task
        .map(|task| summarize_task(store, &project_map, task))
        .transpose()?;
    Ok(RpcResponse::Prime {
        identity,
        projects: projects.into_iter().map(project_summary).collect(),
        peers,
        tasks,
        ready_tasks,
        active_task: active_task.map(Box::new),
        commands: available_commands(store, run_id, caller)?,
    })
}

#[allow(clippy::too_many_arguments)]
fn create_task(
    store: &mut Store,
    run_id: RunId,
    caller: &AuthenticatedCaller,
    operation_id: OperationId,
    title: String,
    description: String,
    project_alias: String,
    group: Option<String>,
    dependencies: Vec<TaskId>,
) -> Result<RpcResponse, RpcFailure> {
    require_capability(store, run_id, caller, "task", "create")?;
    if title.trim().is_empty() {
        return Err(invalid_argument("task title cannot be empty"));
    }
    if description.trim().is_empty() {
        return Err(invalid_argument("task description cannot be empty"));
    }
    if group.as_ref().is_some_and(|name| name.trim().is_empty()) {
        return Err(invalid_argument("task group cannot be empty"));
    }
    let project = store
        .transaction(|repositories| {
            repositories.project_by_alias(run_id, &project_alias)
        })
        .map_err(rpc_state_failure)?
        .ok_or_else(|| {
            not_found(format!("project `{project_alias}` is not attached"))
        })?;
    let task_id = TaskId::generate();
    let now = rpc_timestamp()?;
    let actor_agent_id = caller.agent_id();
    let mutation = Mutation {
        id: operation_id,
        run_id,
        kind: "task.create".to_owned(),
        actor_agent_id,
        request: json!({
            "title": title,
            "description": description,
            "project": project_alias,
            "group": group,
            "dependencies": dependencies,
        }),
        created_at: now,
    };
    let outcome = store
        .mutate(&mutation, |repositories| {
            for dependency_id in &dependencies {
                let dependency = repositories.task(*dependency_id)?;
                if !dependency.is_some_and(|task| task.run_id == run_id) {
                    return Err(StoreError::CorruptTaskState {
                        id: *dependency_id,
                        reason: "a requested dependency is not in this run"
                            .to_owned(),
                    });
                }
            }
            let group_id = if let Some(name) = group.as_deref() {
                Some(
                    repositories
                        .task_group_by_name(run_id, name)?
                        .map_or_else(
                            || {
                                repositories
                                    .insert_named_task_group(run_id, name, now)
                            },
                            |group| Ok(group.id),
                        )?,
                )
            } else {
                None
            };
            repositories.insert_task(&TaskRecord {
                id: task_id,
                run_id,
                project_id: project.id,
                group_id,
                title: title.clone(),
                description: description.clone(),
                status: TaskStatus::Open,
                result: None,
                created_at: now,
                updated_at: now,
            })?;
            for dependency_task_id in &dependencies {
                repositories.insert_dependency(&DependencyRecord {
                    run_id,
                    task_id,
                    dependency_task_id: *dependency_task_id,
                    created_at: now,
                })?;
            }
            repositories.append_event(&NewEvent {
                run_id,
                kind: EventKind::TaskCreated,
                actor: event_actor(caller),
                subject: task_id.to_string(),
                project_id: Some(project.id),
                agent_id: actor_agent_id,
                task_id: Some(task_id),
                operation_id: Some(operation_id),
                correlation_id: None,
                causation_id: None,
                data: json!({
                    "dependencies": dependencies,
                    "group": group,
                    "status": TaskStatus::Open,
                    "title": title,
                }),
                summary: format!("Created task {task_id}."),
                created_at: now,
            })?;
            Ok(task_id)
        })
        .map_err(|error| match error {
            StoreError::CorruptTaskState { id, .. }
                if dependencies.contains(&id) =>
            {
                not_found(format!("dependency task `{id}` does not exist"))
            }
            error => rpc_state_failure(error),
        })?;
    let task_id = mutation_value(outcome);
    Ok(RpcResponse::TaskCreated {
        operation_id,
        task: task_by_id(store, run_id, task_id)?,
    })
}

fn ready_tasks(
    store: &mut Store,
    run_id: RunId,
    caller: &AuthenticatedCaller,
) -> Result<RpcResponse, RpcFailure> {
    require_capability(store, run_id, caller, "task", "read")?;
    let (projects, tasks) = store
        .transaction(|repositories| {
            Ok((
                repositories.projects(run_id)?,
                repositories.ready_tasks(run_id)?,
            ))
        })
        .map_err(rpc_state_failure)?;
    Ok(RpcResponse::ReadyTasks {
        tasks: summarize_tasks(store, &project_aliases(&projects), tasks)?,
    })
}

fn close_task(
    store: &mut Store,
    run_id: RunId,
    caller: &AuthenticatedCaller,
    operation_id: OperationId,
    task_id: TaskId,
    summary: String,
) -> Result<RpcResponse, RpcFailure> {
    require_capability(store, run_id, caller, "task", "close")?;
    if summary.trim().is_empty() {
        return Err(invalid_argument("closure summary cannot be empty"));
    }
    let result = store
        .transaction(|repositories| {
            if let Some(operation) = repositories.operation(operation_id)? {
                return Ok(operation
                    .request
                    .get("result")
                    .cloned()
                    .filter(|result| !result.is_null()));
            }
            let assignment_result =
                repositories.task(task_id)?.and_then(|task| task.result);
            Ok(Some(json!({
                "assignment_result": assignment_result,
                "validation_summary": summary,
            })))
        })
        .map_err(rpc_state_failure)?;
    let transition = TaskTransitionMutation {
        operation_id,
        run_id,
        actor_agent_id: caller.agent_id(),
        task_id,
        transition: TaskTransition::Close,
        result,
        summary: Some(summary),
        transitioned_at: rpc_timestamp()?,
    };
    let result = mutation_value(
        store
            .transition_task(&transition)
            .map_err(rpc_state_failure)?,
    );
    require_transition(result, task_id, "close")?;
    Ok(RpcResponse::TaskClosed {
        operation_id,
        task: task_by_id(store, run_id, task_id)?,
    })
}

fn spawn_agent(
    runtime: SpawnRuntime<'_>,
    caller: &AuthenticatedCaller,
    operation_id: OperationId,
    role: String,
    task_id: TaskId,
) -> Result<RpcResponse, RpcFailure> {
    let SpawnRuntime {
        store,
        sessions,
        workspaces,
        run_state_directory,
        socket_path,
        run_id,
    } = runtime;
    require_capability(store, run_id, caller, "spawn", &role)?;
    let archetype = builtin_standard();
    let role_definition = archetype
        .role(&role)
        .ok_or_else(|| not_found(format!("role `{role}` is not configured")))?;
    if role_definition.mode != RoleMode::Job {
        return Err(conflict(format!(
            "role `{role}` is not a background job role"
        )));
    }
    let replaying = store
        .transaction(|repositories| repositories.operation(operation_id))
        .map_err(rpc_state_failure)?
        .is_some();
    if !replaying {
        let (agents, task, readiness) = store
            .transaction(|repositories| {
                Ok((
                    repositories.agents(run_id)?,
                    repositories.task(task_id)?,
                    repositories.task_readiness(task_id)?,
                ))
            })
            .map_err(rpc_state_failure)?;
        task.filter(|task| task.run_id == run_id).ok_or_else(|| {
            not_found(format!("task `{task_id}` does not exist"))
        })?;
        if !readiness.is_some_and(|readiness| readiness.is_ready()) {
            return Err(conflict(format!("task `{task_id}` is not ready")));
        }
        let active_role_instances = agents
            .iter()
            .filter(|agent| agent.role == role && !agent.state.is_terminal())
            .count();
        if role_definition
            .max_instances
            .is_some_and(|limit| active_role_instances >= usize::from(limit))
        {
            return Err(conflict(format!(
                "role `{role}` has reached its active instance limit"
            )));
        }
        let limits = compiled_defaults().limits;
        if agents.len() >= usize::from(limits.max_agents_per_run)
            || agents
                .iter()
                .filter(|agent| !agent.state.is_terminal())
                .count()
                >= usize::from(limits.max_concurrent_agents)
        {
            return Err(conflict("the run has reached its agent limit"));
        }
    }
    let now = rpc_timestamp()?;
    let agent_id = AgentId::generate();
    let session_id = SessionId::generate();
    let assignment_id = AssignmentId::generate();
    let claim = ClaimTaskMutation {
        operation_id,
        run_id,
        actor_agent_id: caller.agent_id(),
        task_id,
        agent_id,
        assignment_id,
        claimed_at: now,
    };
    let mutation = Mutation {
        id: operation_id,
        run_id,
        kind: "agent.spawn".to_owned(),
        actor_agent_id: caller.agent_id(),
        request: json!({"role": role, "task_id": task_id}),
        created_at: now,
    };
    let outcome = store
        .mutate(&mutation, |repositories| {
            repositories.insert_agent(&AgentRecord {
                id: agent_id,
                run_id,
                role: role.clone(),
                generation: 0,
                state: LifecycleState::Starting,
                created_at: now,
            })?;
            repositories.append_event(&NewEvent {
                run_id,
                kind: EventKind::AgentCreated,
                actor: event_actor(caller),
                subject: agent_id.to_string(),
                project_id: None,
                agent_id: Some(agent_id),
                task_id: Some(task_id),
                operation_id: Some(operation_id),
                correlation_id: None,
                causation_id: None,
                data: json!({
                    "generation": 0,
                    "role": role,
                    "state": LifecycleState::Starting.as_str(),
                }),
                summary: format!("Created agent {agent_id}."),
                created_at: now,
            })?;
            let claim_result = repositories.compare_and_set_claim(&claim)?;
            repositories.append_task_claim_events(&claim, &claim_result)?;
            match claim_result {
                ClaimTaskResult::Claimed { assignment_id, .. } => {
                    let task = repositories.task(task_id)?.ok_or_else(|| {
                        StoreError::CorruptTaskState {
                            id: task_id,
                            reason: "the claimed task disappeared".to_owned(),
                        }
                    })?;
                    let project = repositories
                        .project(task.project_id)?
                        .ok_or_else(|| StoreError::CorruptTaskState {
                            id: task_id,
                            reason: "the target project disappeared".to_owned(),
                        })?;
                    let workspace = WorkspaceRecord {
                        assignment_id,
                        run_id,
                        project_id: task.project_id,
                        kind: workspace_policy_name(role_definition.workspace)
                            .to_owned(),
                        path: assignment_workspace_path(
                            run_state_directory,
                            role_definition.workspace,
                            &project,
                            assignment_id,
                        ),
                        state: ExternalResourceState::Desired,
                        base_commit: None,
                        result_commit: None,
                        target_commit: None,
                        created_at: now,
                        reconciled_at: None,
                    };
                    repositories.insert_workspace(&workspace)?;
                    repositories.append_event(&NewEvent {
                        run_id,
                        kind: EventKind::WorkspaceDesired,
                        actor: event_actor(caller),
                        subject: assignment_id.to_string(),
                        project_id: Some(task.project_id),
                        agent_id: Some(agent_id),
                        task_id: Some(task_id),
                        operation_id: Some(operation_id),
                        correlation_id: None,
                        causation_id: None,
                        data: json!({
                            "kind": workspace.kind,
                            "path": workspace.path,
                            "state": workspace.state.as_str(),
                        }),
                        summary: format!(
                            "Recorded desired workspace for assignment {assignment_id}."
                        ),
                        created_at: now,
                    })?;
                    Ok(SpawnIntent {
                        agent_id,
                        session_id,
                        assignment_id,
                        task_id,
                    })
                }
                ClaimTaskResult::Rejected(reason) => {
                    Err(StoreError::CorruptTaskState {
                        id: task_id,
                        reason: format!(
                            "ready task claim was unexpectedly rejected: {reason:?}"
                        ),
                    })
                }
            }
        })
        .map_err(rpc_state_failure)?;
    let intent = mutation_value(outcome);
    let (workspace, primary_project) = store
        .transaction(|repositories| {
            let workspace = repositories.workspace(intent.assignment_id)?;
            let primary_project = repositories
                .projects(run_id)?
                .into_iter()
                .find(|project| project.is_primary);
            Ok((workspace, primary_project))
        })
        .map_err(rpc_state_failure)?;
    let workspace = workspace.ok_or_else(|| {
        RpcFailure::new(
            RpcFailureCode::Internal,
            "the assignment workspace intent is missing",
        )
    })?;
    let primary_project = primary_project.ok_or_else(|| {
        RpcFailure::new(
            RpcFailureCode::Internal,
            "the run has no primary project",
        )
    })?;
    let workspace_state = workspaces
        .materialize(store, intent.assignment_id, now)
        .map_err(rpc_workspace_failure)?;
    if workspace_state != ExternalResourceState::Observed {
        return Err(conflict(format!(
            "workspace `{}` reconciliation state is `{workspace_state}`",
            intent.assignment_id
        )));
    }
    let launch = AgentLaunch {
        scope: SessionScope {
            run_id,
            agent_id: intent.agent_id,
            session_id: intent.session_id,
            generation: 0,
        },
        role: role.clone(),
        mode: LaunchMode::Job,
        working_directory: workspace.path,
        project_id: workspace.project_id,
        primary_project_root: primary_project.canonical_path,
        task_id: intent.task_id,
        socket_path: socket_path.to_path_buf(),
        bootstrap_instruction: bootstrap_instruction(run_id, &role),
        created_at: now,
    };
    if let Some(launched) = sessions
        .ensure_existing_launch(store, &launch, Some(intent.assignment_id))
        .map_err(rpc_session_failure)?
    {
        debug_assert_eq!(launched.scope, launch.scope);
        let _provider_token = launched.token;
    }
    drain_fake_events(sessions, store, intent.session_id, now)?;
    Ok(RpcResponse::Spawned {
        operation_id,
        agent: summary_for_agent(store, run_id, intent.agent_id)?,
        session_id: intent.session_id,
        assignment_id: intent.assignment_id,
        task_id: intent.task_id,
    })
}

fn finish_assignment(
    store: &mut Store,
    run_id: RunId,
    caller: &AuthenticatedCaller,
    operation_id: OperationId,
    status: FinishStatus,
    summary: String,
) -> Result<RpcResponse, RpcFailure> {
    let agent_id = caller.agent_id().ok_or_else(|| {
        RpcFailure::new(
            RpcFailureCode::PermissionDenied,
            "only an assigned agent can finish its work",
        )
    })?;
    if summary.trim().is_empty() {
        return Err(invalid_argument("finish summary cannot be empty"));
    }
    let assignment = store
        .transaction(|repositories| {
            let existing = repositories.operation(operation_id)?;
            if let Some(existing) = existing {
                let task_id = existing
                    .request
                    .get("task_id")
                    .and_then(serde_json::Value::as_str)
                    .and_then(|task_id| task_id.parse::<TaskId>().ok());
                return task_id
                    .map(|task_id| {
                        repositories.assignment_for_agent_task(
                            run_id, agent_id, task_id,
                        )
                    })
                    .transpose()
                    .map(Option::flatten);
            }
            repositories.active_assignment_for_agent(run_id, agent_id)
        })
        .map_err(rpc_state_failure)?
        .ok_or_else(|| conflict("the caller has no active assignment"))?;
    let transition = match status {
        FinishStatus::Completed => TaskTransition::Submit,
        FinishStatus::Failed => TaskTransition::Reopen,
    };
    let task_transition = TaskTransitionMutation {
        operation_id,
        run_id,
        actor_agent_id: Some(agent_id),
        task_id: assignment.task_id,
        transition,
        result: Some(json!({"status": status, "summary": summary})),
        summary: Some(summary),
        transitioned_at: rpc_timestamp()?,
    };
    let result = mutation_value(
        store
            .transition_task(&task_transition)
            .map_err(rpc_state_failure)?,
    );
    require_transition(result, assignment.task_id, "finish")?;
    Ok(RpcResponse::AssignmentFinished {
        operation_id,
        assignment_id: assignment.id,
        task: task_by_id(store, run_id, assignment.task_id)?,
    })
}

fn send_message(
    store: &mut Store,
    run_id: RunId,
    caller: &AuthenticatedCaller,
    operation_id: OperationId,
    recipient: String,
    message: String,
) -> Result<RpcResponse, RpcFailure> {
    if message.trim().is_empty() {
        return Err(invalid_argument("message cannot be empty"));
    }
    let recipient = resolve_agent(store, run_id, &recipient)?;
    if let Some(sender_id) = caller.agent_id() {
        if sender_id == recipient.id {
            return Err(invalid_argument(
                "an agent cannot send a message to itself",
            ));
        }
        let sender = agent_record(store, run_id, sender_id)?;
        let archetype = builtin_standard();
        let action = if recipient.role == archetype.lead {
            "lead"
        } else if recipient.role == sender.role {
            "peer"
        } else {
            recipient.role.as_str()
        };
        require_capability(store, run_id, caller, "send", action)?;
    }
    let now = rpc_timestamp()?;
    let message_id = MessageId::generate();
    let mutation = Mutation {
        id: operation_id,
        run_id,
        kind: "message.send".to_owned(),
        actor_agent_id: caller.agent_id(),
        request: json!({"recipient_agent_id": recipient.id, "message": message}),
        created_at: now,
    };
    let outcome = store
        .mutate(&mutation, |repositories| {
            let sequence =
                repositories.next_message_sequence(run_id, recipient.id)?;
            repositories.insert_message(&MessageRecord {
                id: message_id,
                run_id,
                sender_agent_id: caller.agent_id(),
                recipient_agent_id: recipient.id,
                sequence,
                body: message.clone(),
                created_at: now,
                acknowledged_at: None,
            })?;
            repositories.append_event(&NewEvent {
                run_id,
                kind: EventKind::MessageSent,
                actor: event_actor(caller),
                subject: message_id.to_string(),
                project_id: None,
                agent_id: Some(recipient.id),
                task_id: None,
                operation_id: Some(operation_id),
                correlation_id: None,
                causation_id: None,
                data: json!({
                    "recipient_agent_id": recipient.id,
                    "sender_agent_id": caller.agent_id(),
                    "sequence": sequence,
                }),
                summary: format!(
                    "Persisted message {message_id} for {}.",
                    recipient.name
                ),
                created_at: now,
            })?;
            Ok(MessageIntent {
                message_id,
                sequence,
            })
        })
        .map_err(rpc_state_failure)?;
    let intent = mutation_value(outcome);
    Ok(RpcResponse::MessageSent {
        operation_id,
        message_id: intent.message_id,
        recipient,
        sequence: u64::try_from(intent.sequence).map_err(|_| {
            RpcFailure::new(
                RpcFailureCode::Internal,
                "message sequence is not positive",
            )
        })?,
    })
}

fn inbox(
    store: &mut Store,
    run_id: RunId,
    caller: &AuthenticatedCaller,
    after: u64,
) -> Result<RpcResponse, RpcFailure> {
    let agent_id = caller.agent_id().ok_or_else(|| {
        RpcFailure::new(
            RpcFailureCode::PermissionDenied,
            "the operator does not have an agent inbox",
        )
    })?;
    let after = i64::try_from(after)
        .map_err(|_| invalid_argument("inbox cursor is too large"))?;
    let messages = store
        .transaction(|repositories| {
            repositories.messages_after(run_id, agent_id, after)
        })
        .map_err(rpc_state_failure)?;
    let next_cursor = messages.last().map_or(after, |message| message.sequence);
    let mut summaries = Vec::with_capacity(messages.len());
    for message in messages {
        let sender = message
            .sender_agent_id
            .map(|id| summary_for_agent(store, run_id, id))
            .transpose()?;
        summaries.push(MessageSummary {
            id: message.id,
            sequence: u64::try_from(message.sequence).map_err(|_| {
                RpcFailure::new(
                    RpcFailureCode::Internal,
                    "message sequence is not positive",
                )
            })?,
            sender,
            body: message.body,
            created_at: message.created_at,
            acknowledged: message.acknowledged_at.is_some(),
        });
    }
    Ok(RpcResponse::Inbox {
        messages: summaries,
        next_cursor: u64::try_from(next_cursor).map_err(|_| {
            RpcFailure::new(RpcFailureCode::Internal, "invalid inbox cursor")
        })?,
    })
}

fn acknowledge_inbox(
    store: &mut Store,
    run_id: RunId,
    caller: &AuthenticatedCaller,
    operation_id: OperationId,
    through: u64,
) -> Result<RpcResponse, RpcFailure> {
    let agent_id = caller.agent_id().ok_or_else(|| {
        RpcFailure::new(
            RpcFailureCode::PermissionDenied,
            "the operator does not have an agent inbox",
        )
    })?;
    let through = i64::try_from(through)
        .map_err(|_| invalid_argument("inbox cursor is too large"))?;
    let outcome = store
        .acknowledge_messages(&AcknowledgeMessagesMutation {
            operation_id,
            run_id,
            agent_id,
            through,
            acknowledged_at: rpc_timestamp()?,
        })
        .map_err(rpc_state_failure)?;
    match mutation_value(outcome) {
        AcknowledgeMessagesResult::Acknowledged {
            acknowledged_through,
            acknowledged_count,
        } => Ok(RpcResponse::InboxAcknowledged {
            operation_id,
            acknowledged_through: u64::try_from(acknowledged_through).map_err(
                |_| {
                    RpcFailure::new(
                        RpcFailureCode::Internal,
                        "invalid acknowledged inbox cursor",
                    )
                },
            )?,
            acknowledged_count: u64::try_from(acknowledged_count).map_err(
                |_| {
                    RpcFailure::new(
                        RpcFailureCode::Internal,
                        "invalid acknowledged message count",
                    )
                },
            )?,
        }),
        AcknowledgeMessagesResult::CursorNotFound { highest_cursor } => {
            Err(invalid_argument(format!(
                "inbox cursor {through} does not exist; highest cursor is {highest_cursor}"
            )))
        }
    }
}

fn logs(
    store: &mut Store,
    run_state_directory: &Path,
    run_id: RunId,
    caller: &AuthenticatedCaller,
    agent_name: &str,
) -> Result<RpcResponse, RpcFailure> {
    let agent = resolve_agent(store, run_id, agent_name)?;
    if caller.agent_id() != Some(agent.id) {
        require_capability(store, run_id, caller, "logs", &agent.role)?;
    }
    let session = store
        .transaction(|repositories| {
            repositories.latest_session_for_agent(run_id, agent.id)
        })
        .map_err(rpc_state_failure)?
        .ok_or_else(|| {
            not_found(format!("agent `{}` has no session", agent.name))
        })?;
    if !safe_relative_path(&session.transcript_path) {
        return Err(RpcFailure::new(
            RpcFailureCode::Internal,
            "stored transcript path is not a safe run-relative path",
        ));
    }
    let path = run_state_directory.join(&session.transcript_path);
    let transcript = match fs::read(&path) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
        Err(error) => {
            return Err(RpcFailure::new(
                RpcFailureCode::Internal,
                format!("could not read provider transcript: {error}"),
            ));
        }
    };
    Ok(RpcResponse::Logs {
        agent,
        session_id: session.id,
        transcript,
    })
}

fn events(
    store: &mut Store,
    run_id: RunId,
    caller: &AuthenticatedCaller,
    after: u64,
    limit: u16,
) -> Result<RpcResponse, RpcFailure> {
    require_operator(
        caller,
        "only the operator can inspect the full event stream",
    )?;
    let after = i64::try_from(after)
        .map_err(|_| invalid_argument("event cursor is too large"))?;
    let events = store
        .transaction(|repositories| {
            repositories.events_after(run_id, after, limit)
        })
        .map_err(rpc_state_failure)?;
    let next_cursor = events.last().map_or(after, |event| event.sequence);
    let events = events
        .into_iter()
        .map(event_summary)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(RpcResponse::Events {
        events,
        next_cursor: u64::try_from(next_cursor).map_err(|_| {
            RpcFailure::new(RpcFailureCode::Internal, "invalid event cursor")
        })?,
    })
}

fn caller_summary(
    store: &mut Store,
    run_id: RunId,
    caller: &AuthenticatedCaller,
) -> Result<CallerSummary, RpcFailure> {
    match caller {
        AuthenticatedCaller::Operator => Ok(CallerSummary {
            run_id,
            channel: CallerChannel::Operator,
            agent: None,
        }),
        AuthenticatedCaller::Agent(scope) => Ok(CallerSummary {
            run_id,
            channel: CallerChannel::Agent,
            agent: Some(summary_for_agent(store, run_id, scope.agent_id)?),
        }),
    }
}

fn require_capability(
    store: &mut Store,
    run_id: RunId,
    caller: &AuthenticatedCaller,
    namespace: &str,
    action: &str,
) -> Result<(), RpcFailure> {
    let Some(agent_id) = caller.agent_id() else {
        return Ok(());
    };
    let agent = agent_record(store, run_id, agent_id)?;
    if builtin_standard()
        .authorize(&agent.role, Capability::new(namespace, action))
        == AuthorizationDecision::Allowed
    {
        Ok(())
    } else {
        Err(RpcFailure::new(
            RpcFailureCode::PermissionDenied,
            format!(
                "role `{}` is not authorized for `{namespace}:{action}`",
                agent.role
            ),
        ))
    }
}

fn require_operator(
    caller: &AuthenticatedCaller,
    message: &'static str,
) -> Result<(), RpcFailure> {
    if caller.is_operator() {
        Ok(())
    } else {
        Err(RpcFailure::new(RpcFailureCode::PermissionDenied, message))
    }
}

fn agent_record(
    store: &mut Store,
    run_id: RunId,
    agent_id: AgentId,
) -> Result<AgentRecord, RpcFailure> {
    store
        .transaction(|repositories| repositories.agent(agent_id))
        .map_err(rpc_state_failure)?
        .filter(|agent| agent.run_id == run_id)
        .ok_or_else(|| not_found(format!("agent `{agent_id}` does not exist")))
}

fn summary_for_agent(
    store: &mut Store,
    run_id: RunId,
    agent_id: AgentId,
) -> Result<AgentSummary, RpcFailure> {
    let agents = store
        .transaction(|repositories| repositories.agents(run_id))
        .map_err(rpc_state_failure)?;
    summarize_agents(&agents)
        .into_iter()
        .find(|agent| agent.id == agent_id)
        .ok_or_else(|| not_found(format!("agent `{agent_id}` does not exist")))
}

fn resolve_agent(
    store: &mut Store,
    run_id: RunId,
    name_or_id: &str,
) -> Result<AgentSummary, RpcFailure> {
    let agents = store
        .transaction(|repositories| repositories.agents(run_id))
        .map_err(rpc_state_failure)?;
    let summaries = summarize_agents(&agents);
    if let Ok(agent_id) = name_or_id.parse::<AgentId>() {
        summaries
            .into_iter()
            .find(|agent| agent.id == agent_id)
            .ok_or_else(|| {
                not_found(format!("agent `{name_or_id}` does not exist"))
            })
    } else {
        summaries
            .into_iter()
            .find(|agent| agent.name == name_or_id)
            .ok_or_else(|| {
                not_found(format!("agent `{name_or_id}` does not exist"))
            })
    }
}

fn summarize_agents(agents: &[AgentRecord]) -> Vec<AgentSummary> {
    let lead_role = builtin_standard().lead;
    let mut role_counts = BTreeMap::<&str, usize>::new();
    agents
        .iter()
        .map(|agent| {
            let ordinal = role_counts.entry(&agent.role).or_default();
            *ordinal += 1;
            let name = if agent.role == lead_role && *ordinal == 1 {
                agent.role.clone()
            } else {
                format!("{}-{ordinal}", agent.role)
            };
            AgentSummary {
                id: agent.id,
                name,
                role: agent.role.clone(),
                state: agent.state.to_string(),
            }
        })
        .collect()
}

fn project_summary(project: ProjectRecord) -> ProjectSummary {
    ProjectSummary {
        id: project.id,
        alias: project.alias,
        root: project.canonical_path.to_string_lossy().into_owned(),
        access: "read_write".to_owned(),
    }
}

fn project_aliases(projects: &[ProjectRecord]) -> BTreeMap<ProjectId, String> {
    projects
        .iter()
        .map(|project| (project.id, project.alias.clone()))
        .collect()
}

fn task_by_id(
    store: &mut Store,
    run_id: RunId,
    task_id: TaskId,
) -> Result<TaskSummary, RpcFailure> {
    let (projects, task) = store
        .transaction(|repositories| {
            Ok((repositories.projects(run_id)?, repositories.task(task_id)?))
        })
        .map_err(rpc_state_failure)?;
    let task = task
        .filter(|task| task.run_id == run_id)
        .ok_or_else(|| not_found(format!("task `{task_id}` does not exist")))?;
    summarize_task(store, &project_aliases(&projects), task)
}

fn summarize_tasks(
    store: &mut Store,
    projects: &BTreeMap<ProjectId, String>,
    tasks: Vec<TaskRecord>,
) -> Result<Vec<TaskSummary>, RpcFailure> {
    tasks
        .into_iter()
        .map(|task| summarize_task(store, projects, task))
        .collect()
}

fn summarize_task(
    store: &mut Store,
    projects: &BTreeMap<ProjectId, String>,
    task: TaskRecord,
) -> Result<TaskSummary, RpcFailure> {
    let readiness = store
        .transaction(|repositories| repositories.task_readiness(task.id))
        .map_err(rpc_state_failure)?;
    let ready = readiness
        .as_ref()
        .is_some_and(crate::tasks::TaskReadiness::is_ready);
    let unresolved_dependencies = readiness
        .map(|readiness| readiness.unresolved_dependencies)
        .unwrap_or_default();
    let project = projects.get(&task.project_id).cloned().ok_or_else(|| {
        RpcFailure::new(
            RpcFailureCode::Internal,
            format!("task `{}` has no attached target project", task.id),
        )
    })?;
    Ok(TaskSummary {
        id: task.id,
        project_id: task.project_id,
        project,
        title: task.title,
        description: task.description,
        status: task.status,
        ready,
        unresolved_dependencies,
        result: task.result,
    })
}

fn task_counts(tasks: &[TaskRecord]) -> TaskCounts {
    let mut counts = TaskCounts::default();
    for task in tasks {
        match task.status {
            TaskStatus::Open => counts.open += 1,
            TaskStatus::InProgress => counts.in_progress += 1,
            TaskStatus::Submitted => counts.submitted += 1,
            TaskStatus::Closed => counts.closed += 1,
            TaskStatus::Canceled => counts.canceled += 1,
        }
    }
    counts
}

fn available_commands(
    store: &mut Store,
    run_id: RunId,
    caller: &AuthenticatedCaller,
) -> Result<Vec<String>, RpcFailure> {
    let mut commands = vec!["whoami", "prime"];
    if caller.is_operator() {
        commands.extend([
            "status",
            "task create",
            "task ready",
            "task close",
            "spawn",
            "send",
            "logs",
            "events",
            "stop",
        ]);
    } else {
        commands.extend(["task ready", "send", "inbox", "finish"]);
        let agent = agent_record(
            store,
            run_id,
            caller.agent_id().expect("agent caller has an ID"),
        )?;
        for (namespace, action, command) in [
            ("task", "create", "task create"),
            ("task", "close", "task close"),
            ("logs", "*", "logs"),
        ] {
            if builtin_standard()
                .authorize(&agent.role, Capability::new(namespace, action))
                == AuthorizationDecision::Allowed
            {
                commands.push(command);
            }
        }
    }
    Ok(commands.into_iter().map(str::to_owned).collect())
}

fn event_summary(event: EventRecord) -> Result<EventSummary, RpcFailure> {
    Ok(EventSummary {
        id: event.id,
        run_id: event.run_id,
        sequence: u64::try_from(event.sequence).map_err(|_| {
            RpcFailure::new(
                RpcFailureCode::Internal,
                "event sequence is not positive",
            )
        })?,
        event_type: event.event_type,
        actor: event.actor,
        subject: event.subject,
        project_id: event.project_id,
        agent_id: event.agent_id,
        task_id: event.task_id,
        operation_id: event.operation_id,
        correlation_id: event.correlation_id,
        causation_id: event.causation_id,
        payload: event.payload,
        summary: event.summary,
        created_at: event.created_at,
    })
}

fn event_actor(caller: &AuthenticatedCaller) -> String {
    caller
        .agent_id()
        .map_or_else(|| "operator".to_owned(), |id| id.to_string())
}

fn safe_relative_path(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

fn require_transition(
    result: TaskTransitionResult,
    task_id: TaskId,
    action: &str,
) -> Result<(), RpcFailure> {
    match result {
        TaskTransitionResult::Transitioned { .. } => Ok(()),
        TaskTransitionResult::Rejected(
            TaskTransitionRejection::TaskNotFound,
        ) => Err(not_found(format!("task `{task_id}` does not exist"))),
        TaskTransitionResult::Rejected(
            TaskTransitionRejection::InvalidStatus { status },
        ) => Err(conflict(format!(
            "task `{task_id}` cannot {action} from `{status}`"
        ))),
    }
}

fn drain_fake_events(
    sessions: &mut AgentSessionSupervisor<FakeProvider>,
    store: &mut Store,
    session_id: SessionId,
    observed_at: i64,
) -> Result<(), RpcFailure> {
    while sessions
        .advance(store, session_id, observed_at)
        .map_err(rpc_session_failure)?
        .is_some()
    {}
    Ok(())
}

fn role_launch_mode(mode: RoleMode) -> LaunchMode {
    match mode {
        RoleMode::Interactive => LaunchMode::Interactive,
        RoleMode::Job => LaunchMode::Job,
    }
}

fn workspace_policy_name(policy: WorkspacePolicy) -> &'static str {
    match policy {
        WorkspacePolicy::Project => "project",
        WorkspacePolicy::Worktree => "worktree",
        WorkspacePolicy::ReadOnly => "read_only",
    }
}

fn assignment_workspace_path(
    run_state_directory: &Path,
    policy: WorkspacePolicy,
    project: &ProjectRecord,
    assignment_id: AssignmentId,
) -> PathBuf {
    match policy {
        WorkspacePolicy::Project | WorkspacePolicy::ReadOnly => {
            project.canonical_path.clone()
        }
        WorkspacePolicy::Worktree => run_state_directory
            .join("workspaces")
            .join(project.id.to_string())
            .join(assignment_id.to_string()),
    }
}

fn bootstrap_instruction(run_id: RunId, role: &str) -> String {
    format!(
        "You are a {role} agent for Coterie run {run_id}. Run `coterie prime` now for current orchestration context. Follow the repository's AGENTS.md instructions."
    )
}

fn mutation_value<T>(outcome: MutationOutcome<T>) -> T {
    match outcome {
        MutationOutcome::Applied(value) | MutationOutcome::Replayed(value) => {
            value
        }
    }
}

fn rpc_timestamp() -> Result<i64, RpcFailure> {
    unix_timestamp().map_err(|error| {
        RpcFailure::new(RpcFailureCode::Internal, error.to_string())
    })
}

fn rpc_session_failure(error: AgentSessionError) -> RpcFailure {
    match error {
        AgentSessionError::IncompatibleProvider { .. }
        | AgentSessionError::MissingCapability { .. }
        | AgentSessionError::Provider(_) => {
            RpcFailure::new(RpcFailureCode::Unavailable, error.to_string())
        }
        AgentSessionError::State(error) => rpc_state_failure(error),
        AgentSessionError::UnresolvedIntent { .. } => {
            RpcFailure::new(RpcFailureCode::Conflict, error.to_string())
        }
        AgentSessionError::Transcript(_)
        | AgentSessionError::Token(_)
        | AgentSessionError::ScopeMismatch { .. }
        | AgentSessionError::UnknownSession { .. }
        | AgentSessionError::IntentMismatch { .. } => {
            RpcFailure::new(RpcFailureCode::Internal, error.to_string())
        }
    }
}

fn rpc_workspace_failure(error: WorkspaceError) -> RpcFailure {
    match error {
        WorkspaceError::Backend(_) => {
            RpcFailure::new(RpcFailureCode::Unavailable, error.to_string())
        }
        WorkspaceError::State(state) => rpc_state_failure(state),
        WorkspaceError::MissingIntent { .. } => {
            RpcFailure::new(RpcFailureCode::Internal, error.to_string())
        }
    }
}

fn rpc_state_failure(error: StoreError) -> RpcFailure {
    let code = match error {
        StoreError::OperationConflict { .. }
        | StoreError::OperationIncomplete { .. }
        | StoreError::RunNotActive { .. } => RpcFailureCode::Conflict,
        StoreError::Database(_)
        | StoreError::EncodeJson(_)
        | StoreError::ModifiedMigration { .. }
        | StoreError::UnsupportedSchema { .. }
        | StoreError::MissingOperationResult { .. }
        | StoreError::CorruptAgentState { .. }
        | StoreError::CorruptTaskState { .. }
        | StoreError::CorruptAssignmentState { .. }
        | StoreError::CredentialAlreadyRevoked { .. }
        | StoreError::InvalidSessionTransition { .. }
        | StoreError::InconsistentSessionLifecycle { .. } => {
            RpcFailureCode::Internal
        }
    };
    RpcFailure::new(code, error.to_string())
}

fn invalid_argument(message: impl Into<String>) -> RpcFailure {
    RpcFailure::new(RpcFailureCode::InvalidArgument, message)
}

fn not_found(message: impl Into<String>) -> RpcFailure {
    RpcFailure::new(RpcFailureCode::NotFound, message)
}

fn conflict(message: impl Into<String>) -> RpcFailure {
    RpcFailure::new(RpcFailureCode::Conflict, message)
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
        repositories.append_event(&NewEvent {
            run_id,
            kind: EventKind::RunStopped,
            actor: "operator".to_owned(),
            subject: run_id.to_string(),
            project_id: None,
            agent_id: None,
            task_id: None,
            operation_id: Some(operation_id),
            correlation_id: None,
            causation_id: None,
            data: json!({
                "previous_status": "active",
                "status": "stopped",
            }),
            summary: format!("Stopped run {run_id}."),
            created_at: stopped_at,
        })?;
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
                let run_event = repositories.append_event(&NewEvent {
                    run_id: active.run_id,
                    kind: EventKind::RunStarted,
                    actor: "supervisor".to_owned(),
                    subject: active.run_id.to_string(),
                    project_id: None,
                    agent_id: None,
                    task_id: None,
                    operation_id: None,
                    correlation_id: None,
                    causation_id: None,
                    data: json!({"status": "active"}),
                    summary: format!("Started run {}.", active.run_id),
                    created_at: now,
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
                })?;
                repositories.append_event(&NewEvent {
                    run_id: active.run_id,
                    kind: EventKind::ProjectAttached,
                    actor: "supervisor".to_owned(),
                    subject: active.project_id.to_string(),
                    project_id: Some(active.project_id),
                    agent_id: None,
                    task_id: None,
                    operation_id: None,
                    correlation_id: Some(run_event.id),
                    causation_id: Some(run_event.id),
                    data: json!({
                        "alias": "primary",
                        "is_primary": true,
                    }),
                    summary: format!(
                        "Attached primary project {}.",
                        active.project_id
                    ),
                    created_at: now,
                })?;
                Ok(())
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
    project_id: ProjectId,
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
                    project_id: response.project_id,
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

    #[must_use]
    pub(crate) fn project_id(&self) -> ProjectId {
        self.project_id
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

    pub(crate) async fn request(
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
            RpcResult::Ok(response) => Ok(*response),
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
                    RpcRequest::Shutdown { operation_id } => {
                        if caller.is_operator() {
                            let result =
                                request_shutdown(&commands, operation_id)
                                    .await?;
                            let shutting_down = matches!(
                                result,
                                RpcResult::Ok(ref response)
                                    if matches!(
                                        response.as_ref(),
                                        RpcResponse::ShuttingDown { .. }
                                    )
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
                    request => (
                        request_dispatch(&commands, caller, request).await?,
                        false,
                    ),
                },
                Err(failure) => (RpcResult::Err(failure), false),
            }
        };
        let write_result = write_frame(
            &mut stream,
            &ServerMessage::Response(VersionedResponse {
                protocol_version: PROTOCOL_VERSION,
                request_id: request.request_id,
                result,
            }),
        )
        .await;
        if shutting_down {
            shutdown
                .send(())
                .await
                .map_err(|_| SupervisorError::ShutdownChannelClosed)?;
            write_result?;
            return Ok(());
        }
        write_result?;
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

    fn agent_id(&self) -> Option<AgentId> {
        match self {
            Self::Operator => None,
            Self::Agent(scope) => Some(scope.agent_id),
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
        Ok(Ok(response)) => RpcResult::Ok(Box::new(response)),
        Ok(Err(failure)) => RpcResult::Err(failure),
        Err(_) => return Err(SupervisorError::CommandChannelClosed),
    })
}

async fn request_dispatch(
    commands: &mpsc::Sender<SupervisorCommand>,
    caller: AuthenticatedCaller,
    request: RpcRequest,
) -> Result<RpcResult, SupervisorError> {
    let (response, receiver) = oneshot::channel();
    commands
        .send(SupervisorCommand::Dispatch {
            caller,
            request,
            response,
        })
        .await
        .map_err(|_| SupervisorError::CommandChannelClosed)?;
    Ok(match receiver.await {
        Ok(Ok(response)) => RpcResult::Ok(Box::new(response)),
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
    #[error(transparent)]
    Session(#[from] AgentSessionError),
    #[error(transparent)]
    Workspace(#[from] WorkspaceError),
    #[error("could not {action} supervisor socket at {path:?}: {source}")]
    SocketIo {
        action: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error(transparent)]
    Frame(#[from] FrameError),
    #[error(transparent)]
    Render(#[from] crate::cli::RenderError),
    #[error("could not encode a command response: {0}")]
    StructuredOutput(#[from] serde_json::Error),
    #[error("supervisor rejected the request ({code:?}): {message}")]
    Rejected {
        code: RpcFailureCode,
        message: String,
    },
    #[error("operation `{operation_id}` failed: {source}")]
    Operation {
        operation_id: OperationId,
        #[source]
        source: Box<SupervisorError>,
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
    #[error(
        "the root `--operation-id` option applies only to foreground launch"
    )]
    ForegroundOperationIdWithCommand,
    #[error(
        "`--json` is unavailable for the interactive foreground launch because Codex owns the terminal streams"
    )]
    ForegroundJsonOutputUnsupported,
    #[error(
        "agent identity requires `COTERIE_AGENT_ID`, `COTERIE_SESSION_ID`, and `COTERIE_TOKEN`"
    )]
    IncompleteAgentEnvironment,
    #[error("agent environment variable `{variable}` is invalid")]
    InvalidAgentEnvironment { variable: &'static str },
    #[error(
        "agent environment belongs to run {found}, but this project is attached to {expected}"
    )]
    AgentEnvironmentRunMismatch { expected: RunId, found: RunId },
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

    fn for_operation(self, operation_id: OperationId) -> Self {
        Self::Operation {
            operation_id,
            source: Box::new(self),
        }
    }

    pub(crate) fn diagnostic(&self) -> crate::cli::Diagnostic {
        let (error, operation_id) = match self {
            Self::Operation {
                operation_id,
                source,
            } => (source.as_ref(), Some(*operation_id)),
            error => (error, None),
        };
        let code = match error {
            Self::Rejected { code, .. } => match code {
                RpcFailureCode::Unauthenticated => {
                    crate::cli::ErrorCode::Unauthenticated
                }
                RpcFailureCode::PermissionDenied => {
                    crate::cli::ErrorCode::PermissionDenied
                }
                RpcFailureCode::InvalidArgument => {
                    crate::cli::ErrorCode::InvalidArgument
                }
                RpcFailureCode::NotFound => crate::cli::ErrorCode::NotFound,
                RpcFailureCode::Conflict
                | RpcFailureCode::RunMismatch
                | RpcFailureCode::ProjectMismatch => {
                    crate::cli::ErrorCode::Conflict
                }
                RpcFailureCode::Unavailable
                | RpcFailureCode::ProtocolVersionMismatch => {
                    crate::cli::ErrorCode::Unavailable
                }
                RpcFailureCode::HandshakeRequired
                | RpcFailureCode::InvalidRequestSequence
                | RpcFailureCode::Internal => crate::cli::ErrorCode::Internal,
            },
            Self::NoActiveRun => crate::cli::ErrorCode::NotFound,
            Self::ForegroundOperationIdWithCommand => {
                crate::cli::ErrorCode::InvalidArgument
            }
            Self::ForegroundJsonOutputUnsupported => {
                crate::cli::ErrorCode::InvalidArgument
            }
            Self::IncompleteAgentEnvironment
            | Self::InvalidAgentEnvironment { .. }
            | Self::AgentEnvironmentRunMismatch { .. } => {
                crate::cli::ErrorCode::Unauthenticated
            }
            Self::State(
                StoreError::OperationConflict { .. }
                | StoreError::OperationIncomplete { .. }
                | StoreError::RunNotActive { .. },
            ) => crate::cli::ErrorCode::Conflict,
            Self::State(_) | Self::RunStateMismatch { .. } => {
                crate::cli::ErrorCode::CorruptState
            }
            Self::SocketIo { .. }
            | Self::Frame(_)
            | Self::Spawn { .. }
            | Self::StartupTimeout { .. }
            | Self::ShutdownTimeout { .. } => {
                crate::cli::ErrorCode::Unavailable
            }
            Self::Session(
                AgentSessionError::Provider(_)
                | AgentSessionError::IncompatibleProvider { .. }
                | AgentSessionError::MissingCapability { .. },
            ) => crate::cli::ErrorCode::Unavailable,
            Self::Project(_)
            | Self::Session(_)
            | Self::Workspace(_)
            | Self::Render(_)
            | Self::StructuredOutput(_)
            | Self::InvalidProof
            | Self::InvalidResponseCorrelation
            | Self::UnexpectedMessage { .. }
            | Self::RequestIdExhausted
            | Self::CurrentDirectory(_)
            | Self::CurrentExecutable(_)
            | Self::ChildStatus(_)
            | Self::StateFileIo { .. }
            | Self::UnsafeSocketPath { .. }
            | Self::SystemClock(_)
            | Self::TimestampOverflow
            | Self::MissingChildStderr
            | Self::CommandChannelClosed
            | Self::ShutdownChannelClosed
            | Self::SocketPathTooLong { .. } => crate::cli::ErrorCode::Internal,
            Self::Operation { .. } => {
                unreachable!("operation errors are unwrapped")
            }
        };
        let diagnostic = crate::cli::Diagnostic::new(code, error.to_string());
        operation_id.map_or(diagnostic.clone(), |operation_id| {
            diagnostic.for_operation(operation_id)
        })
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
        AuthenticatedCaller, SupervisorClient, SupervisorCommand,
        SupervisorError, launch_foreground, persist_shutdown, runtime_sessions,
        runtime_workspaces, serve_connection, serve_listener,
        validate_handshake, validate_socket_path,
    };
    use crate::auth::{AgentToken, SessionScope};
    use crate::id::{AgentId, OperationId, ProjectId, RunId, SessionId};
    use crate::project::{ActiveRunEntry, ProjectIdentity};
    use crate::protocol::{
        ClientMessage, ConnectionChannel, FinishStatus, HandshakeRequest,
        RequestAuthentication, RpcFailureCode, RpcRequest, RpcResponse,
        ServerMessage, VersionedRequest, read_frame, write_frame,
    };
    use crate::providers::LifecycleState;
    use crate::state::{
        AgentRecord, ProjectRecord, RunRecord, SessionCredentialRecord,
        SessionProcessOwner, SessionRecord, Store, StoreError,
    };
    use crate::tasks::TaskStatus;

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
            match command_rx
                .recv()
                .await
                .expect("the ping command should arrive")
            {
                SupervisorCommand::Dispatch {
                    request: RpcRequest::Ping,
                    response,
                    ..
                } => response
                    .send(Ok(RpcResponse::Pong {
                        run_id: server_entry.run_id,
                    }))
                    .expect("the connection should await its ping response"),
                _ => panic!("the first command should be the typed ping"),
            }
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
                SupervisorCommand::Dispatch { .. } => {
                    panic!("the shutdown request must use its dedicated path")
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
        request.protocol_version = 1;
        assert_eq!(
            validate_handshake(&request, &active)
                .expect("the previous wire version should be rejected")
                .code,
            RpcFailureCode::ProtocolVersionMismatch
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
                    state: LifecycleState::Running,
                    created_at: 11,
                })?;
                repositories.insert_session(&SessionRecord {
                    id: session_id,
                    run_id: active.run_id,
                    agent_id,
                    generation: scope.generation,
                    provider: "fake".to_owned(),
                    provider_session_id: Some("fake-session".to_owned()),
                    reconciliation_state:
                        crate::state::ExternalResourceState::Observed,
                    state: LifecycleState::Running,
                    transcript_path: PathBuf::from("transcripts/session.jsonl"),
                    created_at: 12,
                    ended_at: None,
                    reconciled_at: Some(12),
                    process_owner: SessionProcessOwner::Supervisor,
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
        let run_state_directory = fixture.join("run");
        let server_socket = socket.clone();
        let mut sessions = runtime_sessions(&run_state_directory);
        let mut workspaces = runtime_workspaces();
        let server = tokio::spawn(async move {
            serve_listener(
                listener,
                served_entry,
                &run_state_directory,
                &server_socket,
                &mut store,
                &mut sessions,
                &mut workspaces,
            )
            .await
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

    #[tokio::test]
    async fn fake_lead_and_worker_complete_a_task_across_foreground_reconnection()
     {
        let fixture = TestDirectory::new();
        let socket = fixture.join("supervisor.sock");
        let project_path = fixture.join("project");
        fs::create_dir(&project_path).expect("the project should be created");
        let active = entry(&project_path);
        let listener = UnixListener::bind(&socket)
            .expect("the fixture socket should bind");
        let run_state_directory = fixture.join("run");
        fs::create_dir(&run_state_directory)
            .expect("the run directory should be created");
        let mut store = Store::open(&run_state_directory.join("state.sqlite3"))
            .expect("the store should open");
        store
            .transaction(|repositories| {
                repositories.insert_run(&RunRecord {
                    id: active.run_id,
                    status: "active".to_owned(),
                    created_at: 10,
                    stopped_at: None,
                })?;
                repositories.insert_project(&ProjectRecord {
                    id: active.project_id,
                    run_id: active.run_id,
                    alias: "primary".to_owned(),
                    original_path: project_path.clone(),
                    canonical_path: project_path.clone(),
                    identity: active.project_identity.clone(),
                    is_primary: true,
                    attached_at: 10,
                })
            })
            .expect("the run prerequisites should be inserted");
        let (credential_tx, credential_rx) = std::sync::mpsc::channel();
        let served_entry = active.clone();
        let server_socket = socket.clone();
        let mut sessions = runtime_sessions(&run_state_directory)
            .with_credential_observer(credential_tx);
        let mut workspaces = runtime_workspaces();
        let server = tokio::spawn(async move {
            serve_listener(
                listener,
                served_entry,
                &run_state_directory,
                &server_socket,
                &mut store,
                &mut sessions,
                &mut workspaces,
            )
            .await
        });
        let mut operator =
            SupervisorClient::connect_operator_at(&socket, &active)
                .await
                .expect("the operator should connect");
        let launch_operation = OperationId::generate();
        let launch = operator
            .request(RpcRequest::LaunchForeground {
                operation_id: launch_operation,
                token: AgentToken::generate()
                    .expect("the lead token should be generated"),
            })
            .await
            .expect("the foreground lead should be prepared");
        let lead_scope = match launch {
            RpcResponse::ForegroundPrepared {
                run_id,
                agent,
                session_id,
                generation,
                ..
            } => {
                assert_eq!(run_id, active.run_id);
                assert_eq!(agent.name, "lead");
                SessionScope {
                    run_id,
                    agent_id: agent.id,
                    session_id,
                    generation,
                }
            }
            response => panic!("unexpected launch response: {response:?}"),
        };
        operator
            .request(RpcRequest::ForegroundLaunchFailed { scope: lead_scope })
            .await
            .expect("the unlaunched foreground fixture should become terminal");

        let task = operator
            .request(RpcRequest::TaskCreate {
                operation_id: OperationId::generate(),
                title: "Implement parser".to_owned(),
                description: "Add parsing tests first.".to_owned(),
                project: "primary".to_owned(),
                group: None,
                dependencies: Vec::new(),
            })
            .await
            .expect("the delegated task should be created");
        let task_id = match task {
            RpcResponse::TaskCreated { task, .. } => task.id,
            response => panic!("unexpected task response: {response:?}"),
        };
        let downstream = operator
            .request(RpcRequest::TaskCreate {
                operation_id: OperationId::generate(),
                title: "Document parser".to_owned(),
                description: "Document the accepted parser.".to_owned(),
                project: "primary".to_owned(),
                group: None,
                dependencies: vec![task_id],
            })
            .await
            .expect("the dependent task should be created");
        let downstream_id = match downstream {
            RpcResponse::TaskCreated { task, .. } => task.id,
            response => panic!("unexpected task response: {response:?}"),
        };
        let spawn = operator
            .request(RpcRequest::Spawn {
                operation_id: OperationId::generate(),
                role: "worker".to_owned(),
                task_id,
            })
            .await
            .expect("the task should be delegated to a fake worker");
        let (agent_id, session_id, assignment_id) = match spawn {
            RpcResponse::Spawned {
                agent,
                session_id,
                assignment_id,
                task_id: spawned_task_id,
                ..
            } => {
                assert_eq!(agent.name, "worker-1");
                assert_eq!(spawned_task_id, task_id);
                (agent.id, session_id, assignment_id)
            }
            response => panic!("unexpected spawn response: {response:?}"),
        };
        let launched = credential_rx
            .recv()
            .expect("the fake worker should receive a credential");
        assert_eq!(launched.scope.agent_id, agent_id);
        assert_eq!(launched.scope.session_id, session_id);

        let initial_events = operator
            .request(RpcRequest::Events {
                after: 0,
                limit: 100,
            })
            .await
            .expect("fixture setup should be observable");
        let RpcResponse::Events {
            events,
            next_cursor: initial_event_cursor,
        } = initial_events
        else {
            panic!("the event stream should be returned");
        };
        assert!(
            events
                .iter()
                .any(|event| event.event_type == "task.claimed")
        );
        let sent = operator
            .request(RpcRequest::Send {
                operation_id: OperationId::generate(),
                recipient: "worker-1".to_owned(),
                message: "Check the parser edge cases.".to_owned(),
            })
            .await
            .expect("the operator message should be durable");
        assert!(matches!(sent, RpcResponse::MessageSent { sequence: 1, .. }));

        let mut agent = SupervisorClient::connect_agent_at(
            &socket,
            &active,
            agent_id,
            session_id,
            launched.token,
        )
        .await
        .expect("the worker should connect");
        assert!(matches!(
            agent.request(RpcRequest::Whoami).await,
            Ok(RpcResponse::Identity {
                channel: crate::protocol::CallerChannel::Agent,
                agent: Some(ref agent),
                ..
            }) if agent.name == "worker-1"
        ));
        assert!(matches!(
            agent.request(RpcRequest::Prime).await,
            Ok(RpcResponse::Prime {
                identity: crate::protocol::CallerSummary {
                    channel: crate::protocol::CallerChannel::Agent,
                    ..
                },
                active_task: Some(ref task),
                ref commands,
                ..
            }) if task.id == task_id && commands.iter().any(|command| command == "finish")
        ));
        assert!(matches!(
            agent
                .request(RpcRequest::Spawn {
                    operation_id: OperationId::generate(),
                    role: "worker".to_owned(),
                    task_id,
                })
                .await,
            Err(SupervisorError::Rejected {
                code: RpcFailureCode::PermissionDenied,
                ..
            })
        ));
        assert!(matches!(
            agent
                .request(RpcRequest::Events {
                    after: 0,
                    limit: 100,
                })
                .await,
            Err(SupervisorError::Rejected {
                code: RpcFailureCode::PermissionDenied,
                ..
            })
        ));
        assert!(matches!(
            agent.request(RpcRequest::Status).await,
            Err(SupervisorError::Rejected {
                code: RpcFailureCode::PermissionDenied,
                ..
            })
        ));
        assert!(matches!(
            agent.request(RpcRequest::Inbox { after: 0 }).await,
            Ok(RpcResponse::Inbox {
                ref messages,
                next_cursor: 1,
            }) if messages.len() == 1
                && messages[0].body == "Check the parser edge cases."
        ));
        let acknowledgement_operation = OperationId::generate();
        let acknowledge_request = || RpcRequest::InboxAcknowledge {
            operation_id: acknowledgement_operation,
            through: 1,
        };
        let acknowledged = agent
            .request(acknowledge_request())
            .await
            .expect("the worker should acknowledge its durable inbox");
        assert!(matches!(
            acknowledged,
            RpcResponse::InboxAcknowledged {
                acknowledged_through: 1,
                acknowledged_count: 1,
                ..
            }
        ));
        assert_eq!(
            agent
                .request(acknowledge_request())
                .await
                .expect("the acknowledgement retry should replay"),
            acknowledged
        );
        assert!(matches!(
            agent.request(RpcRequest::Inbox { after: 0 }).await,
            Ok(RpcResponse::Inbox { ref messages, .. })
                if messages.len() == 1 && messages[0].acknowledged
        ));
        let finish_operation = OperationId::generate();
        let finish_request = || RpcRequest::Finish {
            operation_id: finish_operation,
            status: FinishStatus::Completed,
            summary: "Parser and tests implemented.".to_owned(),
        };
        let finished = agent
            .request(finish_request())
            .await
            .expect("the worker should finish its assignment");
        assert!(matches!(
            finished,
            RpcResponse::AssignmentFinished {
                assignment_id: finished_assignment_id,
                ref task,
                ..
            } if finished_assignment_id == assignment_id
                && task.status == TaskStatus::Submitted
        ));
        assert_eq!(
            agent
                .request(finish_request())
                .await
                .expect("the finish retry should replay"),
            finished
        );
        let close_operation = OperationId::generate();
        let close_request = || RpcRequest::TaskClose {
            operation_id: close_operation,
            task_id,
            summary: "Integrated and verified.".to_owned(),
        };
        let closed = operator
            .request(close_request())
            .await
            .expect("the operator should close the task");
        assert!(matches!(
            closed,
            RpcResponse::TaskClosed { ref task, .. }
                if task.status == TaskStatus::Closed
        ));
        assert_eq!(
            operator
                .request(close_request())
                .await
                .expect("the closure retry should replay"),
            closed
        );
        let events = operator
            .request(RpcRequest::Events {
                after: initial_event_cursor,
                limit: 100,
            })
            .await
            .expect("state transitions should be observable");
        let RpcResponse::Events { events, .. } = events else {
            panic!("the event stream should be returned");
        };
        let event_types = events
            .iter()
            .map(|event| event.event_type.as_str())
            .collect::<Vec<_>>();
        for event_type in [
            "message.sent",
            "message.acknowledged",
            "task.lifecycle_changed",
        ] {
            assert_eq!(
                event_types
                    .iter()
                    .filter(|candidate| **candidate == event_type)
                    .count(),
                if event_type == "task.lifecycle_changed" {
                    2
                } else {
                    1
                },
                "every applied transition should emit once, including retries"
            );
        }
        assert!(events.iter().all(|event| {
            event.run_id == active.run_id
                && event.payload["schema_version"] == 1
        }));
        drop(agent);
        drop(operator);

        let mut reconnected =
            SupervisorClient::connect_operator_at(&socket, &active)
                .await
                .expect("the foreground should reconnect to the same run");
        assert!(matches!(
            reconnected
                .request(RpcRequest::LaunchForeground {
                    operation_id: launch_operation,
                    token: AgentToken::generate()
                        .expect("the retry token should be generated"),
                })
                .await,
            Err(SupervisorError::Rejected {
                code: RpcFailureCode::Conflict,
                ..
            })
        ));
        assert!(matches!(
            reconnected.request(RpcRequest::TaskReady).await,
            Ok(RpcResponse::ReadyTasks { ref tasks })
                if tasks.len() == 1 && tasks[0].id == downstream_id
        ));
        let primed_tasks = match reconnected
            .request(RpcRequest::Prime)
            .await
            .expect("prime should reconstruct the closed task result")
        {
            RpcResponse::Prime { tasks, .. } => tasks,
            response => panic!("unexpected prime response: {response:?}"),
        };
        let closed_task = primed_tasks
            .iter()
            .find(|task| task.id == task_id)
            .expect("prime should retain the closed task");
        assert_eq!(closed_task.status, TaskStatus::Closed);
        assert_eq!(
            closed_task.result,
            Some(serde_json::json!({
                "assignment_result": {
                    "status": "completed",
                    "summary": "Parser and tests implemented.",
                },
                "validation_summary": "Integrated and verified.",
            }))
        );
        reconnected
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
        store
            .transaction(|repositories| {
                let events = repositories.events_after(run_id, 0, 100)?;
                assert_eq!(events.len(), 1);
                assert_eq!(events[0].event_type, "run.stopped");
                assert_eq!(events[0].operation_id, Some(operation_id));
                Ok(())
            })
            .expect("the shutdown event should be durable exactly once");

        let different_operation = "co-01ARZ3NDEKTSV4RRFFQ69G5FAY"
            .parse::<OperationId>()
            .expect("valid operation ID");
        assert!(matches!(
            persist_shutdown(&mut store, run_id, different_operation),
            Err(SupervisorError::State(StoreError::RunNotActive { id }))
                if id == run_id
        ));
    }

    #[test]
    fn a_prepared_foreground_launch_is_claimed_before_its_response() {
        let mut store = Store::open_in_memory().expect("the store should open");
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
        let operation_id = OperationId::generate();
        let first_token =
            AgentToken::generate().expect("the token should be generated");

        let first = launch_foreground(
            &mut store,
            run_id,
            &AuthenticatedCaller::Operator,
            operation_id,
            &first_token,
        )
        .expect("the first caller should claim the launch");
        let scope = match first {
            RpcResponse::ForegroundPrepared {
                agent,
                session_id,
                generation,
                ..
            } => SessionScope {
                run_id,
                agent_id: agent.id,
                session_id,
                generation,
            },
            response => panic!("unexpected launch response: {response:?}"),
        };

        let retry_token = AgentToken::generate()
            .expect("the retry token should be generated");
        let retry = launch_foreground(
            &mut store,
            run_id,
            &AuthenticatedCaller::Operator,
            operation_id,
            &retry_token,
        )
        .expect_err("the claimed launch must not be replayed");
        assert_eq!(retry.code, RpcFailureCode::Conflict);
        store
            .transaction(|repositories| {
                let credential = repositories
                    .active_session_credential(
                        run_id,
                        scope.agent_id,
                        scope.session_id,
                    )?
                    .expect(
                        "the claimed launch credential should remain active",
                    );
                assert!(credential.token_verifier.verify(&first_token, scope));
                assert!(!credential.token_verifier.verify(&retry_token, scope));
                Ok(())
            })
            .expect("the launch credential should be inspectable");
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

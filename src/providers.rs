//! Out-of-process agent harness adapters.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::env;
use std::ffi::OsStr;
use std::ffi::OsString;
use std::fmt;
use std::future::{Future, pending};
use std::io::{self, BufRead, BufReader, IsTerminal, Read};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::path::PathBuf;
use std::process::{Child, ChildStdout, Command, Stdio};
use std::str::FromStr;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::thread;
use std::time::Duration;

use nix::errno::Errno;
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use semver::Version;
use signal_hook::consts::signal::{
    SIGHUP, SIGINT, SIGKILL, SIGQUIT, SIGTERM, SIGWINCH,
};
use signal_hook::iterator::SignalsInfo;
use signal_hook::iterator::exfiltrator::WithOrigin;
use signal_hook::iterator::exfiltrator::origin::Origin;
use signal_hook::low_level::siginfo::Cause;
use thiserror::Error;
use tokio::sync::mpsc;
use tokio::time::{Instant, sleep_until};

use crate::auth::{AgentToken, SessionScope};
use crate::config::{
    ApprovalPolicy, FilesystemPolicy, NetworkPolicy, PermissionProfile,
};
use crate::id::{ProjectId, TaskId};

const MAXIMUM_CODEX_JSONL_FRAME_BYTES: u64 = 1024 * 1024;
const MAXIMUM_PENDING_CODEX_FRAMES: usize = 64;
const CODEX_RUNTIME_ENVIRONMENT_VARIABLES: [&str; 9] = [
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "CODEX_HOME",
    "OPENAI_API_KEY",
    "__ETC_PROFILE_DONE",
    "__NIXOS_SET_ENVIRONMENT_DONE",
];

#[cfg(test)]
#[path = "providers/shell_tests.rs"]
mod shell_tests;

/// A provider feature that Coterie must verify before depending on it.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum ProviderCapability {
    StartupInstructions,
    ForegroundInteractive,
    BackgroundJobs,
    StructuredLifecycleEvents,
    WorkingDirectory,
    FilesystemSandbox,
    NetworkSandbox,
    ApprovalPolicy,
    Interrupt,
    Termination,
    TranscriptStreaming,
}

impl fmt::Display for ProviderCapability {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::StartupInstructions => "startup instruction injection",
            Self::ForegroundInteractive => "foreground interactive sessions",
            Self::BackgroundJobs => "background job execution",
            Self::StructuredLifecycleEvents => "structured lifecycle events",
            Self::WorkingDirectory => "working directory enforcement",
            Self::FilesystemSandbox => "filesystem sandbox enforcement",
            Self::NetworkSandbox => "network sandbox enforcement",
            Self::ApprovalPolicy => "approval policy enforcement",
            Self::Interrupt => "interrupt",
            Self::Termination => "termination",
            Self::TranscriptStreaming => "transcript streaming",
        })
    }
}

/// The installed provider identity and its observed behavior contract.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ProviderProbe {
    pub(crate) name: String,
    pub(crate) version: Version,
    pub(crate) capabilities: BTreeSet<ProviderCapability>,
    pub(crate) compatibility: ProviderCompatibility,
}

/// Whether an installed provider version belongs to a validated release range.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ProviderCompatibility {
    Compatible,
    Incompatible { reason: String, remedy: String },
}

/// Whether Coterie owns an interactive foreground or background job session.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LaunchMode {
    Interactive,
    Job,
}

/// Provider-independent input for starting one fenced session generation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LaunchSpecification {
    pub(crate) scope: SessionScope,
    pub(crate) working_directory: PathBuf,
    pub(crate) permission_profile: PermissionProfile,
    pub(crate) bootstrap_instruction: String,
}

/// Session identity exposed to one foreground provider process.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct InteractiveEnvironment {
    pub(crate) project_id: ProjectId,
    pub(crate) primary_project_root: PathBuf,
    pub(crate) role: String,
    pub(crate) socket_path: PathBuf,
    pub(crate) token: AgentToken,
}

/// The complete environment granted to one non-interactive provider process.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct JobEnvironment {
    pub(crate) project_id: ProjectId,
    pub(crate) primary_project_root: PathBuf,
    pub(crate) role: String,
    pub(crate) task_id: TaskId,
    pub(crate) socket_path: PathBuf,
    pub(crate) token: AgentToken,
}

/// One foreground Codex process whose terminal is owned by the caller.
pub(crate) struct CodexInteractiveProcess {
    child: tokio::process::Child,
    inherited_terminal: bool,
    signals: SignalMonitor,
    supervision: crate::config::SupervisionPolicy,
}

struct CodexJobProcess {
    child: Child,
    scope: SessionScope,
    frames: Receiver<JobStreamItem>,
    pending: VecDeque<ProviderEventKind>,
    observation: SessionObservation,
    next_sequence: u64,
    stdout_closed: bool,
    exit_observed: bool,
}

enum JobStreamItem {
    Frame(Vec<u8>),
    ReadFailure(String),
}

impl CodexInteractiveProcess {
    #[must_use]
    pub(crate) fn process_id(&self) -> u32 {
        self.child
            .id()
            .expect("a running Tokio child retains its process ID")
    }

    pub(crate) fn terminate(&self) -> Result<(), ProviderError> {
        forward_signal(self.process_id(), SIGTERM)
    }

    pub(crate) async fn wait_until_termination<F>(
        mut self,
        termination: F,
    ) -> Result<(std::process::ExitStatus, bool), ProviderError>
    where
        F: Future<Output = ()>,
    {
        crate::fault::point("process.foreground.wait.before");
        let mut termination = Box::pin(termination);
        let mut termination_requested = false;
        let mut shutdown_started = false;
        let mut terminate_deadline = None;
        let mut kill_deadline = None;
        loop {
            tokio::select! {
                status = self.child.wait() => {
                    crate::fault::point("process.foreground.wait.after");
                    return status
                        .map(|status| (status, termination_requested))
                        .map_err(ProviderError::InteractiveWait);
                }
                signal = self.signals.receiver.recv() => {
                    let signal = signal.ok_or(
                        ProviderError::SignalMonitorClosed,
                    )?;
                    // Terminal-generated signals already reach Codex because it
                    // shares Coterie's foreground process group.
                    if !self.inherited_terminal
                        || !matches!(signal.cause, Cause::Kernel)
                    {
                        forward_signal(self.process_id(), signal.signal)?;
                    }
                    // A closed editor terminal cannot host a surviving TUI.
                    // Bound cleanup even when the provider ignores the signal.
                    if matches!(signal.signal, SIGHUP | SIGQUIT | SIGTERM)
                        && !shutdown_started
                    {
                        (terminate_deadline, kill_deadline) = self.shutdown_deadlines();
                        if signal.signal == SIGTERM {
                            terminate_deadline = None;
                        }
                        shutdown_started = true;
                    }
                }
                () = &mut termination, if !termination_requested => {
                    if !shutdown_started {
                        forward_signal(self.process_id(), SIGINT)?;
                        (terminate_deadline, kill_deadline) = self.shutdown_deadlines();
                        shutdown_started = true;
                    }
                    termination_requested = true;
                }
                () = wait_for_deadline(terminate_deadline), if terminate_deadline.is_some() => {
                    self.terminate()?;
                    terminate_deadline = None;
                }
                () = wait_for_deadline(kill_deadline),
                    if kill_deadline.is_some() =>
                {
                    forward_signal(self.process_id(), SIGKILL)?;
                    kill_deadline = None;
                }
            }
        }
    }

    fn shutdown_deadlines(&self) -> (Option<Instant>, Option<Instant>) {
        let now = Instant::now();
        (
            Some(
                now + Duration::from_millis(
                    self.supervision.interrupt_grace_ms as u64,
                ),
            ),
            Some(
                now + Duration::from_millis(
                    (self.supervision.shutdown_timeout_ms / 2) as u64,
                ),
            ),
        )
    }
}

async fn wait_for_deadline(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => sleep_until(deadline).await,
        None => pending().await,
    }
}

struct SignalMonitor {
    receiver: mpsc::UnboundedReceiver<Origin>,
    handle: signal_hook::iterator::Handle,
}

impl SignalMonitor {
    fn install() -> Result<Self, ProviderError> {
        let mut signals = SignalsInfo::<WithOrigin>::new([
            SIGHUP, SIGINT, SIGQUIT, SIGTERM, SIGWINCH,
        ])
        .map_err(ProviderError::SignalRegistration)?;
        let handle = signals.handle();
        let (sender, receiver) = mpsc::unbounded_channel();
        thread::Builder::new()
            .name("coterie-signal-forwarder".to_owned())
            .spawn(move || {
                for signal in signals.forever() {
                    if sender.send(signal).is_err() {
                        break;
                    }
                }
            })
            .map_err(ProviderError::SignalThread)?;
        Ok(Self { receiver, handle })
    }
}

impl Drop for SignalMonitor {
    fn drop(&mut self) {
        self.handle.close();
    }
}

fn forward_signal(process_id: u32, signal: i32) -> Result<(), ProviderError> {
    let signal = Signal::try_from(signal)
        .map_err(|_| ProviderError::UnsupportedSignal { signal })?;
    let process_id = i32::try_from(process_id)
        .map(Pid::from_raw)
        .map_err(|_| ProviderError::InvalidProcessId { process_id })?;
    crate::fault::point("process.signal.before");
    match kill(process_id, signal) {
        Ok(()) | Err(Errno::ESRCH) => {
            crate::fault::point("process.signal.after");
            Ok(())
        }
        Err(source) => Err(ProviderError::SignalForward { signal, source }),
    }
}

fn process_is_absent(provider_session_id: &str) -> bool {
    let Some(process_id) = provider_session_id
        .strip_prefix("process:")
        .and_then(|process_id| process_id.parse::<u32>().ok())
        .filter(|process_id| *process_id > 0)
        .and_then(|process_id| i32::try_from(process_id).ok())
    else {
        return false;
    };
    matches!(kill(Pid::from_raw(process_id), None), Err(Errno::ESRCH))
}

/// An opaque provider execution identity bound to a Coterie session scope.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ProviderSessionHandle {
    provider_id: String,
    pub(crate) scope: SessionScope,
}

impl ProviderSessionHandle {
    #[must_use]
    pub(crate) fn new(
        provider_id: impl Into<String>,
        scope: SessionScope,
    ) -> Self {
        Self {
            provider_id: provider_id.into(),
            scope,
        }
    }

    /// Returns the provider-native identity used only by its adapter.
    #[must_use]
    pub(crate) fn provider_id(&self) -> &str {
        &self.provider_id
    }
}

/// The process-level state of one provider session.
#[derive(
    Clone,
    Copy,
    Debug,
    Eq,
    PartialEq,
    serde::Serialize,
    serde::Deserialize,
    schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LifecycleState {
    Starting,
    Running,
    Exited,
    Lost,
    Unknown,
    Quarantined,
}

impl LifecycleState {
    #[must_use]
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Exited => "exited",
            Self::Lost => "lost",
            Self::Unknown => "unknown",
            Self::Quarantined => "quarantined",
        }
    }

    #[must_use]
    pub(crate) const fn is_terminal(self) -> bool {
        matches!(self, Self::Exited | Self::Lost | Self::Quarantined)
    }

    #[must_use]
    pub(crate) fn allows(self, next: Self) -> bool {
        self == next
            || matches!(self, Self::Starting | Self::Running | Self::Unknown)
                && matches!(
                    next,
                    Self::Running
                        | Self::Exited
                        | Self::Lost
                        | Self::Unknown
                        | Self::Quarantined
                )
    }
}

impl fmt::Display for LifecycleState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for LifecycleState {
    type Err = InvalidLifecycleState;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "starting" => Ok(Self::Starting),
            "running" => Ok(Self::Running),
            "exited" => Ok(Self::Exited),
            "lost" => Ok(Self::Lost),
            "unknown" => Ok(Self::Unknown),
            "quarantined" => Ok(Self::Quarantined),
            _ => Err(InvalidLifecycleState(value.to_owned())),
        }
    }
}

/// A lifecycle value read from a provider or durable state is not recognized.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error("unknown provider lifecycle state `{0}`")]
pub(crate) struct InvalidLifecycleState(String);

/// Provider-reported semantic activity, independent of process liveness.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ActivityState {
    Busy,
    Idle,
    Unknown,
}

/// Why a provider session reached the known `exited` lifecycle state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ExitReason {
    Process,
    Interrupted,
    Terminated,
}

impl ExitReason {
    #[must_use]
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Process => "process",
            Self::Interrupted => "interrupted",
            Self::Terminated => "terminated",
        }
    }
}

/// A provider exit observation without interpreting it as task success.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SessionExit {
    pub(crate) code: Option<i32>,
    pub(crate) reason: ExitReason,
}

/// The latest provider observation for a session.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SessionObservation {
    pub(crate) lifecycle: LifecycleState,
    pub(crate) activity: ActivityState,
    pub(crate) exit: Option<SessionExit>,
}

impl SessionObservation {
    #[must_use]
    pub(crate) const fn starting() -> Self {
        Self {
            lifecycle: LifecycleState::Starting,
            activity: ActivityState::Unknown,
            exit: None,
        }
    }

    #[must_use]
    pub(crate) const fn exited(code: i32) -> Self {
        Self::process_exit(Some(code))
    }

    #[must_use]
    pub(crate) const fn process_exit(code: Option<i32>) -> Self {
        Self {
            lifecycle: LifecycleState::Exited,
            activity: ActivityState::Unknown,
            exit: Some(SessionExit {
                code,
                reason: ExitReason::Process,
            }),
        }
    }

    #[must_use]
    const fn interrupted() -> Self {
        Self {
            lifecycle: LifecycleState::Exited,
            activity: ActivityState::Unknown,
            exit: Some(SessionExit {
                code: None,
                reason: ExitReason::Interrupted,
            }),
        }
    }

    #[must_use]
    const fn terminated() -> Self {
        Self {
            lifecycle: LifecycleState::Exited,
            activity: ActivityState::Unknown,
            exit: Some(SessionExit {
                code: None,
                reason: ExitReason::Terminated,
            }),
        }
    }
}

/// One ordered provider-native event.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ProviderEvent {
    pub(crate) sequence: u64,
    pub(crate) kind: ProviderEventKind,
}

impl ProviderEvent {
    #[must_use]
    pub(crate) fn observation(&self) -> Option<SessionObservation> {
        match self.kind {
            ProviderEventKind::Observation(observation) => Some(observation),
            ProviderEventKind::Output(_) => None,
            ProviderEventKind::MalformedOutput { observation, .. } => {
                Some(observation)
            }
        }
    }
}

/// Provider data retained before later normalization into Coterie events.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ProviderEventKind {
    Observation(SessionObservation),
    Output(Vec<u8>),
    MalformedOutput {
        bytes: Vec<u8>,
        diagnostic: String,
        observation: SessionObservation,
    },
}

/// What an adapter can prove about a durable provider execution identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ProviderRecovery {
    Observed {
        handle: ProviderSessionHandle,
        observation: SessionObservation,
    },
    Lost,
    Unknown,
}

/// The process boundary used by the session supervisor.
pub(crate) trait Provider {
    /// Selects the trusted binding for the next preflight and launch. Existing handles keep their processes.
    fn configure(&mut self, binding: &crate::config::ProviderBinding);

    fn probe(&self) -> Result<ProviderProbe, ProviderError>;

    fn launch_interactive(
        &mut self,
        specification: &LaunchSpecification,
        environment: Option<&InteractiveEnvironment>,
    ) -> Result<ProviderSessionHandle, ProviderError>;

    fn launch_job(
        &mut self,
        specification: &LaunchSpecification,
        environment: &JobEnvironment,
    ) -> Result<ProviderSessionHandle, ProviderError>;

    fn recover(
        &self,
        provider_session_id: &str,
        scope: SessionScope,
    ) -> Result<ProviderRecovery, ProviderError>;

    fn observe(
        &self,
        session: &ProviderSessionHandle,
    ) -> Result<SessionObservation, ProviderError>;

    fn next_event(
        &mut self,
        session: &ProviderSessionHandle,
    ) -> Result<Option<ProviderEvent>, ProviderError>;

    fn interrupt(
        &mut self,
        session: &ProviderSessionHandle,
    ) -> Result<SessionObservation, ProviderError>;

    fn terminate(
        &mut self,
        session: &ProviderSessionHandle,
    ) -> Result<SessionObservation, ProviderError>;

    fn kill(
        &mut self,
        session: &ProviderSessionHandle,
    ) -> Result<SessionObservation, ProviderError>;
}

/// A provider adapter could not perform a requested session operation.
#[derive(Debug, Error)]
pub(crate) enum ProviderError {
    #[error("the fake provider has no launch script remaining")]
    NoLaunchScript,
    #[error("provider session `{provider_id}` does not exist")]
    UnknownSession { provider_id: String },
    #[error(
        "the Codex provider command is empty; configure a provider executable"
    )]
    EmptyCodexCommand,
    #[error(
        "could not execute a Codex probe with `{executable}`: {source}; install Codex CLI 0.151.0 or later and ensure `{executable}` is on PATH"
    )]
    ProbeExecution {
        executable: String,
        #[source]
        source: io::Error,
    },
    #[error(
        "the Codex {probe} probe exited with {status}: {diagnostic}; run `codex update` or install Codex CLI 0.151.0 or later"
    )]
    ProbeFailed {
        probe: &'static str,
        status: String,
        diagnostic: String,
    },
    #[error(
        "could not parse {output:?}; expected `codex-cli <semantic-version>` from `codex --version`; run `codex update` or install Codex CLI 0.151.0 or later"
    )]
    InvalidVersionOutput { output: String },
    #[error("the Codex adapter does not implement {operation} yet")]
    UnsupportedCodexOperation { operation: &'static str },
    #[error("the foreground Codex launch is missing its session environment")]
    MissingInteractiveEnvironment,
    #[error(
        "could not locate the Coterie bootstrap executable: {0}; restore the Coterie installation before launching an agent"
    )]
    BootstrapExecutable(#[source] io::Error),
    #[error(
        "could not start the foreground Codex process with `{executable}`: {source}"
    )]
    InteractiveLaunch {
        executable: String,
        #[source]
        source: io::Error,
    },
    #[error(
        "could not start the background Codex process with `{executable}`: {source}"
    )]
    JobLaunch {
        executable: String,
        #[source]
        source: io::Error,
    },
    #[error(
        "could not resolve background Codex executable `{executable}` from its configured location or the trusted provider PATH"
    )]
    JobExecutableResolution { executable: String },
    #[error("the background Codex process has no captured standard output")]
    MissingJobStdout,
    #[error("could not start the background Codex output-reader thread: {0}")]
    JobReaderThread(#[source] io::Error),
    #[error(
        "could not inspect background Codex process `{provider_id}`: {source}"
    )]
    JobObservation {
        provider_id: String,
        #[source]
        source: io::Error,
    },
    #[error(
        "could not control background Codex process `{provider_id}`: {source}"
    )]
    JobControl {
        provider_id: String,
        #[source]
        source: io::Error,
    },
    #[error("could not wait for the foreground Codex process: {0}")]
    InteractiveWait(#[source] io::Error),
    #[error("could not register foreground signal handling: {0}")]
    SignalRegistration(#[source] io::Error),
    #[error("could not start the foreground signal-forwarding thread: {0}")]
    SignalThread(#[source] io::Error),
    #[error("the foreground signal monitor closed while Codex was running")]
    SignalMonitorClosed,
    #[error("cannot forward unsupported signal {signal}")]
    UnsupportedSignal { signal: i32 },
    #[error("foreground process ID {process_id} does not fit Linux's PID type")]
    InvalidProcessId { process_id: u32 },
    #[error("could not forward {signal:?} to foreground Codex: {source}")]
    SignalForward {
        signal: Signal,
        #[source]
        source: Errno,
    },
}

impl ProviderError {
    /// Only pre-spawn failures permit another attempt with the same launch intent.
    pub(crate) fn proves_no_process_started(&self) -> bool {
        matches!(
            self,
            Self::NoLaunchScript
                | Self::EmptyCodexCommand
                | Self::JobLaunch { .. }
                | Self::JobExecutableResolution { .. }
                | Self::InteractiveLaunch { .. }
                | Self::MissingInteractiveEnvironment
                | Self::BootstrapExecutable(_)
                | Self::SignalRegistration(_)
                | Self::SignalThread(_)
        )
    }
}

const MINIMUM_CODEX_VERSION: &str = "0.151.0";
const CODEX_VERSION_REQUIREMENT: &str = ">=0.151.0 and <1.0.0";

/// The installed Codex CLI, invoked only through its documented process boundary.
pub(crate) struct CodexProvider {
    supervision: crate::config::SupervisionPolicy,
    command: Vec<OsString>,
    probe_runner: Box<dyn ProbeCommandRunner>,
    interactive_sessions:
        BTreeMap<String, (SessionScope, CodexInteractiveProcess)>,
    job_sessions: BTreeMap<String, CodexJobProcess>,
}

impl CodexProvider {
    pub(crate) fn with_supervision(
        mut self,
        policy: crate::config::SupervisionPolicy,
    ) -> Self {
        self.supervision = policy;
        self
    }

    pub(crate) fn new(
        command: impl IntoIterator<Item = impl Into<OsString>>,
    ) -> Self {
        Self {
            supervision: crate::config::compiled_defaults().supervision,
            command: command.into_iter().map(Into::into).collect(),
            probe_runner: Box::new(ProcessProbeRunner),
            interactive_sessions: BTreeMap::new(),
            job_sessions: BTreeMap::new(),
        }
    }

    #[cfg(test)]
    fn with_runner(
        command: impl IntoIterator<Item = impl Into<OsString>>,
        runner: impl ProbeCommandRunner + 'static,
    ) -> Self {
        Self {
            command: command.into_iter().map(Into::into).collect(),
            supervision: crate::config::compiled_defaults().supervision,
            probe_runner: Box::new(runner),
            interactive_sessions: BTreeMap::new(),
            job_sessions: BTreeMap::new(),
        }
    }

    fn command_output(
        &self,
        arguments: &[&str],
        probe: &'static str,
    ) -> Result<ProbeOutput, ProviderError> {
        let executable = self
            .command
            .first()
            .ok_or(ProviderError::EmptyCodexCommand)?
            .to_string_lossy()
            .into_owned();
        let output = self.probe_runner.run(&self.command, arguments).map_err(
            |source| ProviderError::ProbeExecution { executable, source },
        )?;
        if !output.success {
            let status = output.code.map_or_else(
                || "termination by signal".to_owned(),
                |code| format!("status {code}"),
            );
            return Err(ProviderError::ProbeFailed {
                probe,
                status,
                diagnostic: diagnostic_output(&output.stderr, &output.stdout),
            });
        }
        Ok(output)
    }

    fn interactive_command(
        &self,
        specification: &LaunchSpecification,
        environment: &InteractiveEnvironment,
    ) -> Result<Command, ProviderError> {
        let (program, configured_arguments) = self
            .command
            .split_first()
            .ok_or(ProviderError::EmptyCodexCommand)?;
        let executable =
            env::current_exe().map_err(ProviderError::BootstrapExecutable)?;
        let bootstrap = codex_bootstrap(specification, &executable);
        let mut command = Command::new(program);
        command.args(configured_arguments);
        apply_codex_permission_profile(&mut command, specification);
        command
            .arg("--cd")
            .arg(&specification.working_directory)
            .arg("--config")
            .arg(format!("developer_instructions={bootstrap}"))
            .current_dir(&specification.working_directory)
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .env("COTERIE_BIN", &executable)
            .env("COTERIE_PROJECT_ROOT", &specification.working_directory)
            .env("COTERIE_PROJECT_ID", environment.project_id.to_string())
            .env(
                "COTERIE_PRIMARY_PROJECT_ROOT",
                &environment.primary_project_root,
            )
            .env("COTERIE_RUN_ID", specification.scope.run_id.to_string())
            .env("COTERIE_AGENT_ID", specification.scope.agent_id.to_string())
            .env(
                "COTERIE_SESSION_ID",
                specification.scope.session_id.to_string(),
            )
            .env("COTERIE_ROLE", &environment.role)
            .env("COTERIE_SOCKET", &environment.socket_path)
            .env("COTERIE_TOKEN", environment.token.expose_secret())
            .env_remove("COTERIE_TASK_ID");
        Ok(command)
    }

    fn job_command(
        &self,
        specification: &LaunchSpecification,
        environment: &JobEnvironment,
    ) -> Result<Command, ProviderError> {
        let (program, configured_arguments) = self
            .command
            .split_first()
            .ok_or(ProviderError::EmptyCodexCommand)?;
        Self::job_command_with_program(
            program,
            configured_arguments,
            specification,
            environment,
        )
    }

    fn job_command_with_program(
        program: &OsStr,
        configured_arguments: &[OsString],
        specification: &LaunchSpecification,
        environment: &JobEnvironment,
    ) -> Result<Command, ProviderError> {
        let executable =
            env::current_exe().map_err(ProviderError::BootstrapExecutable)?;
        let bootstrap = codex_bootstrap(specification, &executable);
        let mut command = Command::new(program);
        command.args(configured_arguments);
        apply_codex_permission_profile(&mut command, specification);
        command.arg("exec").arg("--json");
        command
            .arg("--cd")
            .arg(&specification.working_directory)
            .arg("--config")
            .arg(format!("developer_instructions={bootstrap}"))
            .arg("--config")
            .arg("allow_login_shell=false")
            .arg("Begin your assigned task.")
            .current_dir(&specification.working_directory)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .env_clear();
        apply_codex_runtime_environment(&mut command, env::vars_os());
        command
            .env("COTERIE_BIN", &executable)
            .env("COTERIE_PROJECT_ROOT", &specification.working_directory)
            .env("COTERIE_PROJECT_ID", environment.project_id.to_string())
            .env(
                "COTERIE_PRIMARY_PROJECT_ROOT",
                &environment.primary_project_root,
            )
            .env("COTERIE_RUN_ID", specification.scope.run_id.to_string())
            .env("COTERIE_AGENT_ID", specification.scope.agent_id.to_string())
            .env(
                "COTERIE_SESSION_ID",
                specification.scope.session_id.to_string(),
            )
            .env("COTERIE_ROLE", &environment.role)
            .env("COTERIE_TASK_ID", environment.task_id.to_string())
            .env("COTERIE_SOCKET", &environment.socket_path)
            .env("COTERIE_TOKEN", environment.token.expose_secret());
        Ok(command)
    }

    fn resolved_job_program(&self) -> Result<OsString, ProviderError> {
        let program = self
            .command
            .first()
            .ok_or(ProviderError::EmptyCodexCommand)?;
        let path = Path::new(program);
        if path.components().count() > 1 {
            return path.canonicalize().map(PathBuf::into_os_string).map_err(
                |_| ProviderError::JobExecutableResolution {
                    executable: program.to_string_lossy().into_owned(),
                },
            );
        }
        let executable = program.to_string_lossy().into_owned();
        env::var_os("PATH")
            .into_iter()
            .flat_map(|paths| env::split_paths(&paths).collect::<Vec<_>>())
            .map(|directory| directory.join(path))
            .find(|candidate| {
                candidate.metadata().is_ok_and(|metadata| {
                    metadata.is_file()
                        && metadata.permissions().mode() & 0o111 != 0
                })
            })
            .and_then(|candidate| candidate.canonicalize().ok())
            .map(PathBuf::into_os_string)
            .ok_or(ProviderError::JobExecutableResolution { executable })
    }

    fn launch_job_process(
        &self,
        specification: &LaunchSpecification,
        environment: &JobEnvironment,
    ) -> Result<CodexJobProcess, ProviderError> {
        let (configured_program, configured_arguments) = self
            .command
            .split_first()
            .ok_or(ProviderError::EmptyCodexCommand)?;
        let resolved_program = self.resolved_job_program()?;
        debug_assert!(
            Path::new(configured_program).components().count() > 1
                || Path::new(&resolved_program).is_absolute()
        );
        let mut command = Self::job_command_with_program(
            &resolved_program,
            configured_arguments,
            specification,
            environment,
        )?;
        let executable = command.get_program().to_string_lossy().into_owned();
        crate::fault::point("process.job.spawn.before");
        let mut child = command.spawn().map_err(|source| {
            ProviderError::JobLaunch { executable, source }
        })?;
        crate::fault::point("process.job.spawn.after");
        let Some(stdout) = child.stdout.take() else {
            terminate_failed_job_launch(&mut child);
            return Err(ProviderError::MissingJobStdout);
        };
        let frames = match stream_job_stdout(stdout) {
            Ok(frames) => frames,
            Err(error) => {
                terminate_failed_job_launch(&mut child);
                return Err(error);
            }
        };
        crate::fault::point("process.job.reader.after");
        Ok(CodexJobProcess {
            child,
            scope: specification.scope,
            frames,
            pending: VecDeque::from([ProviderEventKind::Observation(
                SessionObservation {
                    lifecycle: LifecycleState::Running,
                    activity: ActivityState::Busy,
                    exit: None,
                },
            )]),
            observation: SessionObservation::starting(),
            next_sequence: 1,
            stdout_closed: false,
            exit_observed: false,
        })
    }

    fn launch_foreground_process(
        &self,
        specification: &LaunchSpecification,
        environment: &InteractiveEnvironment,
    ) -> Result<CodexInteractiveProcess, ProviderError> {
        let inherited_terminal = io::stdin().is_terminal();
        let signals = SignalMonitor::install()?;
        let command = self.interactive_command(specification, environment)?;
        let executable = command.get_program().to_string_lossy().into_owned();
        crate::fault::point("process.foreground.spawn.before");
        let child = tokio::process::Command::from(command).spawn().map_err(
            |source| ProviderError::InteractiveLaunch { executable, source },
        )?;
        crate::fault::point("process.foreground.spawn.after");
        Ok(CodexInteractiveProcess {
            supervision: self.supervision,
            child,
            inherited_terminal,
            signals,
        })
    }

    pub(crate) fn foreground_process_id(
        &self,
        session: &ProviderSessionHandle,
    ) -> Result<u32, ProviderError> {
        self.interactive_session(session)
            .map(CodexInteractiveProcess::process_id)
    }

    pub(crate) async fn wait_foreground_until_termination<F>(
        &mut self,
        session: &ProviderSessionHandle,
        termination: F,
    ) -> Result<(std::process::ExitStatus, bool), ProviderError>
    where
        F: Future<Output = ()>,
    {
        self.interactive_session(session)?;
        let (_, process) = self
            .interactive_sessions
            .remove(session.provider_id())
            .ok_or_else(|| ProviderError::UnknownSession {
                provider_id: session.provider_id().to_owned(),
            })?;
        process.wait_until_termination(termination).await
    }

    fn interactive_session(
        &self,
        session: &ProviderSessionHandle,
    ) -> Result<&CodexInteractiveProcess, ProviderError> {
        self.interactive_sessions
            .get(session.provider_id())
            .filter(|(scope, _)| scope == &session.scope)
            .map(|(_, process)| process)
            .ok_or_else(|| ProviderError::UnknownSession {
                provider_id: session.provider_id().to_owned(),
            })
    }

    fn job_session(
        &self,
        session: &ProviderSessionHandle,
    ) -> Result<&CodexJobProcess, ProviderError> {
        self.job_sessions
            .get(session.provider_id())
            .filter(|process| process.scope == session.scope)
            .ok_or_else(|| ProviderError::UnknownSession {
                provider_id: session.provider_id().to_owned(),
            })
    }

    fn job_session_mut(
        &mut self,
        session: &ProviderSessionHandle,
    ) -> Result<&mut CodexJobProcess, ProviderError> {
        self.job_sessions
            .get_mut(session.provider_id())
            .filter(|process| process.scope == session.scope)
            .ok_or_else(|| ProviderError::UnknownSession {
                provider_id: session.provider_id().to_owned(),
            })
    }

    fn probe_codex(&self) -> Result<ProviderProbe, ProviderError> {
        let version_output = self.command_output(&["--version"], "version")?;
        let version_text =
            String::from_utf8(version_output.stdout).map_err(|error| {
                ProviderError::InvalidVersionOutput {
                    output: format!("non-UTF-8 output: {error}"),
                }
            })?;
        let version = parse_codex_version(&version_text)?;
        let compatibility = codex_compatibility(&version);
        if !matches!(compatibility, ProviderCompatibility::Compatible) {
            return Ok(ProviderProbe {
                name: "codex".to_owned(),
                version,
                capabilities: BTreeSet::new(),
                compatibility,
            });
        }

        let interactive_help = String::from_utf8_lossy(
            &self
                .command_output(&["--help"], "interactive capability")?
                .stdout,
        )
        .into_owned();
        let job_help = String::from_utf8_lossy(
            &self
                .command_output(&["exec", "--help"], "job capability")?
                .stdout,
        )
        .into_owned();
        let capabilities = codex_capabilities(&interactive_help, &job_help);

        Ok(ProviderProbe {
            name: "codex".to_owned(),
            version,
            capabilities,
            compatibility,
        })
    }
}

fn stream_job_stdout(
    stdout: ChildStdout,
) -> Result<Receiver<JobStreamItem>, ProviderError> {
    let (sender, receiver) =
        std::sync::mpsc::sync_channel(MAXIMUM_PENDING_CODEX_FRAMES);
    thread::Builder::new()
        .name("coterie-codex-jsonl-reader".to_owned())
        .spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let mut bytes = Vec::new();
                let read = reader
                    .by_ref()
                    .take(MAXIMUM_CODEX_JSONL_FRAME_BYTES + 1)
                    .read_until(b'\n', &mut bytes);
                match read {
                    Ok(0) => break,
                    Ok(_) => {
                        if sender.send(JobStreamItem::Frame(bytes)).is_err() {
                            break;
                        }
                    }
                    Err(error) => {
                        let _receiver_may_have_closed = sender.send(
                            JobStreamItem::ReadFailure(error.to_string()),
                        );
                        break;
                    }
                }
            }
        })
        .map_err(ProviderError::JobReaderThread)?;
    Ok(receiver)
}

fn terminate_failed_job_launch(child: &mut Child) {
    if child.try_wait().is_ok_and(|status| status.is_none()) {
        let _kill = child.kill();
        let _reap = reap_with_deadline(child, Duration::from_millis(250));
    }
}

fn codex_bootstrap(
    specification: &LaunchSpecification,
    executable: &Path,
) -> String {
    let profile = serde_json::to_string(&specification.permission_profile)
        .expect("permission profiles serialize as JSON");
    let instruction = format!(
        "Coterie CLI absolute path: {executable:?}. COTERIE_BIN contains this path; use `\"$COTERIE_BIN\"` for every Coterie command. Run `\"$COTERIE_BIN\" prime` now. Selected permission profile: {profile}. Use non-login shell tools to preserve the inherited toolchain PATH. If the executable is missing or inaccessible, report its path and the error to the operator. If the supervisor socket is denied, report its path and the selected permission profile to the operator; do not bypass the sandbox or change permissions. Never print tokens or the complete environment.\n{}",
        specification.bootstrap_instruction
    );
    serde_json::to_string(&instruction)
        .expect("a Rust string is always representable as a TOML basic string")
}

fn apply_codex_runtime_environment(
    command: &mut Command,
    environment: impl IntoIterator<Item = (OsString, OsString)>,
) {
    command.envs(environment.into_iter().filter(|(name, _)| {
        CODEX_RUNTIME_ENVIRONMENT_VARIABLES
            .iter()
            .any(|allowed| name == OsStr::new(allowed))
    }));
}

fn parse_codex_jsonl_frame(
    bytes: &[u8],
) -> Result<Option<SessionObservation>, String> {
    if bytes.len() as u64 > MAXIMUM_CODEX_JSONL_FRAME_BYTES {
        return Err(format!(
            "frame exceeds the {}-byte limit",
            MAXIMUM_CODEX_JSONL_FRAME_BYTES
        ));
    }
    let value = serde_json::from_slice::<serde_json::Value>(bytes)
        .map_err(|error| format!("invalid JSON: {error}"))?;
    let event_type = value
        .as_object()
        .and_then(|object| object.get("type"))
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            "JSONL event must be an object with a string `type`".to_owned()
        })?;
    let activity = match event_type {
        "thread.started" | "turn.started" => Some(ActivityState::Busy),
        "turn.completed" | "turn.failed" | "error" => Some(ActivityState::Idle),
        _ => None,
    };
    Ok(activity.map(|activity| SessionObservation {
        lifecycle: LifecycleState::Running,
        activity,
        exit: None,
    }))
}

impl CodexJobProcess {
    fn next_event_kind(
        &mut self,
        provider_id: &str,
    ) -> Result<Option<ProviderEventKind>, ProviderError> {
        if let Some(kind) = self.pending.pop_front() {
            if let ProviderEventKind::Observation(observation) = kind {
                self.observation = observation;
                return Ok(Some(ProviderEventKind::Observation(observation)));
            }
            return Ok(Some(kind));
        }
        if !self.stdout_closed {
            match self.frames.try_recv() {
                Ok(JobStreamItem::Frame(bytes)) => {
                    crate::fault::point("process.job.frame.after");
                    return self.classify_frame(provider_id, bytes).map(Some);
                }
                Ok(JobStreamItem::ReadFailure(diagnostic)) => {
                    return self
                        .quarantine(provider_id, Vec::new(), diagnostic)
                        .map(Some);
                }
                Err(TryRecvError::Empty) => return Ok(None),
                Err(TryRecvError::Disconnected) => {
                    self.stdout_closed = true;
                }
            }
        }
        if self.exit_observed || self.observation.lifecycle.is_terminal() {
            return Ok(None);
        }
        let status = self.child.try_wait().map_err(|source| {
            ProviderError::JobObservation {
                provider_id: provider_id.to_owned(),
                source,
            }
        })?;
        let Some(status) = status else {
            return Ok(None);
        };
        crate::fault::point("process.job.wait.after");
        self.exit_observed = true;
        let observation = SessionObservation::process_exit(status.code());
        self.observation = observation;
        Ok(Some(ProviderEventKind::Observation(observation)))
    }

    fn classify_frame(
        &mut self,
        provider_id: &str,
        bytes: Vec<u8>,
    ) -> Result<ProviderEventKind, ProviderError> {
        match parse_codex_jsonl_frame(&bytes) {
            Ok(observation) => {
                if let Some(observation) = observation {
                    self.pending
                        .push_back(ProviderEventKind::Observation(observation));
                }
                Ok(ProviderEventKind::Output(bytes))
            }
            Err(diagnostic) => self.quarantine(provider_id, bytes, diagnostic),
        }
    }

    fn quarantine(
        &mut self,
        provider_id: &str,
        bytes: Vec<u8>,
        diagnostic: String,
    ) -> Result<ProviderEventKind, ProviderError> {
        if self
            .child
            .try_wait()
            .map_err(|source| ProviderError::JobObservation {
                provider_id: provider_id.to_owned(),
                source,
            })?
            .is_none()
        {
            crate::fault::point("process.quarantine.kill.before");
            self.child
                .kill()
                .map_err(|source| ProviderError::JobControl {
                    provider_id: provider_id.to_owned(),
                    source,
                })?;
            crate::fault::point("process.quarantine.kill.after");
            self.exit_observed =
                reap_with_deadline(&mut self.child, Duration::from_millis(250))
                    .map_err(|source| ProviderError::JobControl {
                        provider_id: provider_id.to_owned(),
                        source,
                    })?
                    .is_some();
            crate::fault::point("process.quarantine.reap.after");
        }
        let reaped = self
            .child
            .try_wait()
            .map_err(|source| ProviderError::JobObservation {
                provider_id: provider_id.to_owned(),
                source,
            })?
            .is_some();
        let observation = SessionObservation {
            lifecycle: if reaped {
                LifecycleState::Quarantined
            } else {
                LifecycleState::Unknown
            },
            activity: ActivityState::Unknown,
            exit: None,
        };
        self.observation = observation;
        Ok(ProviderEventKind::MalformedOutput {
            bytes,
            diagnostic,
            observation,
        })
    }
}

impl Provider for CodexProvider {
    fn configure(&mut self, binding: &crate::config::ProviderBinding) {
        self.command = binding.command.iter().map(OsString::from).collect();
    }

    fn probe(&self) -> Result<ProviderProbe, ProviderError> {
        self.probe_codex()
    }

    fn launch_interactive(
        &mut self,
        specification: &LaunchSpecification,
        environment: Option<&InteractiveEnvironment>,
    ) -> Result<ProviderSessionHandle, ProviderError> {
        let environment =
            environment.ok_or(ProviderError::MissingInteractiveEnvironment)?;
        let process =
            self.launch_foreground_process(specification, environment)?;
        let provider_id = format!("process:{}", process.process_id());
        self.interactive_sessions
            .insert(provider_id.clone(), (specification.scope, process));
        Ok(ProviderSessionHandle::new(provider_id, specification.scope))
    }

    fn launch_job(
        &mut self,
        specification: &LaunchSpecification,
        environment: &JobEnvironment,
    ) -> Result<ProviderSessionHandle, ProviderError> {
        let process = self.launch_job_process(specification, environment)?;
        let provider_id = format!("process:{}", process.child.id());
        self.job_sessions.insert(provider_id.clone(), process);
        Ok(ProviderSessionHandle::new(provider_id, specification.scope))
    }

    fn recover(
        &self,
        provider_session_id: &str,
        scope: SessionScope,
    ) -> Result<ProviderRecovery, ProviderError> {
        if provider_session_id
            .strip_prefix("fake-session-")
            .is_some_and(|suffix| suffix.parse::<u64>().is_ok())
        {
            return Ok(ProviderRecovery::Lost);
        }
        Ok(self.job_sessions.get(provider_session_id).map_or_else(
            || {
                if process_is_absent(provider_session_id) {
                    ProviderRecovery::Lost
                } else {
                    ProviderRecovery::Unknown
                }
            },
            |process| {
                if process.scope == scope {
                    ProviderRecovery::Observed {
                        handle: ProviderSessionHandle::new(
                            provider_session_id,
                            scope,
                        ),
                        observation: process.observation,
                    }
                } else {
                    ProviderRecovery::Unknown
                }
            },
        ))
    }

    fn observe(
        &self,
        session: &ProviderSessionHandle,
    ) -> Result<SessionObservation, ProviderError> {
        if let Ok(process) = self.job_session(session) {
            return Ok(process.observation);
        }
        self.interactive_session(session)?;
        Ok(SessionObservation {
            lifecycle: LifecycleState::Running,
            activity: ActivityState::Unknown,
            exit: None,
        })
    }

    fn next_event(
        &mut self,
        session: &ProviderSessionHandle,
    ) -> Result<Option<ProviderEvent>, ProviderError> {
        let provider_id = session.provider_id().to_owned();
        let process = self.job_session_mut(session)?;
        let Some(kind) = process.next_event_kind(&provider_id)? else {
            return Ok(None);
        };
        let sequence = process.next_sequence;
        process.next_sequence += 1;
        Ok(Some(ProviderEvent { sequence, kind }))
    }

    fn interrupt(
        &mut self,
        session: &ProviderSessionHandle,
    ) -> Result<SessionObservation, ProviderError> {
        if let Ok(process) = self.job_session_mut(session) {
            if process
                .child
                .try_wait()
                .map_err(|source| ProviderError::JobControl {
                    provider_id: session.provider_id().to_owned(),
                    source,
                })?
                .is_none()
            {
                forward_signal(process.child.id(), SIGINT)?;
            }
            return Ok(process.observation);
        }
        forward_signal(
            self.interactive_session(session)?.process_id(),
            SIGINT,
        )?;
        Ok(SessionObservation {
            lifecycle: LifecycleState::Running,
            activity: ActivityState::Unknown,
            exit: None,
        })
    }

    fn terminate(
        &mut self,
        session: &ProviderSessionHandle,
    ) -> Result<SessionObservation, ProviderError> {
        if let Ok(process) = self.job_session_mut(session) {
            if process
                .child
                .try_wait()
                .map_err(|source| ProviderError::JobControl {
                    provider_id: session.provider_id().to_owned(),
                    source,
                })?
                .is_none()
            {
                forward_signal(process.child.id(), SIGTERM)?;
            }
            return Ok(process.observation);
        }
        self.interactive_session(session)?.terminate()?;
        Ok(SessionObservation {
            lifecycle: LifecycleState::Running,
            activity: ActivityState::Unknown,
            exit: None,
        })
    }
    fn kill(
        &mut self,
        session: &ProviderSessionHandle,
    ) -> Result<SessionObservation, ProviderError> {
        if let Ok(process) = self.job_session_mut(session) {
            if process
                .child
                .try_wait()
                .map_err(|source| ProviderError::JobControl {
                    provider_id: session.provider_id().to_owned(),
                    source,
                })?
                .is_none()
            {
                crate::fault::point("process.kill.before");
                process.child.kill().map_err(|source| {
                    ProviderError::JobControl {
                        provider_id: session.provider_id().to_owned(),
                        source,
                    }
                })?;
                crate::fault::point("process.kill.after");
            }
            return Ok(process.observation);
        }
        forward_signal(
            self.interactive_session(session)?.process_id(),
            SIGKILL,
        )?;
        Ok(SessionObservation {
            lifecycle: LifecycleState::Running,
            activity: ActivityState::Unknown,
            exit: None,
        })
    }
}

struct ProbeOutput {
    success: bool,
    code: Option<i32>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

trait ProbeCommandRunner {
    fn run(
        &self,
        command: &[OsString],
        arguments: &[&str],
    ) -> Result<ProbeOutput, io::Error>;
}

struct ProcessProbeRunner;

impl ProbeCommandRunner for ProcessProbeRunner {
    fn run(
        &self,
        command: &[OsString],
        arguments: &[&str],
    ) -> Result<ProbeOutput, io::Error> {
        self.run_bounded(command, arguments, Duration::from_secs(2))
    }
}

impl ProcessProbeRunner {
    fn run_bounded(
        &self,
        command: &[OsString],
        arguments: &[&str],
        timeout: Duration,
    ) -> Result<ProbeOutput, io::Error> {
        let (program, configured_arguments) =
            command.split_first().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "empty provider command",
                )
            })?;
        crate::fault::point("process.probe.before");
        let mut child = Command::new(program)
            .args(configured_arguments)
            .args(arguments)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        crate::fault::point("process.probe.spawned");
        let result = (|| {
            let mut stdout = child
                .stdout
                .take()
                .ok_or_else(|| io::Error::other("missing probe stdout"))?;
            let mut stderr = child
                .stderr
                .take()
                .ok_or_else(|| io::Error::other("missing probe stderr"))?;
            set_pipe_nonblocking(&stdout)?;
            set_pipe_nonblocking(&stderr)?;
            let mut output = ProbeOutput {
                success: false,
                code: None,
                stdout: Vec::new(),
                stderr: Vec::new(),
            };
            let deadline = std::time::Instant::now() + timeout;
            loop {
                let stdout_closed =
                    read_probe_pipe(&mut stdout, &mut output.stdout)?;
                let stderr_closed =
                    read_probe_pipe(&mut stderr, &mut output.stderr)?;
                let status = child.try_wait()?;
                if stdout_closed
                    && stderr_closed
                    && let Some(status) = status
                {
                    crate::fault::point("process.probe.observed");
                    output.success = status.success();
                    output.code = status.code();
                    return Ok(output);
                }
                if std::time::Instant::now() >= deadline {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "provider probe exceeded its deadline",
                    ));
                }
                thread::sleep(Duration::from_millis(5));
            }
        })();
        if result.is_err() && child.try_wait()?.is_none() {
            child.kill()?;
            if reap_with_deadline(&mut child, Duration::from_millis(250))?
                .is_none()
            {
                thread::spawn(move || {
                    let _status = child.wait();
                });
            }
        }
        result
    }
}

fn set_pipe_nonblocking(pipe: &impl std::os::fd::AsFd) -> io::Result<()> {
    use nix::fcntl::{FcntlArg, OFlag, fcntl};
    let flags = fcntl(pipe, FcntlArg::F_GETFL).map_err(io::Error::from)?;
    fcntl(
        pipe,
        FcntlArg::F_SETFL(OFlag::from_bits_retain(flags) | OFlag::O_NONBLOCK),
    )
    .map_err(io::Error::from)?;
    Ok(())
}

fn read_probe_pipe(
    reader: &mut impl Read,
    output: &mut Vec<u8>,
) -> io::Result<bool> {
    let mut buffer = [0; 8192];
    // A bounded batch keeps a noisy stream from starving the other pipe or the deadline.
    for _ in 0..8 {
        match reader.read(&mut buffer) {
            Ok(0) => return Ok(true),
            Ok(count) => {
                if output.len() + count > 1024 * 1024 {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "provider probe output exceeded 1 MiB",
                    ));
                }
                output.extend_from_slice(&buffer[..count]);
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                return Ok(false);
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(false)
}

fn reap_with_deadline(
    child: &mut Child,
    timeout: Duration,
) -> io::Result<Option<std::process::ExitStatus>> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let status = child.try_wait()?;
        if status.is_some() || std::time::Instant::now() >= deadline {
            return Ok(status);
        }
        thread::sleep(Duration::from_millis(5));
    }
}

fn parse_codex_version(output: &str) -> Result<Version, ProviderError> {
    let fields = output.split_whitespace().collect::<Vec<_>>();
    if fields.len() != 2 || fields[0] != "codex-cli" {
        return Err(ProviderError::InvalidVersionOutput {
            output: output.trim().to_owned(),
        });
    }
    Version::parse(fields[1]).map_err(|_| ProviderError::InvalidVersionOutput {
        output: output.trim().to_owned(),
    })
}

fn codex_compatibility(version: &Version) -> ProviderCompatibility {
    let minimum = Version::parse(MINIMUM_CODEX_VERSION)
        .expect("the compiled Codex minimum version is valid");
    let reason = if version < &minimum {
        Some(format!(
            "Codex CLI {version} is older than the minimum supported version {minimum}"
        ))
    } else if version.major != 0 {
        Some(format!(
            "Codex CLI {version} has an unvalidated major version"
        ))
    } else {
        None
    };
    reason.map_or(ProviderCompatibility::Compatible, |reason| {
        ProviderCompatibility::Incompatible {
            reason,
            remedy: format!(
                "run `codex update` or install Codex CLI {CODEX_VERSION_REQUIREMENT}; upgrade Coterie before using a newer Codex major release"
            ),
        }
    })
}

fn codex_capabilities(
    interactive_help: &str,
    job_help: &str,
) -> BTreeSet<ProviderCapability> {
    let interactive = interactive_help.contains("Usage: codex ");
    let job = job_help.contains("Usage: codex exec ");
    let structured_job = job && job_help.contains("--json");
    let both_contain =
        |option| interactive_help.contains(option) && job_help.contains(option);
    let mut capabilities = BTreeSet::new();
    if interactive {
        capabilities.insert(ProviderCapability::ForegroundInteractive);
    }
    if job {
        capabilities.insert(ProviderCapability::BackgroundJobs);
    }
    if interactive || job {
        capabilities.insert(ProviderCapability::Interrupt);
        capabilities.insert(ProviderCapability::Termination);
    }
    if interactive && job && both_contain("--config") {
        capabilities.insert(ProviderCapability::StartupInstructions);
    }
    if structured_job {
        capabilities.insert(ProviderCapability::StructuredLifecycleEvents);
        capabilities.insert(ProviderCapability::TranscriptStreaming);
    }
    if interactive && job && both_contain("--cd") {
        capabilities.insert(ProviderCapability::WorkingDirectory);
    }
    if interactive && job && both_contain("--sandbox") {
        capabilities.insert(ProviderCapability::FilesystemSandbox);
    }
    if interactive && job && both_contain("--config") {
        capabilities.insert(ProviderCapability::NetworkSandbox);
    }
    if interactive && interactive_help.contains("--ask-for-approval") {
        capabilities.insert(ProviderCapability::ApprovalPolicy);
    }
    capabilities
}

fn apply_codex_permission_profile(
    command: &mut Command,
    specification: &LaunchSpecification,
) {
    let profile = specification.permission_profile;
    let sandbox = match profile.filesystem {
        FilesystemPolicy::ProjectWrite | FilesystemPolicy::WorkspaceWrite => {
            "workspace-write"
        }
        FilesystemPolicy::ReadOnly => "read-only",
    };
    let approvals = match profile.approvals {
        ApprovalPolicy::Interactive => "on-request",
        ApprovalPolicy::Never => "never",
    };
    command
        .arg("--sandbox")
        .arg(sandbox)
        .arg("--ask-for-approval")
        .arg(approvals);
    if profile.approvals == ApprovalPolicy::Interactive {
        command.arg("--config").arg("approvals_reviewer=\"user\"");
    }
    if profile.network == NetworkPolicy::Deny {
        command
            .arg("--config")
            .arg("sandbox_workspace_write.network_access=false")
            .arg("--config")
            .arg("web_search=\"disabled\"");
    }
}

fn diagnostic_output(primary: &[u8], fallback: &[u8]) -> String {
    let bytes = if primary.is_empty() {
        fallback
    } else {
        primary
    };
    let output = crate::redaction::text(&String::from_utf8_lossy(bytes));
    let output = output.trim();
    if output.is_empty() {
        "no diagnostic output".to_owned()
    } else {
        output.chars().take(512).collect()
    }
}

pub(crate) mod fake {
    use std::collections::{BTreeMap, BTreeSet, VecDeque};

    use semver::Version;

    use crate::auth::SessionScope;

    use super::{
        LaunchMode, LaunchSpecification, Provider, ProviderCapability,
        ProviderCompatibility, ProviderError, ProviderEvent, ProviderEventKind,
        ProviderProbe, ProviderRecovery, ProviderSessionHandle,
        SessionObservation,
    };

    #[derive(Clone, Debug, Eq, PartialEq)]
    pub(crate) struct FakeEvent(ProviderEventKind);

    impl FakeEvent {
        pub(crate) fn observation(observation: SessionObservation) -> Self {
            Self(ProviderEventKind::Observation(observation))
        }

        pub(crate) fn output(output: &[u8]) -> Self {
            Self(ProviderEventKind::Output(output.to_vec()))
        }

        #[cfg(test)]
        pub(crate) fn malformed(output: &[u8], diagnostic: &str) -> Self {
            Self(ProviderEventKind::MalformedOutput {
                bytes: output.to_vec(),
                diagnostic: diagnostic.to_owned(),
                observation: SessionObservation {
                    lifecycle: super::LifecycleState::Quarantined,
                    activity: super::ActivityState::Unknown,
                    exit: None,
                },
            })
        }
    }

    #[derive(Clone, Debug, Eq, PartialEq)]
    pub(crate) struct FakeScript {
        events: VecDeque<FakeEvent>,
    }

    impl FakeScript {
        pub(crate) fn new(events: impl IntoIterator<Item = FakeEvent>) -> Self {
            Self {
                events: events.into_iter().collect(),
            }
        }
    }

    #[derive(Clone, Debug, Eq, PartialEq)]
    pub(crate) struct FakeLaunch {
        pub(crate) mode: LaunchMode,
        pub(crate) specification: LaunchSpecification,
    }

    struct FakeSession {
        scope: SessionScope,
        observation: SessionObservation,
        next_sequence: u64,
        events: VecDeque<FakeEvent>,
    }

    /// A model-free provider whose behavior comes only from supplied scripts.
    pub(crate) struct FakeProvider {
        scripts: VecDeque<FakeScript>,
        sessions: BTreeMap<String, FakeSession>,
        launches: Vec<FakeLaunch>,
        next_session: u64,
        #[cfg(test)]
        ignored_controls: BTreeSet<&'static str>,
        #[cfg(test)]
        pub(crate) controls: Vec<(SessionScope, &'static str)>,
        capabilities: BTreeSet<ProviderCapability>,
        compatibility: ProviderCompatibility,
        #[cfg(test)]
        missing_recoveries: std::cell::RefCell<VecDeque<ProviderRecovery>>,
    }

    impl FakeProvider {
        pub(crate) fn new(
            scripts: impl IntoIterator<Item = FakeScript>,
        ) -> Self {
            Self {
                scripts: scripts.into_iter().collect(),
                sessions: BTreeMap::new(),
                launches: Vec::new(),
                next_session: 1,
                #[cfg(test)]
                ignored_controls: BTreeSet::new(),
                #[cfg(test)]
                controls: Vec::new(),
                capabilities: BTreeSet::from([
                    ProviderCapability::StartupInstructions,
                    ProviderCapability::ForegroundInteractive,
                    ProviderCapability::BackgroundJobs,
                    ProviderCapability::StructuredLifecycleEvents,
                    ProviderCapability::WorkingDirectory,
                    ProviderCapability::FilesystemSandbox,
                    ProviderCapability::NetworkSandbox,
                    ProviderCapability::ApprovalPolicy,
                    ProviderCapability::Interrupt,
                    ProviderCapability::Termination,
                    ProviderCapability::TranscriptStreaming,
                ]),
                compatibility: ProviderCompatibility::Compatible,
                #[cfg(test)]
                missing_recoveries: std::cell::RefCell::new(VecDeque::new()),
            }
        }

        #[cfg(test)]
        pub(crate) fn ignoring_controls(
            mut self,
            controls: impl IntoIterator<Item = &'static str>,
        ) -> Self {
            self.ignored_controls.extend(controls);
            self
        }

        #[cfg(test)]
        fn control(
            &mut self,
            session: &ProviderSessionHandle,
            kind: &'static str,
        ) -> Option<Result<SessionObservation, ProviderError>> {
            self.controls.push((session.scope, kind));
            self.ignored_controls
                .contains(kind)
                .then(|| self.observe(session))
        }

        #[cfg(test)]
        pub(crate) fn with_missing_recoveries(
            self,
            recoveries: impl IntoIterator<Item = ProviderRecovery>,
        ) -> Self {
            self.missing_recoveries.borrow_mut().extend(recoveries);
            self
        }

        #[cfg(test)]
        pub(crate) fn with_compatibility(
            mut self,
            compatibility: ProviderCompatibility,
        ) -> Self {
            self.compatibility = compatibility;
            self
        }

        #[cfg(test)]
        pub(crate) fn without_capability(
            mut self,
            capability: ProviderCapability,
        ) -> Self {
            self.capabilities.remove(&capability);
            self
        }

        pub(crate) fn launches(&self) -> &[FakeLaunch] {
            &self.launches
        }

        fn launch(
            &mut self,
            mode: LaunchMode,
            specification: &LaunchSpecification,
        ) -> Result<ProviderSessionHandle, ProviderError> {
            let script = self
                .scripts
                .pop_front()
                .ok_or(ProviderError::NoLaunchScript)?;
            let provider_id = format!("fake-session-{}", self.next_session);
            self.next_session += 1;
            self.launches.push(FakeLaunch {
                mode,
                specification: specification.clone(),
            });
            self.sessions.insert(
                provider_id.clone(),
                FakeSession {
                    scope: specification.scope,
                    observation: SessionObservation::starting(),
                    next_sequence: 1,
                    events: script.events,
                },
            );
            Ok(ProviderSessionHandle::new(provider_id, specification.scope))
        }

        fn session(
            &self,
            handle: &ProviderSessionHandle,
        ) -> Result<&FakeSession, ProviderError> {
            self.sessions.get(handle.provider_id()).ok_or_else(|| {
                ProviderError::UnknownSession {
                    provider_id: handle.provider_id().to_owned(),
                }
            })
        }

        fn session_mut(
            &mut self,
            handle: &ProviderSessionHandle,
        ) -> Result<&mut FakeSession, ProviderError> {
            self.sessions.get_mut(handle.provider_id()).ok_or_else(|| {
                ProviderError::UnknownSession {
                    provider_id: handle.provider_id().to_owned(),
                }
            })
        }

        fn stop(
            &mut self,
            handle: &ProviderSessionHandle,
            observation: SessionObservation,
        ) -> Result<SessionObservation, ProviderError> {
            let session = self.session_mut(handle)?;
            if !session.observation.lifecycle.is_terminal() {
                session.observation = observation;
                session.events.clear();
            }
            Ok(session.observation)
        }
    }

    impl Provider for FakeProvider {
        fn configure(&mut self, _binding: &crate::config::ProviderBinding) {}

        fn probe(&self) -> Result<ProviderProbe, ProviderError> {
            Ok(ProviderProbe {
                name: "fake".to_owned(),
                version: Version::new(1, 0, 0),
                capabilities: self.capabilities.clone(),
                compatibility: self.compatibility.clone(),
            })
        }

        fn launch_interactive(
            &mut self,
            specification: &LaunchSpecification,
            _environment: Option<&super::InteractiveEnvironment>,
        ) -> Result<ProviderSessionHandle, ProviderError> {
            self.launch(LaunchMode::Interactive, specification)
        }

        fn launch_job(
            &mut self,
            specification: &LaunchSpecification,
            _environment: &super::JobEnvironment,
        ) -> Result<ProviderSessionHandle, ProviderError> {
            self.launch(LaunchMode::Job, specification)
        }

        fn recover(
            &self,
            provider_session_id: &str,
            scope: SessionScope,
        ) -> Result<ProviderRecovery, ProviderError> {
            let Some(session) = self.sessions.get(provider_session_id) else {
                #[cfg(test)]
                if let Some(recovery) =
                    self.missing_recoveries.borrow_mut().pop_front()
                {
                    return Ok(recovery);
                }
                return Ok(ProviderRecovery::Lost);
            };
            if session.scope != scope {
                return Ok(ProviderRecovery::Unknown);
            }
            Ok(ProviderRecovery::Observed {
                handle: ProviderSessionHandle::new(provider_session_id, scope),
                observation: session.observation,
            })
        }

        fn observe(
            &self,
            session: &ProviderSessionHandle,
        ) -> Result<SessionObservation, ProviderError> {
            Ok(self.session(session)?.observation)
        }

        fn next_event(
            &mut self,
            session: &ProviderSessionHandle,
        ) -> Result<Option<ProviderEvent>, ProviderError> {
            let session = self.session_mut(session)?;
            let Some(event) = session.events.pop_front() else {
                return Ok(None);
            };
            let sequence = session.next_sequence;
            session.next_sequence += 1;
            if let ProviderEventKind::Observation(observation)
            | ProviderEventKind::MalformedOutput { observation, .. } =
                event.0
            {
                session.observation = observation;
            }
            Ok(Some(ProviderEvent {
                sequence,
                kind: event.0,
            }))
        }

        fn interrupt(
            &mut self,
            session: &ProviderSessionHandle,
        ) -> Result<SessionObservation, ProviderError> {
            #[cfg(test)]
            if let Some(result) = self.control(session, "interrupt") {
                return result;
            }
            self.stop(session, SessionObservation::interrupted())
        }

        fn terminate(
            &mut self,
            session: &ProviderSessionHandle,
        ) -> Result<SessionObservation, ProviderError> {
            #[cfg(test)]
            if let Some(result) = self.control(session, "terminate") {
                return result;
            }
            self.stop(session, SessionObservation::terminated())
        }

        fn kill(
            &mut self,
            session: &ProviderSessionHandle,
        ) -> Result<SessionObservation, ProviderError> {
            #[cfg(test)]
            if let Some(result) = self.control(session, "kill") {
                return result;
            }
            self.stop(session, SessionObservation::terminated())
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn diagnostic_redaction_precedes_output_truncation() {
        let input = format!("{}cot1_{}", "x".repeat(505), "ab".repeat(32));
        let diagnostic = super::diagnostic_output(input.as_bytes(), b"");
        assert!(!diagnostic.contains("cot1_"));
        assert!(diagnostic.chars().count() <= 512);
    }
    use std::cell::RefCell;
    use std::collections::{BTreeMap, BTreeSet, VecDeque};
    use std::ffi::{OsStr, OsString};
    use std::fs;
    use std::io;
    use std::path::PathBuf;
    use std::process::Command;
    use std::rc::Rc;
    use std::thread;
    use std::time::{Duration, Instant};

    use super::fake::{FakeEvent, FakeProvider, FakeScript};
    use super::{
        ActivityState, CODEX_RUNTIME_ENVIRONMENT_VARIABLES, CodexProvider,
        JobEnvironment, LaunchMode, LaunchSpecification, LifecycleState,
        ProbeCommandRunner, ProbeOutput, Provider, ProviderCapability,
        ProviderCompatibility, ProviderError, ProviderEventKind,
        ProviderRecovery, SessionObservation,
    };
    use crate::auth::{AgentToken, SessionScope};
    use crate::config::PermissionProfile;
    use crate::id::{AgentId, ProjectId, RunId, SessionId, TaskId};

    const RUN_ID: &str = "cr-01ARZ3NDEKTSV4RRFFQ69G5FAV";
    const AGENT_ID: &str = "cg-01ARZ3NDEKTSV4RRFFQ69G5FAX";
    const SESSION_ID: &str = "cs-01ARZ3NDEKTSV4RRFFQ69G5FAY";
    const PROJECT_ID: &str = "cp-01ARZ3NDEKTSV4RRFFQ69G5FAW";
    const TASK_ID: &str = "ct-01ARZ3NDEKTSV4RRFFQ69G5FAZ";

    #[test]
    fn codex_probe_discovers_the_supported_command_surface() {
        let runner = ScriptedProbeRunner::new([
            Ok(success("codex-cli 0.151.0\n")),
            Ok(success(
                "Usage: codex [OPTIONS] [PROMPT]\n  --config <key=value>\n  --cd <DIR>\n  --sandbox <SANDBOX_MODE>\n  --ask-for-approval <APPROVAL_POLICY>\n",
            )),
            Ok(success(
                "Usage: codex exec [OPTIONS] [PROMPT]\n  --config <key=value>\n  --cd <DIR>\n  --sandbox <SANDBOX_MODE>\n  --json\n",
            )),
        ]);
        let observations = runner.observations();
        let provider = CodexProvider::with_runner(
            ["codex-wrapper", "--provider", "codex"],
            runner,
        );

        let probe = provider.probe().expect("the probe should complete");

        assert_eq!(probe.name, "codex");
        assert_eq!(probe.version, semver::Version::new(0, 151, 0));
        assert_eq!(probe.compatibility, ProviderCompatibility::Compatible);
        assert_eq!(
            probe.capabilities,
            BTreeSet::from([
                ProviderCapability::StartupInstructions,
                ProviderCapability::ForegroundInteractive,
                ProviderCapability::BackgroundJobs,
                ProviderCapability::StructuredLifecycleEvents,
                ProviderCapability::TranscriptStreaming,
                ProviderCapability::WorkingDirectory,
                ProviderCapability::FilesystemSandbox,
                ProviderCapability::NetworkSandbox,
                ProviderCapability::ApprovalPolicy,
                ProviderCapability::Interrupt,
                ProviderCapability::Termination,
            ])
        );
        assert_eq!(
            observations.borrow().as_slice(),
            [
                vec![
                    OsString::from("codex-wrapper"),
                    OsString::from("--provider"),
                    OsString::from("codex"),
                    OsString::from("--version"),
                ],
                vec![
                    OsString::from("codex-wrapper"),
                    OsString::from("--provider"),
                    OsString::from("codex"),
                    OsString::from("--help"),
                ],
                vec![
                    OsString::from("codex-wrapper"),
                    OsString::from("--provider"),
                    OsString::from("codex"),
                    OsString::from("exec"),
                    OsString::from("--help"),
                ],
            ]
        );
    }

    #[test]
    fn codex_probe_does_not_claim_missing_permission_controls() {
        const INTERACTIVE_HELP: &str = "Usage: codex [OPTIONS] [PROMPT]\n  --config <key=value>\n  --cd <DIR>\n  --sandbox <SANDBOX_MODE>\n  --ask-for-approval <APPROVAL_POLICY>\n";
        const JOB_HELP: &str = "Usage: codex exec [OPTIONS] [PROMPT]\n  --config <key=value>\n  --cd <DIR>\n  --sandbox <SANDBOX_MODE>\n  --json\n";

        for (option, capability, remove_from_interactive) in [
            ("--cd", ProviderCapability::WorkingDirectory, false),
            ("--sandbox", ProviderCapability::FilesystemSandbox, false),
            ("--config", ProviderCapability::NetworkSandbox, false),
            (
                "--ask-for-approval",
                ProviderCapability::ApprovalPolicy,
                true,
            ),
        ] {
            let interactive_help = if remove_from_interactive {
                INTERACTIVE_HELP.replace(option, "--unsupported")
            } else {
                INTERACTIVE_HELP.to_owned()
            };
            let job_help = if remove_from_interactive {
                JOB_HELP.to_owned()
            } else {
                JOB_HELP.replace(option, "--unsupported")
            };
            let provider = CodexProvider::with_runner(
                ["codex"],
                ScriptedProbeRunner::new([
                    Ok(success("codex-cli 0.151.0\n")),
                    Ok(success(&interactive_help)),
                    Ok(success(&job_help)),
                ]),
            );

            let probe = provider.probe().expect("the probe should complete");

            assert!(
                !probe.capabilities.contains(&capability),
                "the probe must not claim {capability} without `{option}`"
            );
        }
    }

    #[test]
    fn codex_probe_marks_unvalidated_versions_incompatible() {
        for (version, expected_reason) in [
            (
                "0.150.0",
                "is older than the minimum supported version 0.151.0",
            ),
            ("1.0.0", "has an unvalidated major version"),
        ] {
            let provider = CodexProvider::with_runner(
                ["codex"],
                ScriptedProbeRunner::new([Ok(success(&format!(
                    "codex-cli {version}\n"
                )))]),
            );

            let probe = provider.probe().expect("the version should be read");

            assert_eq!(probe.version, version.parse().expect("valid version"));
            assert!(probe.capabilities.is_empty());
            assert!(matches!(
                probe.compatibility,
                ProviderCompatibility::Incompatible { ref reason, ref remedy }
                    if reason.contains(expected_reason)
                        && remedy.contains("codex update")
                        && remedy.contains("0.151.0")
            ));
        }
    }

    #[test]
    fn codex_probe_does_not_claim_missing_job_capabilities() {
        let provider = CodexProvider::with_runner(
            ["codex"],
            ScriptedProbeRunner::new([
                Ok(success("codex-cli 0.151.0\n")),
                Ok(success(
                    "Usage: codex [OPTIONS] [PROMPT]\n  --config <key=value>\n  --cd <DIR>\n",
                )),
                Ok(success(
                    "Usage: codex exec [OPTIONS] [PROMPT]\n  --config <key=value>\n  --cd <DIR>\n",
                )),
            ]),
        );

        let probe = provider.probe().expect("the probe should complete");

        assert!(
            probe
                .capabilities
                .contains(&ProviderCapability::BackgroundJobs)
        );
        assert!(
            !probe
                .capabilities
                .contains(&ProviderCapability::StructuredLifecycleEvents)
        );
        assert!(
            !probe
                .capabilities
                .contains(&ProviderCapability::TranscriptStreaming)
        );
    }

    #[test]
    fn codex_probe_rejects_malformed_version_output() {
        for output in [
            "0.151.0\n",
            "codex-cli newest\n",
            "codex-cli 0.151.0 unexpected\n",
        ] {
            let provider = CodexProvider::with_runner(
                ["codex"],
                ScriptedProbeRunner::new([Ok(success(output))]),
            );

            let error = provider
                .probe()
                .expect_err("ambiguous versions must fail closed");

            assert!(matches!(
                error,
                ProviderError::InvalidVersionOutput { .. }
            ));
            assert!(error.to_string().contains("`codex --version`"));
        }
    }

    #[test]
    fn codex_probe_failure_suggests_how_to_install_or_update_codex() {
        let provider = CodexProvider::with_runner(
            ["missing-codex"],
            ScriptedProbeRunner::new([Err(io::Error::new(
                io::ErrorKind::NotFound,
                "fixture executable is absent",
            ))]),
        );

        let error = provider
            .probe()
            .expect_err("an absent provider must fail the probe");
        let message = error.to_string();

        assert!(matches!(error, ProviderError::ProbeExecution { .. }));
        assert!(message.contains("missing-codex"));
        assert!(message.contains("install Codex CLI"));
        assert!(message.contains("PATH"));
    }

    #[test]
    fn codex_capability_probe_failure_is_actionable() {
        let provider = CodexProvider::with_runner(
            ["codex"],
            ScriptedProbeRunner::new([
                Ok(success("codex-cli 0.151.0\n")),
                Ok(failure(2, "unknown option `--help`\n")),
            ]),
        );

        let error = provider
            .probe()
            .expect_err("a failed capability probe must fail closed");
        let message = error.to_string();

        assert!(matches!(error, ProviderError::ProbeFailed { .. }));
        assert!(message.contains("interactive capability"));
        assert!(message.contains("status 2"));
        assert!(message.contains("unknown option `--help`"));
        assert!(message.contains("codex update"));
    }

    #[test]
    fn codex_interactive_command_preserves_codex_startup_contract() {
        let provider =
            CodexProvider::new(["codex-wrapper", "--provider", "codex"]);
        let mut specification = specification();
        specification.permission_profile = permission_profile("interactive");
        let token = AgentToken::generate()
            .expect("the launch credential should be generated");
        let environment = super::InteractiveEnvironment {
            project_id: "cp-01ARZ3NDEKTSV4RRFFQ69G5FAW"
                .parse::<ProjectId>()
                .expect("valid project ID"),
            primary_project_root: PathBuf::from("/tmp/project"),
            role: "lead".to_owned(),
            socket_path: PathBuf::from("/tmp/coterie.sock"),
            token,
        };

        let command = provider
            .interactive_command(&specification, &environment)
            .expect("the interactive command should be valid");
        let arguments = command.get_args().collect::<Vec<_>>();

        assert_eq!(command.get_program(), "codex-wrapper");
        assert_eq!(
            command.get_current_dir(),
            Some(PathBuf::from("/tmp/project").as_path())
        );
        assert_eq!(
            arguments[..8],
            [
                "--provider",
                "codex",
                "--sandbox",
                "workspace-write",
                "--ask-for-approval",
                "on-request",
                "--config",
                "approvals_reviewer=\"user\"",
            ]
        );
        assert_eq!(arguments[8], "--cd");
        assert_eq!(arguments[9], "/tmp/project");
        assert_eq!(arguments[10], "--config");
        let override_value = arguments[11]
            .to_str()
            .expect("the config override should be UTF-8");
        assert!(override_value.starts_with("developer_instructions=\""));
        assert!(override_value.contains("Run `coterie prime`"));
        assert_eq!(arguments.len(), 12, "the bootstrap must not be a prompt");

        let variables = command
            .get_envs()
            .map(|(name, value)| {
                (
                    name.to_string_lossy().into_owned(),
                    value.map(|value| value.to_string_lossy().into_owned()),
                )
            })
            .collect::<std::collections::BTreeMap<_, _>>();
        assert_eq!(
            variables["COTERIE_PROJECT_ROOT"],
            Some("/tmp/project".to_owned())
        );
        assert_eq!(
            variables["COTERIE_PROJECT_ID"],
            Some("cp-01ARZ3NDEKTSV4RRFFQ69G5FAW".to_owned())
        );
        assert_eq!(
            variables["COTERIE_PRIMARY_PROJECT_ROOT"],
            Some("/tmp/project".to_owned())
        );
        assert_eq!(variables["COTERIE_RUN_ID"], Some(RUN_ID.to_owned()));
        assert_eq!(variables["COTERIE_AGENT_ID"], Some(AGENT_ID.to_owned()));
        assert_eq!(
            variables["COTERIE_SESSION_ID"],
            Some(SESSION_ID.to_owned())
        );
        assert_eq!(variables["COTERIE_ROLE"], Some("lead".to_owned()));
        assert_eq!(
            variables["COTERIE_SOCKET"],
            Some("/tmp/coterie.sock".to_owned())
        );
        assert!(
            variables["COTERIE_TOKEN"]
                .as_deref()
                .is_some_and(|value| value.starts_with("cot1_"))
        );
        assert_eq!(variables["COTERIE_TASK_ID"], None);
    }

    #[test]
    fn codex_job_command_uses_jsonl_and_a_trusted_runtime_environment() {
        let provider =
            CodexProvider::new(["codex-wrapper", "--provider", "codex"]);
        let specification = specification();
        let environment = job_environment();

        let command = provider
            .job_command(&specification, &environment)
            .expect("the job command should be valid");
        let arguments = command
            .get_args()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect::<Vec<_>>();

        assert_eq!(command.get_program(), "codex-wrapper");
        assert_eq!(
            command.get_current_dir(),
            Some(PathBuf::from("/tmp/project").as_path())
        );
        assert_eq!(
            &arguments[..15],
            [
                "--provider",
                "codex",
                "--sandbox",
                "workspace-write",
                "--ask-for-approval",
                "never",
                "--config",
                "sandbox_workspace_write.network_access=false",
                "--config",
                "web_search=\"disabled\"",
                "exec",
                "--json",
                "--cd",
                "/tmp/project",
                "--config",
            ]
        );
        assert!(arguments[15].starts_with("developer_instructions=\""));
        assert!(arguments[15].contains("Run `coterie prime`"));
        assert_eq!(arguments[16], "--config");
        assert_eq!(arguments[17], "allow_login_shell=false");
        assert_eq!(arguments[18], "Begin your assigned task.");

        let variables = command
            .get_envs()
            .map(|(name, value)| {
                (
                    name.to_string_lossy().into_owned(),
                    value.map(|value| value.to_string_lossy().into_owned()),
                )
            })
            .collect::<BTreeMap<_, _>>();
        assert!(variables.keys().all(|name| {
            name.starts_with("COTERIE_")
                || CODEX_RUNTIME_ENVIRONMENT_VARIABLES.contains(&name.as_str())
        }));
        assert_eq!(
            variables["PATH"],
            std::env::var_os("PATH")
                .map(|value| value.to_string_lossy().into_owned())
        );
        assert_eq!(variables["COTERIE_TASK_ID"], Some(TASK_ID.to_owned()));
        assert!(
            variables["COTERIE_TOKEN"]
                .as_deref()
                .is_some_and(|value| value.starts_with("cot1_"))
        );

        let mut command = Command::new("codex-wrapper");
        command.env_clear();
        super::apply_codex_runtime_environment(
            &mut command,
            [
                (OsString::from("PATH"), OsString::from("/project/bin")),
                (OsString::from("HOME"), OsString::from("/home/operator")),
                (OsString::from("CODEX_HOME"), OsString::from("/run/codex")),
                (OsString::from("OPENAI_API_KEY"), OsString::from("test-key")),
                (
                    OsString::from("PROJECT_SECRET"),
                    OsString::from("must-not-pass"),
                ),
            ],
        );
        let inherited = command
            .get_envs()
            .map(|(name, value)| (name.to_owned(), value.map(OsStr::to_owned)))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(inherited[OsStr::new("PATH")], Some("/project/bin".into()));
        assert_eq!(
            inherited[OsStr::new("HOME")],
            Some("/home/operator".into())
        );
        assert_eq!(
            inherited[OsStr::new("CODEX_HOME")],
            Some("/run/codex".into())
        );
        assert_eq!(
            inherited[OsStr::new("OPENAI_API_KEY")],
            Some("test-key".into())
        );
        assert!(!inherited.contains_key(OsStr::new("PROJECT_SECRET")));
    }

    #[test]
    fn codex_review_profile_is_read_only_offline_and_non_interactive() {
        let provider = CodexProvider::new(["codex"]);
        let mut specification = specification();
        specification.permission_profile = permission_profile("review");

        let command = provider
            .job_command(&specification, &job_environment())
            .expect("the review command should be valid");
        let arguments = command
            .get_args()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect::<Vec<_>>();

        assert_eq!(
            &arguments[..12],
            [
                "--sandbox",
                "read-only",
                "--ask-for-approval",
                "never",
                "--config",
                "sandbox_workspace_write.network_access=false",
                "--config",
                "web_search=\"disabled\"",
                "exec",
                "--json",
                "--cd",
                "/tmp/project",
            ]
        );
    }

    #[test]
    fn probe_timeouts_and_output_limits_reap_the_owned_child() {
        for (script, expected) in [
            ("while :; do :; done", std::io::ErrorKind::TimedOut),
            (
                "while :; do printf '0123456789012345678901234567890123456789012345678901234567890123456789\\n'; done",
                std::io::ErrorKind::InvalidData,
            ),
        ] {
            let directory = TestDirectory::new();
            let pid_file = directory.0.join("pid");
            let mut command = directory.script_command(
                "probe",
                &format!("#!/bin/sh\nprintf '%s' \"$$\" > \"$1\"\n{script}\n"),
            );
            command.push(pid_file.clone().into_os_string());
            let started = std::time::Instant::now();
            let result = super::ProcessProbeRunner.run_bounded(
                &command,
                &[],
                Duration::from_millis(
                    if expected == std::io::ErrorKind::TimedOut {
                        100
                    } else {
                        2_000
                    },
                ),
            );
            assert_eq!(result.err().unwrap().kind(), expected);
            assert!(started.elapsed() < Duration::from_secs(3));
            let pid = std::fs::read_to_string(pid_file).unwrap();
            assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
        }
    }

    #[test]
    fn fake_and_codex_share_phased_termination_conformance() {
        let directory = TestDirectory::new();
        let command = directory.script_command("controlled-worker", "#!/bin/sh\ntrap ':' INT TERM\nprintf '%s\\n' '{\"type\":\"thread.started\"}'\nwhile :; do :; done\n");
        let running = SessionObservation {
            lifecycle: LifecycleState::Running,
            activity: super::ActivityState::Busy,
            exit: None,
        };
        let mut fake = FakeProvider::new([FakeScript::new([
            super::fake::FakeEvent::observation(running),
        ])])
        .ignoring_controls(["interrupt", "terminate"]);
        let mut codex = CodexProvider::new(command);
        for (provider, is_fake) in [
            (&mut fake as &mut dyn Provider, true),
            (&mut codex as &mut dyn Provider, false),
        ] {
            let handle = provider
                .launch_job(
                    &specification_in(&directory.0),
                    &job_environment_in(&directory.0),
                )
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let event = provider.next_event(&handle).unwrap();
                let ready = if is_fake {
                    event
                        .as_ref()
                        .and_then(|event| event.observation())
                        .is_some_and(|state| {
                            state.lifecycle == LifecycleState::Running
                        })
                } else {
                    matches!(
                        event.as_ref().map(|event| &event.kind),
                        Some(ProviderEventKind::Output(_))
                    )
                };
                if ready {
                    break;
                }
                assert!(Instant::now() < deadline);
                thread::sleep(Duration::from_millis(5));
            }
            assert!(
                !provider.interrupt(&handle).unwrap().lifecycle.is_terminal()
            );
            assert!(
                !provider.terminate(&handle).unwrap().lifecycle.is_terminal()
            );
            provider.kill(&handle).unwrap();
            while !provider.observe(&handle).unwrap().lifecycle.is_terminal() {
                provider.next_event(&handle).unwrap();
                assert!(Instant::now() < deadline);
                thread::sleep(Duration::from_millis(5));
            }
            assert_eq!(
                provider.observe(&handle).unwrap().lifecycle,
                LifecycleState::Exited
            );
        }
    }

    #[test]
    fn codex_job_stream_preserves_jsonl_and_classifies_process_exit() {
        let directory = TestDirectory::new();
        let command = directory.script_command(
            "codex-ok",
            "#!/bin/sh\n\
             if [ \"${HOME+x}\" = x ]; then printf '%s\\n' '{\"type\":\"runtime.home\"}'; fi\n\
             printf '%s\\n' '{\"type\":\"thread.started\",\"thread_id\":\"thread-1\"}'\n\
             printf '%s\\n' '{\"type\":\"item.completed\",\"future_field\":true}'\n\
             printf '%s\\n' '{\"type\":\"turn.completed\",\"usage\":{}}'\n\
             exit 23\n",
        );
        let mut provider = CodexProvider::new(command);
        let specification = specification_in(&directory.0);
        let environment = job_environment_in(&directory.0);

        let handle = provider
            .launch_job(&specification, &environment)
            .expect("the Codex job should launch");
        let events = collect_job_events(&mut provider, &handle);

        assert_eq!(
            events[0].observation().map(|event| event.lifecycle),
            Some(LifecycleState::Running)
        );
        let output = events
            .iter()
            .filter_map(|event| match &event.kind {
                ProviderEventKind::Output(bytes) => Some(bytes.as_slice()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            output,
            [
                b"{\"type\":\"runtime.home\"}\n".as_slice(),
                b"{\"type\":\"thread.started\",\"thread_id\":\"thread-1\"}\n"
                    .as_slice(),
                b"{\"type\":\"item.completed\",\"future_field\":true}\n"
                    .as_slice(),
                b"{\"type\":\"turn.completed\",\"usage\":{}}\n".as_slice(),
            ]
        );
        let exit = events
            .last()
            .and_then(super::ProviderEvent::observation)
            .expect("the final event should classify process exit");
        assert_eq!(exit.lifecycle, LifecycleState::Exited);
        assert_eq!(exit.exit.and_then(|exit| exit.code), Some(23));
        assert_eq!(
            events
                .iter()
                .map(|event| event.sequence)
                .collect::<Vec<_>>(),
            (1..=events.len() as u64).collect::<Vec<_>>()
        );
    }

    #[test]
    fn codex_recovery_proves_only_that_a_process_is_absent() {
        let provider = CodexProvider::new(["codex"]);
        let scope = specification().scope;

        assert_eq!(
            provider
                .recover(&format!("process:{}", std::process::id()), scope)
                .expect("a live process should be observable conservatively"),
            ProviderRecovery::Unknown
        );
        assert_eq!(
            provider
                .recover("process:2147483647", scope)
                .expect("an absent process should be observable"),
            ProviderRecovery::Lost
        );
        assert_eq!(
            provider
                .recover("opaque-session", scope)
                .expect("an opaque identity should remain conservative"),
            ProviderRecovery::Unknown
        );
        assert_eq!(
            provider
                .recover("fake-session-42", scope)
                .expect("a legacy in-process fake cannot survive restart"),
            ProviderRecovery::Lost
        );
    }

    #[test]
    fn malformed_codex_jsonl_is_retained_and_quarantines_the_job() {
        let directory = TestDirectory::new();
        let command = directory.script_command(
            "codex-malformed",
            "#!/bin/sh\n\
             printf 'not-json\\n'\n\
             while :; do :; done\n",
        );
        let mut provider = CodexProvider::new(command);
        let specification = specification_in(&directory.0);
        let environment = job_environment_in(&directory.0);

        let handle = provider
            .launch_job(&specification, &environment)
            .expect("the Codex job should launch");
        let events = collect_job_events(&mut provider, &handle);
        let malformed = events
            .iter()
            .find_map(|event| match &event.kind {
                ProviderEventKind::MalformedOutput {
                    bytes,
                    diagnostic,
                    observation,
                } => Some((bytes, diagnostic, observation)),
                _ => None,
            })
            .expect("the malformed frame should be classified");

        assert_eq!(malformed.0, b"not-json\n");
        assert!(malformed.1.contains("invalid JSON"));
        assert_eq!(malformed.2.lifecycle, LifecycleState::Quarantined);
        assert_eq!(
            provider
                .observe(&handle)
                .expect("the job should remain known"),
            *malformed.2
        );
    }

    #[test]
    fn providers_preserve_final_fragments_without_inferred_success() {
        for (tail, malformed) in [
            ("{\"type\":\"turn.completed\"}", false),
            ("{\"type\":", true),
        ] {
            let directory = TestDirectory::new();
            let command = directory.script_command(
                "codex-tail",
                &format!("#!/bin/sh\nprintf '%s' '{tail}'\n"),
            );
            let script = if malformed {
                FakeScript::new([FakeEvent::malformed(
                    tail.as_bytes(),
                    "truncated fixture",
                )])
            } else {
                FakeScript::new([
                    FakeEvent::output(tail.as_bytes()),
                    FakeEvent::observation(SessionObservation::process_exit(
                        Some(0),
                    )),
                ])
            };
            let providers: Vec<Box<dyn Provider>> = vec![
                Box::new(FakeProvider::new([script])),
                Box::new(CodexProvider::new(command)),
            ];
            for mut provider in providers {
                let handle = provider
                    .launch_job(
                        &specification_in(&directory.0),
                        &job_environment_in(&directory.0),
                    )
                    .unwrap();
                let events = collect_job_events(provider.as_mut(), &handle);
                let bytes: Vec<u8> = events
                    .iter()
                    .flat_map(|event| match &event.kind {
                        ProviderEventKind::Output(bytes)
                        | ProviderEventKind::MalformedOutput {
                            bytes, ..
                        } => bytes.as_slice(),
                        _ => &[],
                    })
                    .copied()
                    .collect();
                assert_eq!(bytes, tail.as_bytes());
                assert_eq!(
                    events.last().unwrap().observation().unwrap().lifecycle,
                    if malformed {
                        LifecycleState::Quarantined
                    } else {
                        LifecycleState::Exited
                    }
                );
            }
        }
    }

    #[test]
    #[ignore = "requires an explicitly installed Codex CLI"]
    fn installed_codex_satisfies_the_probe_contract() {
        let probe = CodexProvider::new(["codex"])
            .probe()
            .expect("the installed Codex CLI should be probeable");

        assert_eq!(probe.compatibility, ProviderCompatibility::Compatible);
        for capability in [
            ProviderCapability::StartupInstructions,
            ProviderCapability::ForegroundInteractive,
            ProviderCapability::BackgroundJobs,
            ProviderCapability::StructuredLifecycleEvents,
            ProviderCapability::TranscriptStreaming,
            ProviderCapability::WorkingDirectory,
            ProviderCapability::FilesystemSandbox,
            ProviderCapability::NetworkSandbox,
            ProviderCapability::ApprovalPolicy,
            ProviderCapability::Interrupt,
            ProviderCapability::Termination,
        ] {
            assert!(
                probe.capabilities.contains(&capability),
                "installed Codex {} does not advertise {capability}",
                probe.version
            );
        }
    }

    #[test]
    fn fake_provider_replays_a_session_script_deterministically() {
        let running = SessionObservation {
            lifecycle: LifecycleState::Running,
            activity: ActivityState::Busy,
            exit: None,
        };
        let idle = SessionObservation {
            lifecycle: LifecycleState::Running,
            activity: ActivityState::Idle,
            exit: None,
        };
        let exited = SessionObservation::exited(0);
        let script = FakeScript::new([
            FakeEvent::observation(running),
            FakeEvent::output(b"{\"type\":\"turn.started\"}\n"),
            FakeEvent::observation(idle),
            FakeEvent::observation(exited),
        ]);
        let mut provider = FakeProvider::new([script]);

        let handle = provider
            .launch_job(&specification(), &job_environment())
            .expect("the scripted session should launch");
        assert_eq!(handle.provider_id(), "fake-session-1");
        assert_eq!(
            provider.observe(&handle).expect("the session should exist"),
            SessionObservation::starting()
        );

        let expected = [
            ProviderEventKind::Observation(running),
            ProviderEventKind::Output(
                b"{\"type\":\"turn.started\"}\n".to_vec(),
            ),
            ProviderEventKind::Observation(idle),
            ProviderEventKind::Observation(exited),
        ];
        for (index, expected_kind) in expected.into_iter().enumerate() {
            let event = provider
                .next_event(&handle)
                .expect("the scripted event should be readable")
                .expect("the script should have another event");
            assert_eq!(event.sequence, index as u64 + 1);
            assert_eq!(event.kind, expected_kind);
        }

        assert_eq!(
            provider.observe(&handle).expect("the session should exist"),
            exited
        );
        assert_eq!(
            provider
                .next_event(&handle)
                .expect("the completed script should remain readable"),
            None
        );
    }

    #[test]
    fn fake_provider_uses_launch_order_instead_of_time_or_process_state() {
        let mut provider =
            FakeProvider::new([FakeScript::new([]), FakeScript::new([])]);

        let first = provider
            .launch_interactive(&specification(), None)
            .expect("the first session should launch");
        let second = provider
            .launch_job(&specification(), &job_environment())
            .expect("the second session should launch");

        assert_eq!(first.provider_id(), "fake-session-1");
        assert_eq!(second.provider_id(), "fake-session-2");
        assert_eq!(provider.launches()[0].mode, LaunchMode::Interactive);
        assert_eq!(provider.launches()[1].mode, LaunchMode::Job);
        assert_eq!(provider.launches()[0].specification, specification());
    }

    #[test]
    fn lifecycle_values_have_stable_names_and_terminal_rules() {
        let values = [
            ("starting", LifecycleState::Starting),
            ("running", LifecycleState::Running),
            ("exited", LifecycleState::Exited),
            ("lost", LifecycleState::Lost),
            ("unknown", LifecycleState::Unknown),
            ("quarantined", LifecycleState::Quarantined),
        ];

        for (encoded, state) in values {
            assert_eq!(state.to_string(), encoded);
            assert_eq!(encoded.parse::<LifecycleState>(), Ok(state));
        }
        assert!(LifecycleState::Starting.allows(LifecycleState::Running));
        assert!(LifecycleState::Running.allows(LifecycleState::Lost));
        assert!(LifecycleState::Running.allows(LifecycleState::Quarantined));
        assert!(!LifecycleState::Exited.allows(LifecycleState::Running));
    }

    fn specification() -> LaunchSpecification {
        LaunchSpecification {
            scope: SessionScope {
                run_id: RUN_ID.parse::<RunId>().expect("valid run ID"),
                agent_id: AGENT_ID.parse::<AgentId>().expect("valid agent ID"),
                session_id: SESSION_ID
                    .parse::<SessionId>()
                    .expect("valid session ID"),
                generation: 2,
            },
            working_directory: PathBuf::from("/tmp/project"),
            permission_profile: permission_profile("worker"),
            bootstrap_instruction: "Run `coterie prime`.".to_owned(),
        }
    }

    fn permission_profile(name: &str) -> PermissionProfile {
        *crate::config::builtin_standard()
            .permission_profiles
            .get(name)
            .expect("the built-in permission profile should exist")
    }

    fn job_environment() -> JobEnvironment {
        JobEnvironment {
            project_id: PROJECT_ID
                .parse::<ProjectId>()
                .expect("valid project ID"),
            primary_project_root: PathBuf::from("/tmp/project"),
            role: "worker".to_owned(),
            task_id: TASK_ID.parse::<TaskId>().expect("valid task ID"),
            socket_path: PathBuf::from("/tmp/coterie.sock"),
            token: AgentToken::generate()
                .expect("token generation should succeed"),
        }
    }

    fn specification_in(directory: &std::path::Path) -> LaunchSpecification {
        LaunchSpecification {
            working_directory: directory.to_path_buf(),
            ..specification()
        }
    }

    fn job_environment_in(directory: &std::path::Path) -> JobEnvironment {
        JobEnvironment {
            primary_project_root: directory.to_path_buf(),
            socket_path: directory.join("coterie.sock"),
            ..job_environment()
        }
    }

    fn collect_job_events(
        provider: &mut dyn Provider,
        handle: &super::ProviderSessionHandle,
    ) -> Vec<super::ProviderEvent> {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut events = Vec::new();
        loop {
            if let Some(event) = provider
                .next_event(handle)
                .expect("the job event should be readable")
            {
                let terminal = event.observation().is_some_and(|observation| {
                    observation.lifecycle.is_terminal()
                });
                events.push(event);
                if terminal {
                    return events;
                }
            } else {
                assert!(
                    Instant::now() < deadline,
                    "Codex event stream timed out"
                );
                thread::sleep(Duration::from_millis(10));
            }
        }
    }

    #[test]
    fn codex_worker_bootstrap_preserves_shell_identity_and_cli_location() {
        let command = CodexProvider::new(["codex"])
            .job_command(&specification(), &job_environment())
            .unwrap();
        let variables = command.get_envs().collect::<BTreeMap<_, _>>();
        assert_eq!(
            variables.get(OsStr::new("COTERIE_BIN")),
            Some(&Some(std::env::current_exe().unwrap().as_os_str()))
        );
        let arguments = command.get_args().collect::<Vec<_>>();
        assert!(arguments.contains(&OsStr::new("allow_login_shell=false")));
        let bootstrap = arguments
            .iter()
            .find_map(|argument| {
                argument.to_str()?.strip_prefix("developer_instructions=")
            })
            .unwrap();
        let bootstrap: String = serde_json::from_str(bootstrap).unwrap();
        assert!(bootstrap.contains("Selected permission profile: {\"filesystem\":\"workspace-write\",\"network\":\"deny\",\"approvals\":\"never\"}"));
        assert!(bootstrap.contains("missing or inaccessible"));
        assert!(bootstrap.contains("do not bypass the sandbox"));
        assert!(bootstrap.contains("Never print tokens"));

        let mut filtered = Command::new("unused");
        filtered.env_clear();
        super::apply_codex_runtime_environment(
            &mut filtered,
            [
                "USER",
                "LOGNAME",
                "SHELL",
                "BASH_ENV",
                "NIX_SECRET",
                "COTERIE_BIN",
            ]
            .map(|name| (name.into(), "sentinel".into())),
        );
        let names = filtered
            .get_envs()
            .map(|(name, _)| name)
            .collect::<Vec<_>>();
        assert_eq!(names, ["LOGNAME", "SHELL", "USER"]);
    }

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "coterie-provider-test-{}",
                crate::id::RunId::generate()
            ));
            fs::create_dir(&path).expect("the test directory should be unique");
            Self(path)
        }

        fn script_command(&self, name: &str, contents: &str) -> Vec<OsString> {
            let path = self.0.join(name);
            fs::write(&path, contents).expect("the fixture should be writable");
            vec![OsString::from("sh"), path.into_os_string()]
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0)
                .expect("the test directory should be removable");
        }
    }

    #[derive(Clone)]
    struct ScriptedProbeRunner {
        outputs: Rc<RefCell<VecDeque<Result<ProbeOutput, io::Error>>>>,
        observations: Rc<RefCell<Vec<Vec<OsString>>>>,
    }

    impl ScriptedProbeRunner {
        fn new(
            outputs: impl IntoIterator<Item = Result<ProbeOutput, io::Error>>,
        ) -> Self {
            Self {
                outputs: Rc::new(RefCell::new(outputs.into_iter().collect())),
                observations: Rc::new(RefCell::new(Vec::new())),
            }
        }

        fn observations(&self) -> Rc<RefCell<Vec<Vec<OsString>>>> {
            Rc::clone(&self.observations)
        }
    }

    impl ProbeCommandRunner for ScriptedProbeRunner {
        fn run(
            &self,
            command: &[OsString],
            arguments: &[&str],
        ) -> Result<ProbeOutput, io::Error> {
            self.observations.borrow_mut().push(
                command
                    .iter()
                    .cloned()
                    .chain(arguments.iter().map(OsString::from))
                    .collect(),
            );
            self.outputs
                .borrow_mut()
                .pop_front()
                .expect("the probe made an unexpected command call")
        }
    }

    fn success(stdout: &str) -> ProbeOutput {
        ProbeOutput {
            success: true,
            code: Some(0),
            stdout: stdout.as_bytes().to_vec(),
            stderr: Vec::new(),
        }
    }

    fn failure(code: i32, stderr: &str) -> ProbeOutput {
        ProbeOutput {
            success: false,
            code: Some(code),
            stdout: Vec::new(),
            stderr: stderr.as_bytes().to_vec(),
        }
    }
}

//! Out-of-process agent harness adapters.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fmt;
use std::io;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::str::FromStr;

use semver::Version;
use thiserror::Error;

use crate::auth::SessionScope;

/// A provider feature that Coterie must verify before depending on it.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum ProviderCapability {
    StartupInstructions,
    ForegroundInteractive,
    BackgroundJobs,
    StructuredLifecycleEvents,
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
    pub(crate) bootstrap_instruction: String,
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
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
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
        Self {
            lifecycle: LifecycleState::Exited,
            activity: ActivityState::Unknown,
            exit: Some(SessionExit {
                code: Some(code),
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
        }
    }
}

/// Provider data retained before later normalization into Coterie events.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ProviderEventKind {
    Observation(SessionObservation),
    Output(Vec<u8>),
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
    fn probe(&self) -> Result<ProviderProbe, ProviderError>;

    fn launch_interactive(
        &mut self,
        specification: &LaunchSpecification,
    ) -> Result<ProviderSessionHandle, ProviderError>;

    fn launch_job(
        &mut self,
        specification: &LaunchSpecification,
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
}

const MINIMUM_CODEX_VERSION: &str = "0.151.0";
const CODEX_VERSION_REQUIREMENT: &str = ">=0.151.0 and <1.0.0";

/// The installed Codex CLI, invoked only through its documented process boundary.
pub(crate) struct CodexProvider {
    command: Vec<OsString>,
    probe_runner: Box<dyn ProbeCommandRunner>,
}

impl CodexProvider {
    pub(crate) fn new(
        command: impl IntoIterator<Item = impl Into<OsString>>,
    ) -> Self {
        Self {
            command: command.into_iter().map(Into::into).collect(),
            probe_runner: Box::new(ProcessProbeRunner),
        }
    }

    #[cfg(test)]
    fn with_runner(
        command: impl IntoIterator<Item = impl Into<OsString>>,
        runner: impl ProbeCommandRunner + 'static,
    ) -> Self {
        Self {
            command: command.into_iter().map(Into::into).collect(),
            probe_runner: Box::new(runner),
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

impl Provider for CodexProvider {
    fn probe(&self) -> Result<ProviderProbe, ProviderError> {
        self.probe_codex()
    }

    fn launch_interactive(
        &mut self,
        _specification: &LaunchSpecification,
    ) -> Result<ProviderSessionHandle, ProviderError> {
        Err(ProviderError::UnsupportedCodexOperation {
            operation: "interactive launch",
        })
    }

    fn launch_job(
        &mut self,
        _specification: &LaunchSpecification,
    ) -> Result<ProviderSessionHandle, ProviderError> {
        Err(ProviderError::UnsupportedCodexOperation {
            operation: "job launch",
        })
    }

    fn recover(
        &self,
        _provider_session_id: &str,
        _scope: SessionScope,
    ) -> Result<ProviderRecovery, ProviderError> {
        Err(ProviderError::UnsupportedCodexOperation {
            operation: "session recovery",
        })
    }

    fn observe(
        &self,
        _session: &ProviderSessionHandle,
    ) -> Result<SessionObservation, ProviderError> {
        Err(ProviderError::UnsupportedCodexOperation {
            operation: "session observation",
        })
    }

    fn next_event(
        &mut self,
        _session: &ProviderSessionHandle,
    ) -> Result<Option<ProviderEvent>, ProviderError> {
        Err(ProviderError::UnsupportedCodexOperation {
            operation: "event streaming",
        })
    }

    fn interrupt(
        &mut self,
        _session: &ProviderSessionHandle,
    ) -> Result<SessionObservation, ProviderError> {
        Err(ProviderError::UnsupportedCodexOperation {
            operation: "interrupt",
        })
    }

    fn terminate(
        &mut self,
        _session: &ProviderSessionHandle,
    ) -> Result<SessionObservation, ProviderError> {
        Err(ProviderError::UnsupportedCodexOperation {
            operation: "termination",
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
        let (program, configured_arguments) =
            command.split_first().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "empty provider command",
                )
            })?;
        let output = Command::new(program)
            .args(configured_arguments)
            .args(arguments)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()?;
        Ok(ProbeOutput {
            success: output.status.success(),
            code: output.status.code(),
            stdout: output.stdout,
            stderr: output.stderr,
        })
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
    let interactive = interactive_help.contains("Usage: codex ")
        && interactive_help.contains("--cd")
        && interactive_help.contains("--config");
    let job = job_help.contains("Usage: codex exec ")
        && job_help.contains("--cd")
        && job_help.contains("--config");
    let structured_job = job && job_help.contains("--json");
    let mut capabilities = BTreeSet::new();
    if interactive {
        capabilities.insert(ProviderCapability::ForegroundInteractive);
    }
    if job {
        capabilities.insert(ProviderCapability::BackgroundJobs);
    }
    if interactive && job {
        capabilities.insert(ProviderCapability::StartupInstructions);
    }
    if structured_job {
        capabilities.insert(ProviderCapability::StructuredLifecycleEvents);
        capabilities.insert(ProviderCapability::TranscriptStreaming);
    }
    capabilities
}

fn diagnostic_output(primary: &[u8], fallback: &[u8]) -> String {
    let bytes = if primary.is_empty() {
        fallback
    } else {
        primary
    };
    let output = String::from_utf8_lossy(bytes);
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
        capabilities: BTreeSet<ProviderCapability>,
        compatibility: ProviderCompatibility,
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
                capabilities: BTreeSet::from([
                    ProviderCapability::StartupInstructions,
                    ProviderCapability::ForegroundInteractive,
                    ProviderCapability::BackgroundJobs,
                    ProviderCapability::StructuredLifecycleEvents,
                    ProviderCapability::Interrupt,
                    ProviderCapability::Termination,
                    ProviderCapability::TranscriptStreaming,
                ]),
                compatibility: ProviderCompatibility::Compatible,
            }
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
        ) -> Result<ProviderSessionHandle, ProviderError> {
            self.launch(LaunchMode::Interactive, specification)
        }

        fn launch_job(
            &mut self,
            specification: &LaunchSpecification,
        ) -> Result<ProviderSessionHandle, ProviderError> {
            self.launch(LaunchMode::Job, specification)
        }

        fn recover(
            &self,
            provider_session_id: &str,
            scope: SessionScope,
        ) -> Result<ProviderRecovery, ProviderError> {
            let Some(session) = self.sessions.get(provider_session_id) else {
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
            if let ProviderEventKind::Observation(observation) = event.0 {
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
            self.stop(session, SessionObservation::interrupted())
        }

        fn terminate(
            &mut self,
            session: &ProviderSessionHandle,
        ) -> Result<SessionObservation, ProviderError> {
            self.stop(session, SessionObservation::terminated())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::{BTreeSet, VecDeque};
    use std::ffi::OsString;
    use std::io;
    use std::path::PathBuf;
    use std::rc::Rc;

    use super::fake::{FakeEvent, FakeProvider, FakeScript};
    use super::{
        ActivityState, CodexProvider, LaunchMode, LaunchSpecification,
        LifecycleState, ProbeCommandRunner, ProbeOutput, Provider,
        ProviderCapability, ProviderCompatibility, ProviderError,
        ProviderEventKind, SessionObservation,
    };
    use crate::auth::SessionScope;
    use crate::id::{AgentId, RunId, SessionId};

    const RUN_ID: &str = "cr-01ARZ3NDEKTSV4RRFFQ69G5FAV";
    const AGENT_ID: &str = "cg-01ARZ3NDEKTSV4RRFFQ69G5FAX";
    const SESSION_ID: &str = "cs-01ARZ3NDEKTSV4RRFFQ69G5FAY";

    #[test]
    fn codex_probe_discovers_the_supported_command_surface() {
        let runner = ScriptedProbeRunner::new([
            Ok(success("codex-cli 0.151.0\n")),
            Ok(success(
                "Usage: codex [OPTIONS] [PROMPT]\n  --config <key=value>\n  --cd <DIR>\n",
            )),
            Ok(success(
                "Usage: codex exec [OPTIONS] [PROMPT]\n  --config <key=value>\n  --cd <DIR>\n  --json\n",
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
            .launch_job(&specification())
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
            .launch_interactive(&specification())
            .expect("the first session should launch");
        let second = provider
            .launch_job(&specification())
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
            bootstrap_instruction: "Run `coterie prime`.".to_owned(),
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

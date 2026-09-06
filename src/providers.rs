//! Out-of-process agent harness adapters.

use std::collections::BTreeSet;
use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

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
    pub(crate) version: String,
    pub(crate) capabilities: BTreeSet<ProviderCapability>,
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
            || matches!(self, Self::Starting | Self::Running)
                && matches!(
                    next,
                    Self::Running
                        | Self::Exited
                        | Self::Lost
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

/// The process boundary used by the session supervisor.
pub(crate) trait Provider {
    fn probe(&self) -> ProviderProbe;

    fn launch_interactive(
        &mut self,
        specification: &LaunchSpecification,
    ) -> Result<ProviderSessionHandle, ProviderError>;

    fn launch_job(
        &mut self,
        specification: &LaunchSpecification,
    ) -> Result<ProviderSessionHandle, ProviderError>;

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
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub(crate) enum ProviderError {
    #[error("the fake provider has no launch script remaining")]
    NoLaunchScript,
    #[error("provider session `{provider_id}` does not exist")]
    UnknownSession { provider_id: String },
}

pub(crate) mod fake {
    use std::collections::{BTreeMap, BTreeSet, VecDeque};

    use super::{
        LaunchMode, LaunchSpecification, Provider, ProviderCapability,
        ProviderError, ProviderEvent, ProviderEventKind, ProviderProbe,
        ProviderSessionHandle, SessionObservation,
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
            }
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
        fn probe(&self) -> ProviderProbe {
            ProviderProbe {
                name: "fake".to_owned(),
                version: "1".to_owned(),
                capabilities: BTreeSet::from([
                    ProviderCapability::StartupInstructions,
                    ProviderCapability::ForegroundInteractive,
                    ProviderCapability::BackgroundJobs,
                    ProviderCapability::StructuredLifecycleEvents,
                    ProviderCapability::Interrupt,
                    ProviderCapability::Termination,
                    ProviderCapability::TranscriptStreaming,
                ]),
            }
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
    use std::path::PathBuf;

    use super::fake::{FakeEvent, FakeProvider, FakeScript};
    use super::{
        ActivityState, LaunchMode, LaunchSpecification, LifecycleState,
        Provider, ProviderEventKind, SessionObservation,
    };
    use crate::auth::SessionScope;
    use crate::id::{AgentId, RunId, SessionId};

    const RUN_ID: &str = "cr-01ARZ3NDEKTSV4RRFFQ69G5FAV";
    const AGENT_ID: &str = "cg-01ARZ3NDEKTSV4RRFFQ69G5FAX";
    const SESSION_ID: &str = "cs-01ARZ3NDEKTSV4RRFFQ69G5FAY";

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
}

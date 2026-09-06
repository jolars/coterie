//! Provider-driven agent and session lifecycle supervision.

use std::collections::BTreeMap;
use std::path::PathBuf;

use thiserror::Error;

use crate::auth::{AgentToken, SessionScope, TokenGenerationError};
use crate::id::SessionId;
use crate::providers::{
    LaunchMode, LaunchSpecification, LifecycleState, Provider,
    ProviderCapability, ProviderError, ProviderEvent, ProviderEventKind,
    ProviderSessionHandle, SessionObservation,
};
use crate::state::{
    AgentRecord, SessionCredentialRecord, SessionRecord, Store, StoreError,
};
use crate::transcript::{TranscriptError, TranscriptStore};

/// Durable and provider-specific input for launching one new agent.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AgentLaunch {
    pub(crate) scope: SessionScope,
    pub(crate) role: String,
    pub(crate) mode: LaunchMode,
    pub(crate) working_directory: PathBuf,
    pub(crate) bootstrap_instruction: String,
    pub(crate) created_at: i64,
}

/// The secret material returned only to the newly launched provider process.
pub(crate) struct LaunchedAgent {
    pub(crate) scope: SessionScope,
    pub(crate) token: AgentToken,
}

/// Drives provider events into durable agent state and append-only transcripts.
pub(crate) struct AgentSessionSupervisor<P> {
    provider: P,
    sessions: BTreeMap<SessionId, ProviderSessionHandle>,
    transcripts: TranscriptStore,
}

impl<P: Provider> AgentSessionSupervisor<P> {
    pub(crate) fn new(
        provider: P,
        run_state_directory: impl Into<PathBuf>,
    ) -> Self {
        Self {
            provider,
            sessions: BTreeMap::new(),
            transcripts: TranscriptStore::new(run_state_directory),
        }
    }

    /// Persists the starting generation before crossing the provider boundary.
    pub(crate) fn launch(
        &mut self,
        store: &mut Store,
        launch: &AgentLaunch,
    ) -> Result<LaunchedAgent, AgentSessionError> {
        let probe = self.provider.probe();
        for capability in required_capabilities(launch) {
            if !probe.capabilities.contains(&capability) {
                return Err(AgentSessionError::MissingCapability {
                    provider: probe.name,
                    capability,
                });
            }
        }

        let token = AgentToken::generate()?;
        let transcript_path =
            TranscriptStore::relative_path(launch.scope.session_id);
        store.transaction(|repositories| {
            repositories.insert_agent(&AgentRecord {
                id: launch.scope.agent_id,
                run_id: launch.scope.run_id,
                role: launch.role.clone(),
                generation: launch.scope.generation,
                state: LifecycleState::Starting,
                created_at: launch.created_at,
            })?;
            repositories.insert_session(&SessionRecord {
                id: launch.scope.session_id,
                run_id: launch.scope.run_id,
                agent_id: launch.scope.agent_id,
                generation: launch.scope.generation,
                provider: probe.name.clone(),
                state: LifecycleState::Starting,
                transcript_path,
                created_at: launch.created_at,
                ended_at: None,
            })?;
            repositories.activate_session_credential(&SessionCredentialRecord {
                session_id: launch.scope.session_id,
                run_id: launch.scope.run_id,
                agent_id: launch.scope.agent_id,
                generation: launch.scope.generation,
                token_verifier: token.verifier(launch.scope),
                created_at: launch.created_at,
                revoked_at: None,
            })
        })?;

        let specification = LaunchSpecification {
            scope: launch.scope,
            working_directory: launch.working_directory.clone(),
            bootstrap_instruction: launch.bootstrap_instruction.clone(),
        };
        let handle = match launch.mode {
            LaunchMode::Interactive => {
                self.provider.launch_interactive(&specification)?
            }
            LaunchMode::Job => self.provider.launch_job(&specification)?,
        };
        if handle.scope != launch.scope {
            return Err(AgentSessionError::ScopeMismatch {
                expected: Box::new(launch.scope),
                observed: Box::new(handle.scope),
            });
        }
        self.sessions.insert(launch.scope.session_id, handle);
        Ok(LaunchedAgent {
            scope: launch.scope,
            token,
        })
    }

    /// Applies exactly one provider event, preserving its deterministic order.
    pub(crate) fn advance(
        &mut self,
        store: &mut Store,
        session_id: SessionId,
        observed_at: i64,
    ) -> Result<Option<ProviderEvent>, AgentSessionError> {
        let handle = self.handle(session_id)?.clone();
        let Some(event) = self.provider.next_event(&handle)? else {
            return Ok(None);
        };
        match &event.kind {
            ProviderEventKind::Observation(observation) => {
                store.transaction(|repositories| {
                    repositories.record_session_lifecycle(
                        handle.scope,
                        observation.lifecycle,
                        observed_at,
                    )?;
                    Ok(())
                })?;
            }
            ProviderEventKind::Output(bytes) => {
                self.transcripts.append(session_id, bytes)?;
            }
        }
        Ok(Some(event))
    }

    pub(crate) fn observe(
        &self,
        session_id: SessionId,
    ) -> Result<SessionObservation, AgentSessionError> {
        Ok(self.provider.observe(self.handle(session_id)?)?)
    }

    pub(crate) fn interrupt(
        &mut self,
        store: &mut Store,
        session_id: SessionId,
        observed_at: i64,
    ) -> Result<SessionObservation, AgentSessionError> {
        let handle = self.handle(session_id)?.clone();
        let observation = self.provider.interrupt(&handle)?;
        self.record_observation(store, &handle, observation, observed_at)?;
        Ok(observation)
    }

    pub(crate) fn terminate(
        &mut self,
        store: &mut Store,
        session_id: SessionId,
        observed_at: i64,
    ) -> Result<SessionObservation, AgentSessionError> {
        let handle = self.handle(session_id)?.clone();
        let observation = self.provider.terminate(&handle)?;
        self.record_observation(store, &handle, observation, observed_at)?;
        Ok(observation)
    }

    fn record_observation(
        &self,
        store: &mut Store,
        handle: &ProviderSessionHandle,
        observation: SessionObservation,
        observed_at: i64,
    ) -> Result<(), AgentSessionError> {
        store.transaction(|repositories| {
            repositories.record_session_lifecycle(
                handle.scope,
                observation.lifecycle,
                observed_at,
            )?;
            Ok(())
        })?;
        Ok(())
    }

    fn handle(
        &self,
        session_id: SessionId,
    ) -> Result<&ProviderSessionHandle, AgentSessionError> {
        self.sessions
            .get(&session_id)
            .ok_or(AgentSessionError::UnknownSession { session_id })
    }
}

fn required_capabilities(
    launch: &AgentLaunch,
) -> impl Iterator<Item = ProviderCapability> {
    let mode = match launch.mode {
        LaunchMode::Interactive => ProviderCapability::ForegroundInteractive,
        LaunchMode::Job => ProviderCapability::BackgroundJobs,
    };
    let startup = (!launch.bootstrap_instruction.is_empty())
        .then_some(ProviderCapability::StartupInstructions);
    [
        Some(mode),
        startup,
        Some(ProviderCapability::StructuredLifecycleEvents),
        Some(ProviderCapability::TranscriptStreaming),
    ]
    .into_iter()
    .flatten()
}

/// A provider event could not be applied to its durable session generation.
#[derive(Debug, Error)]
pub(crate) enum AgentSessionError {
    #[error(transparent)]
    State(#[from] StoreError),
    #[error(transparent)]
    Provider(#[from] ProviderError),
    #[error(transparent)]
    Transcript(#[from] TranscriptError),
    #[error(transparent)]
    Token(#[from] TokenGenerationError),
    #[error("provider `{provider}` does not support {capability}")]
    MissingCapability {
        provider: String,
        capability: ProviderCapability,
    },
    #[error("provider session scope is {observed:?}, expected {expected:?}")]
    ScopeMismatch {
        expected: Box<SessionScope>,
        observed: Box<SessionScope>,
    },
    #[error("session `{session_id}` is not managed by this supervisor")]
    UnknownSession { session_id: SessionId },
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::{AgentLaunch, AgentSessionSupervisor};
    use crate::auth::SessionScope;
    use crate::id::{AgentId, RunId, SessionId};
    use crate::providers::fake::{FakeEvent, FakeProvider, FakeScript};
    use crate::providers::{
        ActivityState, LaunchMode, LifecycleState, SessionObservation,
    };
    use crate::state::{RunRecord, Store};

    const RUN_ID: &str = "cr-01ARZ3NDEKTSV4RRFFQ69G5FAV";
    const AGENT_ID: &str = "cg-01ARZ3NDEKTSV4RRFFQ69G5FAX";
    const SESSION_ID: &str = "cs-01ARZ3NDEKTSV4RRFFQ69G5FAY";

    #[test]
    fn fake_provider_drives_durable_agent_and_session_lifecycles() {
        let directory = TestDirectory::new();
        let mut store = Store::open(&directory.0.join("state.sqlite3"))
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
            .expect("the run should commit");
        let running = SessionObservation {
            lifecycle: LifecycleState::Running,
            activity: ActivityState::Busy,
            exit: None,
        };
        let provider = FakeProvider::new([FakeScript::new([
            FakeEvent::observation(running),
            FakeEvent::output(b"{\"type\":\"item.completed\"}\n"),
            FakeEvent::observation(SessionObservation::exited(0)),
        ])]);
        let mut supervisor =
            AgentSessionSupervisor::new(provider, &directory.0);
        let launch = launch(run_id);

        let launched = supervisor
            .launch(&mut store, &launch)
            .expect("the fake agent should launch");
        assert_eq!(launched.scope, launch.scope);
        store
            .transaction(|repositories| {
                let agent = repositories
                    .agent(launch.scope.agent_id)?
                    .expect("the agent intent should be durable");
                let session = repositories
                    .session(launch.scope.session_id)?
                    .expect("the session intent should be durable");
                let credential = repositories
                    .active_session_credential(
                        launch.scope.run_id,
                        launch.scope.agent_id,
                        launch.scope.session_id,
                    )?
                    .expect("the launched session should authenticate");
                assert_eq!(agent.state, LifecycleState::Starting);
                assert_eq!(session.state, LifecycleState::Starting);
                assert!(
                    credential
                        .token_verifier
                        .verify(&launched.token, launched.scope)
                );
                Ok(())
            })
            .expect("the launch state should be readable");

        let started = supervisor
            .advance(&mut store, launch.scope.session_id, 11)
            .expect("the running event should apply")
            .expect("the running event should exist");
        assert_eq!(started.observation(), Some(running));
        assert_eq!(
            supervisor
                .observe(launch.scope.session_id)
                .expect("the fake session should be observable"),
            running
        );

        supervisor
            .advance(&mut store, launch.scope.session_id, 12)
            .expect("the output event should append")
            .expect("the output event should exist");
        let transcript = directory.0.join(
            crate::transcript::TranscriptStore::relative_path(
                launch.scope.session_id,
            ),
        );
        assert_eq!(
            fs::read(transcript).expect("the transcript should be readable"),
            b"{\"type\":\"item.completed\"}\n"
        );

        supervisor
            .advance(&mut store, launch.scope.session_id, 13)
            .expect("the exit event should apply")
            .expect("the exit event should exist");
        store
            .transaction(|repositories| {
                let agent = repositories
                    .agent(launch.scope.agent_id)?
                    .expect("the agent should remain durable");
                let session = repositories
                    .session(launch.scope.session_id)?
                    .expect("the session should remain durable");
                assert_eq!(agent.state, LifecycleState::Exited);
                assert_eq!(session.state, LifecycleState::Exited);
                assert_eq!(session.ended_at, Some(13));
                assert_eq!(
                    repositories.active_session_credential(
                        launch.scope.run_id,
                        launch.scope.agent_id,
                        launch.scope.session_id,
                    )?,
                    None
                );
                Ok(())
            })
            .expect("the terminal state should be readable");
    }

    #[test]
    fn lifecycle_control_is_explicit_and_idempotent() {
        let directory = TestDirectory::new();
        let mut store = Store::open(&directory.0.join("state.sqlite3"))
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
            .expect("the run should commit");
        let provider = FakeProvider::new([FakeScript::new([])]);
        let mut supervisor =
            AgentSessionSupervisor::new(provider, &directory.0);
        let launch = launch(run_id);
        supervisor
            .launch(&mut store, &launch)
            .expect("the fake agent should launch");

        let interrupted = supervisor
            .interrupt(&mut store, launch.scope.session_id, 11)
            .expect("the session should be interrupted");
        let repeated = supervisor
            .terminate(&mut store, launch.scope.session_id, 12)
            .expect("terminating a stopped session should be idempotent");

        assert_eq!(interrupted.lifecycle, LifecycleState::Exited);
        assert_eq!(repeated, interrupted);
        store
            .transaction(|repositories| {
                assert_eq!(
                    repositories
                        .session(launch.scope.session_id)?
                        .expect("the interrupted session should remain durable")
                        .ended_at,
                    Some(11)
                );
                Ok(())
            })
            .expect("the first terminal observation should win");
    }

    #[test]
    fn uncertain_terminal_states_propagate_without_inferred_success() {
        for terminal in [LifecycleState::Lost, LifecycleState::Quarantined] {
            let directory = TestDirectory::new();
            let mut store = Store::open(&directory.0.join("state.sqlite3"))
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
                .expect("the run should commit");
            let provider =
                FakeProvider::new([FakeScript::new([FakeEvent::observation(
                    SessionObservation {
                        lifecycle: terminal,
                        activity: ActivityState::Unknown,
                        exit: None,
                    },
                )])]);
            let mut supervisor =
                AgentSessionSupervisor::new(provider, &directory.0);
            let launch = launch(run_id);
            supervisor
                .launch(&mut store, &launch)
                .expect("the fake agent should launch");

            supervisor
                .advance(&mut store, launch.scope.session_id, 11)
                .expect("the terminal observation should apply")
                .expect("the terminal event should exist");

            store
                .transaction(|repositories| {
                    assert_eq!(
                        repositories
                            .agent(launch.scope.agent_id)?
                            .expect("the agent should remain durable")
                            .state,
                        terminal
                    );
                    assert_eq!(
                        repositories
                            .session(launch.scope.session_id)?
                            .expect("the session should remain durable")
                            .state,
                        terminal
                    );
                    assert_eq!(
                        repositories.active_session_credential(
                            launch.scope.run_id,
                            launch.scope.agent_id,
                            launch.scope.session_id,
                        )?,
                        None
                    );
                    Ok(())
                })
                .expect("uncertain state should remain explicit");
        }
    }

    fn launch(run_id: RunId) -> AgentLaunch {
        AgentLaunch {
            scope: SessionScope {
                run_id,
                agent_id: AGENT_ID.parse::<AgentId>().expect("valid agent ID"),
                session_id: SESSION_ID
                    .parse::<SessionId>()
                    .expect("valid session ID"),
                generation: 0,
            },
            role: "worker".to_owned(),
            mode: LaunchMode::Job,
            working_directory: PathBuf::from("/tmp/project"),
            bootstrap_instruction: "Run `coterie prime`.".to_owned(),
            created_at: 10,
        }
    }

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let path = std::env::temp_dir()
                .join(format!("coterie-session-test-{}", RunId::generate()));
            fs::create_dir(&path).expect("the test directory should be unique");
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0)
                .expect("the test directory should be removable");
        }
    }
}

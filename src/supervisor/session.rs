//! Provider-driven agent and session lifecycle supervision.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::json;
use thiserror::Error;

use crate::auth::{AgentToken, SessionScope, TokenGenerationError};
use crate::config::{NetworkPolicy, PermissionProfile};
use crate::id::{AssignmentId, ProjectId, SessionId, TaskId};
use crate::providers::{
    JobEnvironment, LaunchMode, LaunchSpecification, LifecycleState, Provider,
    ProviderCapability, ProviderError, ProviderEvent, ProviderEventKind,
    ProviderRecovery, ProviderSessionHandle, SessionObservation,
};
use crate::state::{
    AgentRecord, EventKind, ExternalResourceState, NewEvent,
    SessionCredentialRecord, SessionProcessOwner, SessionRecord,
    SessionTransitionOutcome, Store, StoreError,
};
use crate::transcript::{TranscriptError, TranscriptStore};

/// Durable and provider-specific input for launching one new agent.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AgentLaunch {
    pub(crate) scope: SessionScope,
    pub(crate) role: String,
    pub(crate) mode: LaunchMode,
    pub(crate) working_directory: PathBuf,
    pub(crate) project_id: ProjectId,
    pub(crate) primary_project_root: PathBuf,
    pub(crate) task_id: TaskId,
    pub(crate) socket_path: PathBuf,
    pub(crate) permission_profile: PermissionProfile,
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
    transcript_secrets: BTreeMap<SessionId, Vec<u8>>,
    transcripts: TranscriptStore,
    #[cfg(test)]
    credential_observer: Option<std::sync::mpsc::Sender<LaunchedAgent>>,
}

impl<P: Provider> AgentSessionSupervisor<P> {
    pub(crate) fn new(
        provider: P,
        run_state_directory: impl Into<PathBuf>,
    ) -> Self {
        Self {
            provider,
            sessions: BTreeMap::new(),
            transcript_secrets: BTreeMap::new(),
            transcripts: TranscriptStore::new(run_state_directory),
            #[cfg(test)]
            credential_observer: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_credential_observer(
        mut self,
        observer: std::sync::mpsc::Sender<LaunchedAgent>,
    ) -> Self {
        self.credential_observer = Some(observer);
        self
    }

    /// Persists the starting generation before crossing the provider boundary.
    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "used by provider conformance tests")
    )]
    pub(crate) fn launch(
        &mut self,
        store: &mut Store,
        launch: &AgentLaunch,
    ) -> Result<LaunchedAgent, AgentSessionError> {
        self.launch_session(store, launch, false, None)
    }

    /// Completes or reuses one durable launch intent without duplicating it.
    pub(crate) fn ensure_existing_launch(
        &mut self,
        store: &mut Store,
        launch: &AgentLaunch,
        assignment_id: Option<AssignmentId>,
    ) -> Result<Option<LaunchedAgent>, AgentSessionError> {
        let session = store.transaction(|repositories| {
            repositories.session(launch.scope.session_id)
        })?;
        let Some(session) = session else {
            return self
                .launch_session(store, launch, true, assignment_id)
                .map(Some);
        };
        let probe = self.provider.probe()?;
        if session.run_id != launch.scope.run_id
            || session.agent_id != launch.scope.agent_id
            || session.generation != launch.scope.generation
            || session.provider != probe.name
        {
            return Err(AgentSessionError::IntentMismatch {
                session_id: launch.scope.session_id,
            });
        }
        match session.reconciliation_state {
            ExternalResourceState::Observed => Ok(None),
            ExternalResourceState::Desired => {
                validate_provider_probe(&probe, launch)?;
                if self.sessions.contains_key(&launch.scope.session_id) {
                    self.record_launch_observation(
                        store,
                        launch.scope.session_id,
                        launch.created_at,
                    )?;
                    return Ok(None);
                }
                let token = AgentToken::generate()?;
                store.transaction(|repositories| {
                    repositories.replace_desired_session_credential(
                        &SessionCredentialRecord {
                            session_id: launch.scope.session_id,
                            run_id: launch.scope.run_id,
                            agent_id: launch.scope.agent_id,
                            generation: launch.scope.generation,
                            token_verifier: token.verifier(launch.scope),
                            created_at: launch.created_at,
                            revoked_at: None,
                        },
                    )
                })?;
                self.start_and_record(store, launch, token).map(Some)
            }
            state @ (ExternalResourceState::Lost
            | ExternalResourceState::Unknown) => {
                Err(AgentSessionError::UnresolvedIntent {
                    session_id: launch.scope.session_id,
                    state,
                })
            }
        }
    }

    fn launch_session(
        &mut self,
        store: &mut Store,
        launch: &AgentLaunch,
        agent_exists: bool,
        assignment_id: Option<AssignmentId>,
    ) -> Result<LaunchedAgent, AgentSessionError> {
        let token = self.persist_launch_intent(
            store,
            launch,
            agent_exists,
            assignment_id,
        )?;
        self.start_and_record(store, launch, token)
    }

    fn start_and_record(
        &mut self,
        store: &mut Store,
        launch: &AgentLaunch,
        token: AgentToken,
    ) -> Result<LaunchedAgent, AgentSessionError> {
        let launched = match self.start_provider(launch, token) {
            Ok(launched) => launched,
            Err(AgentSessionError::ScopeMismatch { expected, observed }) => {
                self.record_reconciliation(
                    store,
                    launch.scope,
                    ExternalResourceState::Unknown,
                    LifecycleState::Unknown,
                    launch.created_at,
                )?;
                return Err(AgentSessionError::ScopeMismatch {
                    expected,
                    observed,
                });
            }
            Err(error) => return Err(error),
        };
        self.record_launch_observation(
            store,
            launch.scope.session_id,
            launch.created_at,
        )?;
        Ok(launched)
    }

    fn persist_launch_intent(
        &mut self,
        store: &mut Store,
        launch: &AgentLaunch,
        agent_exists: bool,
        assignment_id: Option<AssignmentId>,
    ) -> Result<AgentToken, AgentSessionError> {
        let probe = self.provider.probe()?;
        validate_provider_probe(&probe, launch)?;

        let token = AgentToken::generate()?;
        let transcript_path =
            TranscriptStore::relative_path(launch.scope.session_id);
        store.transaction(|repositories| {
            if agent_exists {
                let agent = repositories.agent(launch.scope.agent_id)?;
                if !matches!(
                    agent,
                    Some(AgentRecord {
                        run_id,
                        generation,
                        state: LifecycleState::Starting,
                        ref role,
                        ..
                    }) if run_id == launch.scope.run_id
                        && generation == launch.scope.generation
                        && role == &launch.role
                ) {
                    return Err(assignment_id.map_or_else(
                        || StoreError::InconsistentSessionLifecycle {
                            session_id: launch.scope.session_id,
                            agent_id: launch.scope.agent_id,
                        },
                        |id| StoreError::CorruptAssignmentState {
                            id,
                            reason:
                                "the claimed agent launch intent does not match"
                                    .to_owned(),
                        },
                    ));
                }
            } else {
                repositories.insert_agent(&AgentRecord {
                    id: launch.scope.agent_id,
                    run_id: launch.scope.run_id,
                    role: launch.role.clone(),
                    generation: launch.scope.generation,
                    state: LifecycleState::Starting,
                    created_at: launch.created_at,
                })?;
                repositories.append_event(&NewEvent {
                    run_id: launch.scope.run_id,
                    kind: EventKind::AgentCreated,
                    actor: "supervisor".to_owned(),
                    subject: launch.scope.agent_id.to_string(),
                    project_id: None,
                    agent_id: Some(launch.scope.agent_id),
                    task_id: None,
                    operation_id: None,
                    correlation_id: None,
                    causation_id: None,
                    data: json!({
                        "generation": launch.scope.generation,
                        "role": launch.role,
                        "state": LifecycleState::Starting.as_str(),
                    }),
                    summary: format!(
                        "Created agent {}.",
                        launch.scope.agent_id
                    ),
                    created_at: launch.created_at,
                })?;
            }
            repositories.insert_session(&SessionRecord {
                id: launch.scope.session_id,
                run_id: launch.scope.run_id,
                agent_id: launch.scope.agent_id,
                generation: launch.scope.generation,
                provider: probe.name.clone(),
                provider_session_id: None,
                reconciliation_state: ExternalResourceState::Desired,
                state: LifecycleState::Starting,
                transcript_path,
                created_at: launch.created_at,
                ended_at: None,
                reconciled_at: None,
                process_owner: SessionProcessOwner::Supervisor,
            })?;
            repositories.activate_session_credential(
                &SessionCredentialRecord {
                    session_id: launch.scope.session_id,
                    run_id: launch.scope.run_id,
                    agent_id: launch.scope.agent_id,
                    generation: launch.scope.generation,
                    token_verifier: token.verifier(launch.scope),
                    created_at: launch.created_at,
                    revoked_at: None,
                },
            )?;
            let session_event = repositories.append_event(&NewEvent {
                run_id: launch.scope.run_id,
                kind: EventKind::SessionStarted,
                actor: "supervisor".to_owned(),
                subject: launch.scope.session_id.to_string(),
                project_id: None,
                agent_id: Some(launch.scope.agent_id),
                task_id: None,
                operation_id: None,
                correlation_id: None,
                causation_id: None,
                data: json!({
                    "generation": launch.scope.generation,
                    "provider": probe.name,
                    "state": LifecycleState::Starting.as_str(),
                }),
                summary: format!(
                    "Started session {} for agent {}.",
                    launch.scope.session_id, launch.scope.agent_id
                ),
                created_at: launch.created_at,
            })?;
            if let Some(assignment_id) = assignment_id {
                repositories.associate_assignment_session(
                    assignment_id,
                    launch.scope.session_id,
                )?;
                let assignment = repositories
                    .assignment(assignment_id)?
                    .ok_or_else(|| StoreError::CorruptAssignmentState {
                        id: assignment_id,
                        reason: "the assignment disappeared after session association"
                            .to_owned(),
                    })?;
                let task = repositories.task(assignment.task_id)?.ok_or_else(
                    || StoreError::CorruptAssignmentState {
                        id: assignment_id,
                        reason: "the assignment task does not exist".to_owned(),
                    },
                )?;
                repositories.append_event(&NewEvent {
                    run_id: launch.scope.run_id,
                    kind: EventKind::AssignmentSessionAssociated,
                    actor: "supervisor".to_owned(),
                    subject: assignment_id.to_string(),
                    project_id: Some(task.project_id),
                    agent_id: Some(launch.scope.agent_id),
                    task_id: Some(assignment.task_id),
                    operation_id: None,
                    correlation_id: Some(session_event.id),
                    causation_id: Some(session_event.id),
                    data: json!({"session_id": launch.scope.session_id}),
                    summary: format!(
                        "Associated session {} with assignment {assignment_id}.",
                        launch.scope.session_id
                    ),
                    created_at: launch.created_at,
                })?;
            }
            Ok(())
        })?;

        Ok(token)
    }

    fn start_provider(
        &mut self,
        launch: &AgentLaunch,
        token: AgentToken,
    ) -> Result<LaunchedAgent, AgentSessionError> {
        let transcript_secret = token.expose_secret().as_bytes().to_vec();
        let specification = LaunchSpecification {
            scope: launch.scope,
            working_directory: launch.working_directory.clone(),
            permission_profile: launch.permission_profile,
            bootstrap_instruction: launch.bootstrap_instruction.clone(),
        };
        let handle = match launch.mode {
            LaunchMode::Interactive => {
                self.provider.launch_interactive(&specification, None)?
            }
            LaunchMode::Job => self.provider.launch_job(
                &specification,
                &JobEnvironment {
                    project_id: launch.project_id,
                    primary_project_root: launch.primary_project_root.clone(),
                    role: launch.role.clone(),
                    task_id: launch.task_id,
                    socket_path: launch.socket_path.clone(),
                    token: token.clone(),
                },
            )?,
        };
        if handle.scope != launch.scope {
            return Err(AgentSessionError::ScopeMismatch {
                expected: Box::new(launch.scope),
                observed: Box::new(handle.scope),
            });
        }
        self.sessions.insert(launch.scope.session_id, handle);
        self.transcript_secrets
            .insert(launch.scope.session_id, transcript_secret);
        let launched = LaunchedAgent {
            scope: launch.scope,
            token,
        };
        #[cfg(test)]
        if let Some(observer) = &self.credential_observer {
            let _receiver_may_have_closed = observer.send(LaunchedAgent {
                scope: launched.scope,
                token: launched.token.clone(),
            });
        }
        Ok(launched)
    }

    fn record_launch_observation(
        &self,
        store: &mut Store,
        session_id: SessionId,
        observed_at: i64,
    ) -> Result<(), AgentSessionError> {
        let handle = self.handle(session_id)?;
        store.transaction(|repositories| {
            let Some(session) = repositories.session(session_id)? else {
                return Ok(());
            };
            let outcome = repositories.record_session_launch_observation(
                handle.scope,
                handle.provider_id(),
                observed_at,
            )?;
            if outcome == SessionTransitionOutcome::Applied {
                repositories.append_event(&NewEvent {
                    run_id: handle.scope.run_id,
                    kind: EventKind::SessionReconciliationChanged,
                    actor: "reconciler".to_owned(),
                    subject: session_id.to_string(),
                    project_id: None,
                    agent_id: Some(handle.scope.agent_id),
                    task_id: None,
                    operation_id: None,
                    correlation_id: None,
                    causation_id: None,
                    data: json!({
                        "previous_state": session.reconciliation_state.as_str(),
                        "provider_session_id": handle.provider_id(),
                        "state": ExternalResourceState::Observed.as_str(),
                    }),
                    summary: format!("Observed provider session {session_id}."),
                    created_at: observed_at,
                })?;
            }
            Ok(())
        })?;
        Ok(())
    }

    /// Conservatively classifies sessions that outlived a supervisor process.
    pub(crate) fn reconcile_after_restart(
        &mut self,
        store: &mut Store,
        run_id: crate::id::RunId,
        reconciled_at: i64,
    ) -> Result<(), AgentSessionError> {
        self.reconcile_sessions(store, run_id, reconciled_at, false)
    }

    /// Rechecks sessions whose provider state could not previously be proved.
    pub(crate) fn reconcile_unknown_sessions(
        &mut self,
        store: &mut Store,
        run_id: crate::id::RunId,
        reconciled_at: i64,
    ) -> Result<(), AgentSessionError> {
        self.reconcile_sessions(store, run_id, reconciled_at, true)
    }

    fn reconcile_sessions(
        &mut self,
        store: &mut Store,
        run_id: crate::id::RunId,
        reconciled_at: i64,
        unknown_only: bool,
    ) -> Result<(), AgentSessionError> {
        let sessions =
            store.transaction(|repositories| repositories.sessions(run_id))?;
        for session in sessions {
            if session.state.is_terminal()
                || unknown_only
                    && session.reconciliation_state
                        != ExternalResourceState::Unknown
            {
                continue;
            }
            let scope = SessionScope {
                run_id: session.run_id,
                agent_id: session.agent_id,
                session_id: session.id,
                generation: session.generation,
            };
            if session.process_owner == SessionProcessOwner::Foreground {
                self.record_reconciliation(
                    store,
                    scope,
                    ExternalResourceState::Unknown,
                    LifecycleState::Unknown,
                    reconciled_at,
                )?;
                continue;
            }
            let Some(provider_session_id) = session.provider_session_id else {
                self.record_reconciliation(
                    store,
                    scope,
                    ExternalResourceState::Unknown,
                    LifecycleState::Unknown,
                    reconciled_at,
                )?;
                continue;
            };
            match self.provider.recover(&provider_session_id, scope) {
                Ok(ProviderRecovery::Observed {
                    handle,
                    observation,
                }) => {
                    if handle.scope != scope {
                        self.record_reconciliation(
                            store,
                            scope,
                            ExternalResourceState::Unknown,
                            LifecycleState::Unknown,
                            reconciled_at,
                        )?;
                        continue;
                    }
                    self.sessions.insert(session.id, handle);
                    self.record_launch_observation(
                        store,
                        session.id,
                        reconciled_at,
                    )?;
                    let handle = self.handle(session.id)?.clone();
                    self.record_observation(
                        store,
                        &handle,
                        observation,
                        reconciled_at,
                    )?;
                }
                Ok(ProviderRecovery::Lost) => self.record_reconciliation(
                    store,
                    scope,
                    ExternalResourceState::Lost,
                    LifecycleState::Lost,
                    reconciled_at,
                )?,
                Ok(ProviderRecovery::Unknown) | Err(_) => {
                    self.record_reconciliation(
                        store,
                        scope,
                        ExternalResourceState::Unknown,
                        LifecycleState::Unknown,
                        reconciled_at,
                    )?;
                }
            }
        }
        Ok(())
    }

    fn record_reconciliation(
        &self,
        store: &mut Store,
        scope: SessionScope,
        reconciliation_state: ExternalResourceState,
        lifecycle: LifecycleState,
        reconciled_at: i64,
    ) -> Result<(), AgentSessionError> {
        store.transaction(|repositories| {
            let Some(session) = repositories.session(scope.session_id)? else {
                return Ok(());
            };
            let reconciliation = repositories
                .record_session_reconciliation_state(
                    scope,
                    reconciliation_state,
                    reconciled_at,
                )?;
            if reconciliation == SessionTransitionOutcome::Applied {
                repositories.append_event(&NewEvent {
                    run_id: scope.run_id,
                    kind: EventKind::SessionReconciliationChanged,
                    actor: "reconciler".to_owned(),
                    subject: scope.session_id.to_string(),
                    project_id: None,
                    agent_id: Some(scope.agent_id),
                    task_id: None,
                    operation_id: None,
                    correlation_id: None,
                    causation_id: None,
                    data: json!({
                        "previous_state": session.reconciliation_state.as_str(),
                        "state": reconciliation_state.as_str(),
                    }),
                    summary: format!(
                        "Session {} reconciliation changed from {} to {}.",
                        scope.session_id,
                        session.reconciliation_state,
                        reconciliation_state
                    ),
                    created_at: reconciled_at,
                })?;
            }
            let lifecycle_outcome = repositories.record_session_lifecycle(
                scope,
                lifecycle,
                reconciled_at,
            )?;
            if lifecycle_outcome == SessionTransitionOutcome::Applied {
                let session_event = repositories.append_event(&NewEvent {
                    run_id: scope.run_id,
                    kind: EventKind::SessionLifecycleChanged,
                    actor: "reconciler".to_owned(),
                    subject: scope.session_id.to_string(),
                    project_id: None,
                    agent_id: Some(scope.agent_id),
                    task_id: None,
                    operation_id: None,
                    correlation_id: None,
                    causation_id: None,
                    data: json!({
                        "generation": scope.generation,
                        "previous_state": session.state.as_str(),
                        "provider": session.provider,
                        "state": lifecycle.as_str(),
                    }),
                    summary: format!(
                        "Session {} changed from {} to {}.",
                        scope.session_id, session.state, lifecycle
                    ),
                    created_at: reconciled_at,
                })?;
                repositories.append_event(&NewEvent {
                    run_id: scope.run_id,
                    kind: EventKind::AgentLifecycleChanged,
                    actor: "reconciler".to_owned(),
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
                        "Agent {} changed from {} to {}.",
                        scope.agent_id, session.state, lifecycle
                    ),
                    created_at: reconciled_at,
                })?;
            }
            Ok(())
        })?;
        Ok(())
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
                self.record_observation(
                    store,
                    &handle,
                    *observation,
                    observed_at,
                )?;
            }
            ProviderEventKind::Output(bytes) => {
                self.append_transcript(session_id, bytes)?;
            }
            ProviderEventKind::MalformedOutput {
                bytes, observation, ..
            } => {
                self.append_transcript(session_id, bytes)?;
                self.record_observation(
                    store,
                    &handle,
                    *observation,
                    observed_at,
                )?;
            }
        }
        Ok(Some(event))
    }

    /// Drains a bounded batch of provider events without waiting for output.
    pub(crate) fn advance_available(
        &mut self,
        store: &mut Store,
        run_id: crate::id::RunId,
        observed_at: i64,
        maximum_events: usize,
    ) -> Result<usize, AgentSessionError> {
        self.reconcile_unknown_sessions(store, run_id, observed_at)?;
        let session_ids = self.sessions.keys().copied().collect::<Vec<_>>();
        let mut advanced = 0;
        loop {
            let mut made_progress = false;
            for session_id in &session_ids {
                if advanced == maximum_events {
                    return Ok(advanced);
                }
                if self.advance(store, *session_id, observed_at)?.is_some() {
                    advanced += 1;
                    made_progress = true;
                }
            }
            if !made_progress {
                return Ok(advanced);
            }
        }
    }

    fn append_transcript(
        &self,
        session_id: SessionId,
        bytes: &[u8],
    ) -> Result<(), TranscriptError> {
        self.transcript_secrets.get(&session_id).map_or_else(
            || self.transcripts.append(session_id, bytes),
            |secret| {
                self.transcripts.append_redacted(session_id, bytes, secret)
            },
        )
    }

    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "used by provider conformance tests")
    )]
    pub(crate) fn observe(
        &self,
        session_id: SessionId,
    ) -> Result<SessionObservation, AgentSessionError> {
        Ok(self.provider.observe(self.handle(session_id)?)?)
    }

    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "used by provider conformance tests")
    )]
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

    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "used by provider conformance tests")
    )]
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
        &mut self,
        store: &mut Store,
        handle: &ProviderSessionHandle,
        observation: SessionObservation,
        observed_at: i64,
    ) -> Result<(), AgentSessionError> {
        store.transaction(|repositories| {
            let Some(session) =
                repositories.session(handle.scope.session_id)?
            else {
                return Ok(());
            };
            let reconciliation_state = match observation.lifecycle {
                LifecycleState::Lost => ExternalResourceState::Lost,
                LifecycleState::Unknown => ExternalResourceState::Unknown,
                LifecycleState::Starting
                | LifecycleState::Running
                | LifecycleState::Exited
                | LifecycleState::Quarantined => {
                    ExternalResourceState::Observed
                }
            };
            let reconciliation = repositories
                .record_session_reconciliation_state(
                    handle.scope,
                    reconciliation_state,
                    observed_at,
                )?;
            if reconciliation == SessionTransitionOutcome::Applied {
                repositories.append_event(&NewEvent {
                    run_id: handle.scope.run_id,
                    kind: EventKind::SessionReconciliationChanged,
                    actor: "provider".to_owned(),
                    subject: handle.scope.session_id.to_string(),
                    project_id: None,
                    agent_id: Some(handle.scope.agent_id),
                    task_id: None,
                    operation_id: None,
                    correlation_id: None,
                    causation_id: None,
                    data: json!({
                        "previous_state": session.reconciliation_state.as_str(),
                        "state": reconciliation_state.as_str(),
                    }),
                    summary: format!(
                        "Session {} reconciliation changed from {} to {}.",
                        handle.scope.session_id,
                        session.reconciliation_state,
                        reconciliation_state
                    ),
                    created_at: observed_at,
                })?;
            }
            let outcome = repositories.record_session_lifecycle(
                handle.scope,
                observation.lifecycle,
                observed_at,
            )?;
            if outcome == SessionTransitionOutcome::Applied {
                let exit = observation.exit.map(|exit| {
                    json!({
                        "code": exit.code,
                        "reason": exit.reason.as_str(),
                    })
                });
                let session_event = repositories.append_event(&NewEvent {
                    run_id: handle.scope.run_id,
                    kind: EventKind::SessionLifecycleChanged,
                    actor: "provider".to_owned(),
                    subject: handle.scope.session_id.to_string(),
                    project_id: None,
                    agent_id: Some(handle.scope.agent_id),
                    task_id: None,
                    operation_id: None,
                    correlation_id: None,
                    causation_id: None,
                    data: json!({
                        "generation": handle.scope.generation,
                        "previous_state": session.state.as_str(),
                        "provider": session.provider,
                        "state": observation.lifecycle.as_str(),
                        "exit": exit,
                    }),
                    summary: format!(
                        "Session {} changed from {} to {}.",
                        handle.scope.session_id,
                        session.state,
                        observation.lifecycle
                    ),
                    created_at: observed_at,
                })?;
                repositories.append_event(&NewEvent {
                    run_id: handle.scope.run_id,
                    kind: EventKind::AgentLifecycleChanged,
                    actor: "provider".to_owned(),
                    subject: handle.scope.agent_id.to_string(),
                    project_id: None,
                    agent_id: Some(handle.scope.agent_id),
                    task_id: None,
                    operation_id: None,
                    correlation_id: Some(session_event.id),
                    causation_id: Some(session_event.id),
                    data: json!({
                        "generation": handle.scope.generation,
                        "previous_state": session.state.as_str(),
                        "state": observation.lifecycle.as_str(),
                    }),
                    summary: format!(
                        "Agent {} changed from {} to {}.",
                        handle.scope.agent_id,
                        session.state,
                        observation.lifecycle
                    ),
                    created_at: observed_at,
                })?;
            }
            Ok(())
        })?;
        if observation.lifecycle.is_terminal() {
            self.transcript_secrets.remove(&handle.scope.session_id);
        }
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
    let structured = matches!(launch.mode, LaunchMode::Job)
        .then_some(ProviderCapability::StructuredLifecycleEvents);
    let transcript = matches!(launch.mode, LaunchMode::Job)
        .then_some(ProviderCapability::TranscriptStreaming);
    let interrupt = matches!(launch.mode, LaunchMode::Job)
        .then_some(ProviderCapability::Interrupt);
    let termination = matches!(launch.mode, LaunchMode::Job)
        .then_some(ProviderCapability::Termination);
    [
        Some(mode),
        startup,
        structured,
        transcript,
        interrupt,
        termination,
    ]
    .into_iter()
    .flatten()
    .chain(required_permission_capabilities(launch.permission_profile))
}

pub(crate) fn required_permission_capabilities(
    permission_profile: PermissionProfile,
) -> impl Iterator<Item = ProviderCapability> {
    let network = (permission_profile.network == NetworkPolicy::Deny)
        .then_some(ProviderCapability::NetworkSandbox);
    [
        Some(ProviderCapability::WorkingDirectory),
        Some(ProviderCapability::FilesystemSandbox),
        network,
        Some(ProviderCapability::ApprovalPolicy),
    ]
    .into_iter()
    .flatten()
}

fn validate_provider_probe(
    probe: &crate::providers::ProviderProbe,
    launch: &AgentLaunch,
) -> Result<(), AgentSessionError> {
    validate_provider_capabilities(probe, required_capabilities(launch))
}

pub(crate) fn validate_provider_capabilities(
    probe: &crate::providers::ProviderProbe,
    capabilities: impl IntoIterator<Item = ProviderCapability>,
) -> Result<(), AgentSessionError> {
    if let crate::providers::ProviderCompatibility::Incompatible {
        reason,
        remedy,
    } = &probe.compatibility
    {
        return Err(AgentSessionError::IncompatibleProvider {
            provider: probe.name.clone(),
            version: probe.version.clone(),
            reason: reason.clone(),
            remedy: remedy.clone(),
        });
    }
    for capability in capabilities {
        if !probe.capabilities.contains(&capability) {
            return Err(AgentSessionError::MissingCapability {
                provider: probe.name.clone(),
                version: probe.version.clone(),
                capability,
            });
        }
    }
    Ok(())
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
    #[error(
        "provider `{provider}` version {version} is incompatible: {reason}; {remedy}"
    )]
    IncompatibleProvider {
        provider: String,
        version: semver::Version,
        reason: String,
        remedy: String,
    },
    #[error(
        "provider `{provider}` version {version} does not support {capability}; update the provider or select a compatible provider binding"
    )]
    MissingCapability {
        provider: String,
        version: semver::Version,
        capability: ProviderCapability,
    },
    #[error("provider session scope is {observed:?}, expected {expected:?}")]
    ScopeMismatch {
        expected: Box<SessionScope>,
        observed: Box<SessionScope>,
    },
    #[error("session `{session_id}` is not managed by this supervisor")]
    UnknownSession { session_id: SessionId },
    #[error("session `{session_id}` does not match its durable launch intent")]
    IntentMismatch { session_id: SessionId },
    #[error("session `{session_id}` launch state is `{state}`")]
    UnresolvedIntent {
        session_id: SessionId,
        state: ExternalResourceState,
    },
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::thread;
    use std::time::{Duration, Instant};

    use super::{AgentLaunch, AgentSessionSupervisor};
    use crate::auth::SessionScope;
    use crate::config::builtin_standard;
    use crate::id::{AgentId, ProjectId, RunId, SessionId, TaskId};
    use crate::providers::fake::{FakeEvent, FakeProvider, FakeScript};
    use crate::providers::{
        ActivityState, CodexProvider, LaunchMode, LifecycleState,
        ProviderCapability, ProviderCompatibility, SessionObservation,
    };
    use crate::state::{ExternalResourceState, RunRecord, Store};

    const RUN_ID: &str = "cr-01ARZ3NDEKTSV4RRFFQ69G5FAV";
    const AGENT_ID: &str = "cg-01ARZ3NDEKTSV4RRFFQ69G5FAX";
    const SESSION_ID: &str = "cs-01ARZ3NDEKTSV4RRFFQ69G5FAY";
    const PROJECT_ID: &str = "cp-01ARZ3NDEKTSV4RRFFQ69G5FAW";
    const TASK_ID: &str = "ct-01ARZ3NDEKTSV4RRFFQ69G5FAZ";

    #[test]
    fn failed_launch_retains_durable_desired_state() {
        let directory = TestDirectory::new();
        let mut store = store_with_run(&directory);
        let run_id = RUN_ID.parse::<RunId>().expect("valid run ID");
        let mut supervisor =
            AgentSessionSupervisor::new(FakeProvider::new([]), &directory.0);

        assert!(
            supervisor.launch(&mut store, &launch(run_id)).is_err(),
            "the scripted launch should fail"
        );

        store
            .transaction(|repositories| {
                let session = repositories
                    .session(launch(run_id).scope.session_id)?
                    .expect("launch intent should survive provider failure");
                assert_eq!(
                    session.reconciliation_state,
                    ExternalResourceState::Desired
                );
                assert_eq!(session.provider_session_id, None);
                Ok(())
            })
            .expect("the launch intent should be readable");
    }

    #[test]
    fn incompatible_provider_is_rejected_before_launch_intent() {
        let directory = TestDirectory::new();
        let mut store = store_with_run(&directory);
        let run_id = RUN_ID.parse::<RunId>().expect("valid run ID");
        let provider = FakeProvider::new([FakeScript::new([])])
            .with_compatibility(ProviderCompatibility::Incompatible {
                reason: "fixture version is unsupported".to_owned(),
                remedy: "install fixture provider 2.0 or later".to_owned(),
            });
        let mut supervisor =
            AgentSessionSupervisor::new(provider, &directory.0);

        let error = match supervisor.launch(&mut store, &launch(run_id)) {
            Err(error) => error,
            Ok(_) => panic!("an incompatible provider must not launch"),
        };

        assert!(error.to_string().contains("fixture version is unsupported"));
        assert!(error.to_string().contains("install fixture provider"));
        assert!(supervisor.provider.launches().is_empty());
        store
            .transaction(|repositories| {
                assert_eq!(
                    repositories.session(launch(run_id).scope.session_id)?,
                    None
                );
                Ok(())
            })
            .expect("failed preflight must not create durable launch intent");
    }

    #[test]
    fn missing_required_capability_is_rejected_before_launch_intent() {
        let directory = TestDirectory::new();
        let mut store = store_with_run(&directory);
        let run_id = RUN_ID.parse::<RunId>().expect("valid run ID");
        let provider = FakeProvider::new([FakeScript::new([])])
            .without_capability(ProviderCapability::BackgroundJobs);
        let mut supervisor =
            AgentSessionSupervisor::new(provider, &directory.0);

        let error = match supervisor.launch(&mut store, &launch(run_id)) {
            Err(error) => error,
            Ok(_) => panic!("a provider missing job support must not launch"),
        };

        let message = error.to_string();
        assert!(message.contains("fake"));
        assert!(message.contains("1.0.0"));
        assert!(message.contains("background job execution"));
        assert!(message.contains("update the provider"));
        assert!(supervisor.provider.launches().is_empty());
        store
            .transaction(|repositories| {
                assert_eq!(
                    repositories.session(launch(run_id).scope.session_id)?,
                    None
                );
                Ok(())
            })
            .expect("failed preflight must not create durable launch intent");
    }

    #[test]
    fn missing_permission_capability_is_rejected_before_launch_intent() {
        for (capability, diagnostic) in [
            (ProviderCapability::WorkingDirectory, "working directory"),
            (ProviderCapability::FilesystemSandbox, "filesystem"),
            (ProviderCapability::NetworkSandbox, "network"),
            (ProviderCapability::ApprovalPolicy, "approval"),
            (
                ProviderCapability::StartupInstructions,
                "startup instruction",
            ),
        ] {
            let directory = TestDirectory::new();
            let mut store = store_with_run(&directory);
            let run_id = RUN_ID.parse::<RunId>().expect("valid run ID");
            let provider = FakeProvider::new([FakeScript::new([])])
                .without_capability(capability);
            let mut supervisor =
                AgentSessionSupervisor::new(provider, &directory.0);

            let error = match supervisor.launch(&mut store, &launch(run_id)) {
                Err(error) => error,
                Ok(_) => panic!("an unenforceable permission must fail closed"),
            };

            assert!(error.to_string().contains(diagnostic));
            assert!(supervisor.provider.launches().is_empty());
            store
                .transaction(|repositories| {
                    assert_eq!(
                        repositories
                            .session(launch(run_id).scope.session_id)?,
                        None
                    );
                    Ok(())
                })
                .expect(
                    "failed preflight must not create durable launch intent",
                );
        }
    }

    #[test]
    fn successful_launch_records_observed_provider_identity() {
        let directory = TestDirectory::new();
        let mut store = store_with_run(&directory);
        let run_id = RUN_ID.parse::<RunId>().expect("valid run ID");
        let mut supervisor = AgentSessionSupervisor::new(
            FakeProvider::new([FakeScript::new([])]),
            &directory.0,
        );

        supervisor
            .launch(&mut store, &launch(run_id))
            .expect("the fake session should launch");

        store
            .transaction(|repositories| {
                let session = repositories
                    .session(launch(run_id).scope.session_id)?
                    .expect("the launch observation should be durable");
                assert_eq!(
                    session.reconciliation_state,
                    ExternalResourceState::Observed
                );
                assert_eq!(
                    session.provider_session_id.as_deref(),
                    Some("fake-session-1")
                );
                assert_eq!(session.reconciled_at, Some(10));
                Ok(())
            })
            .expect("the launch observation should be readable");
    }

    #[test]
    fn restart_reconciliation_preserves_launch_uncertainty() {
        let directory = TestDirectory::new();
        let mut store = store_with_run(&directory);
        let run_id = RUN_ID.parse::<RunId>().expect("valid run ID");
        let launch = launch(run_id);
        let mut crashed = AgentSessionSupervisor::new(
            FakeProvider::new([FakeScript::new([])]),
            &directory.0,
        );

        let token = crashed
            .persist_launch_intent(&mut store, &launch, false, None)
            .expect("intent should commit before launch");
        crashed
            .start_provider(&launch, token)
            .expect("the side effect should happen");
        drop(crashed);

        let mut restarted =
            AgentSessionSupervisor::new(FakeProvider::new([]), &directory.0);
        restarted
            .reconcile_after_restart(&mut store, run_id, 11)
            .expect("restart reconciliation should succeed");

        store
            .transaction(|repositories| {
                let session = repositories
                    .session(launch.scope.session_id)?
                    .expect("the uncertain session should remain durable");
                assert_eq!(
                    session.reconciliation_state,
                    ExternalResourceState::Unknown
                );
                assert_eq!(session.state, LifecycleState::Unknown);
                Ok(())
            })
            .expect("the uncertain session should be readable");
    }

    #[test]
    fn restart_reconciliation_migrates_legacy_fake_session_to_lost_once() {
        let directory = TestDirectory::new();
        let mut store = store_with_run(&directory);
        let run_id = RUN_ID.parse::<RunId>().expect("valid run ID");
        let launch = launch(run_id);
        let mut first = AgentSessionSupervisor::new(
            FakeProvider::new([FakeScript::new([])]),
            &directory.0,
        );
        first
            .launch(&mut store, &launch)
            .expect("the fake session should launch");
        drop(first);

        let mut restarted = AgentSessionSupervisor::new(
            CodexProvider::new(["codex"]),
            &directory.0,
        );
        restarted
            .reconcile_after_restart(&mut store, run_id, 11)
            .expect("restart reconciliation should succeed");
        restarted
            .reconcile_after_restart(&mut store, run_id, 12)
            .expect("repeated reconciliation should be idempotent");

        store
            .transaction(|repositories| {
                let session = repositories
                    .session(launch.scope.session_id)?
                    .expect("the lost session should remain durable");
                assert_eq!(
                    session.reconciliation_state,
                    ExternalResourceState::Lost
                );
                assert_eq!(session.state, LifecycleState::Lost);
                let reconciled_events = repositories
                    .events_after(run_id, 0, 100)?
                    .into_iter()
                    .filter(|event| {
                        event.event_type == "session.reconciliation_changed"
                            && event.payload["data"]["state"] == "lost"
                    })
                    .count();
                assert_eq!(reconciled_events, 1);
                Ok(())
            })
            .expect("the lost session should be readable");
    }

    #[test]
    fn polling_rechecks_an_unknown_session_until_it_is_lost() {
        let directory = TestDirectory::new();
        let mut store = store_with_run(&directory);
        let run_id = RUN_ID.parse::<RunId>().expect("valid run ID");
        let launch = launch(run_id);
        let mut first = AgentSessionSupervisor::new(
            FakeProvider::new([FakeScript::new([])]),
            &directory.0,
        );
        first
            .launch(&mut store, &launch)
            .expect("the fake session should launch");
        drop(first);

        let provider = FakeProvider::new([]).with_missing_recoveries([
            crate::providers::ProviderRecovery::Unknown,
            crate::providers::ProviderRecovery::Lost,
        ]);
        let mut restarted = AgentSessionSupervisor::new(provider, &directory.0);
        restarted
            .reconcile_after_restart(&mut store, run_id, 11)
            .expect("initial restart reconciliation should remain uncertain");
        restarted
            .advance_available(&mut store, run_id, 12, 10)
            .expect("polling should retry unresolved recovery");

        store
            .transaction(|repositories| {
                let session = repositories
                    .session(launch.scope.session_id)?
                    .expect("the lost session should remain durable");
                assert_eq!(
                    session.reconciliation_state,
                    ExternalResourceState::Lost
                );
                assert_eq!(session.state, LifecycleState::Lost);
                Ok(())
            })
            .expect("the retried recovery should be readable");
    }

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
                let event_types = repositories
                    .events_after(run_id, 0, 100)?
                    .into_iter()
                    .map(|event| event.event_type)
                    .collect::<Vec<_>>();
                assert_eq!(
                    event_types,
                    [
                        "agent.created",
                        "session.started",
                        "session.reconciliation_changed",
                        "session.lifecycle_changed",
                        "agent.lifecycle_changed",
                        "session.lifecycle_changed",
                        "agent.lifecycle_changed",
                    ]
                );
                Ok(())
            })
            .expect("the terminal state should be readable");
    }

    #[test]
    fn malformed_provider_frames_are_appended_before_quarantine() {
        let directory = TestDirectory::new();
        let mut store = store_with_run(&directory);
        let run_id = RUN_ID.parse::<RunId>().expect("valid run ID");
        let provider =
            FakeProvider::new([FakeScript::new([FakeEvent::malformed(
                b"not-json\n",
                "invalid JSON",
            )])]);
        let mut supervisor =
            AgentSessionSupervisor::new(provider, &directory.0);
        let launch = launch(run_id);

        supervisor
            .launch(&mut store, &launch)
            .expect("the fake worker should launch");
        supervisor
            .advance(&mut store, launch.scope.session_id, 11)
            .expect("the malformed event should be durable")
            .expect("the malformed event should exist");

        assert_eq!(
            fs::read(directory.0.join(
                crate::transcript::TranscriptStore::relative_path(
                    launch.scope.session_id,
                ),
            ))
            .expect("the malformed transcript should be readable"),
            b"not-json\n"
        );
        store
            .transaction(|repositories| {
                let session = repositories
                    .session(launch.scope.session_id)?
                    .expect("the quarantined session should remain durable");
                assert_eq!(session.state, LifecycleState::Quarantined);
                assert_eq!(session.ended_at, Some(11));
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
            .expect("the quarantine should be durable");
    }

    #[test]
    fn codex_worker_jsonl_and_identity_are_stored_without_the_token() {
        let directory = TestDirectory::new();
        let executable = directory.0.join("codex-worker-fixture");
        fs::write(
            &executable,
            "#!/bin/sh\n\
             if [ \"$1\" = \"--version\" ]; then printf 'codex-cli 0.151.0\\n'; exit 0; fi\n\
             if [ \"$1\" = \"--help\" ]; then printf 'Usage: codex [OPTIONS] [PROMPT]\\n  --config <key=value>\\n  --cd <DIR>\\n  --sandbox <SANDBOX_MODE>\\n  --ask-for-approval <APPROVAL_POLICY>\\n'; exit 0; fi\n\
             if [ \"$1\" = \"exec\" ] && [ \"$2\" = \"--help\" ]; then printf 'Usage: codex exec [OPTIONS] [PROMPT]\\n  --config <key=value>\\n  --cd <DIR>\\n  --sandbox <SANDBOX_MODE>\\n  --ask-for-approval <APPROVAL_POLICY>\\n  --json\\n'; exit 0; fi\n\
             {\n\
               printf 'project_id=%s\\n' \"$COTERIE_PROJECT_ID\"\n\
               printf 'run_id=%s\\n' \"$COTERIE_RUN_ID\"\n\
               printf 'agent_id=%s\\n' \"$COTERIE_AGENT_ID\"\n\
               printf 'session_id=%s\\n' \"$COTERIE_SESSION_ID\"\n\
               printf 'role=%s\\n' \"$COTERIE_ROLE\"\n\
               printf 'task_id=%s\\n' \"$COTERIE_TASK_ID\"\n\
               printf 'socket=%s\\n' \"$COTERIE_SOCKET\"\n\
               case \"$COTERIE_TOKEN\" in cot1_*) printf 'token=scoped\\n';; esac\n\
               if [ \"${HOME+x}\" = x ]; then printf 'ambient_home=present\\n'; fi\n\
             } > \"$PWD/job-environment\"\n\
             printf '{\"type\":\"thread.started\",\"thread_id\":\"thread-1\"}\\n'\n\
             printf '{\"type\":\"item.completed\",\"token\":\"%s\"}\\n' \"$COTERIE_TOKEN\"\n\
             printf '{\"type\":\"turn.completed\",\"usage\":{}}\\n'\n\
             exit 17\n",
        )
        .expect("the fake Codex executable should be written");
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700))
            .expect("the fake Codex executable should be private");
        let mut store = store_with_run(&directory);
        let run_id = RUN_ID.parse::<RunId>().expect("valid run ID");
        let mut supervisor = AgentSessionSupervisor::new(
            CodexProvider::new([executable.as_os_str()]),
            &directory.0,
        );
        let mut launch = launch(run_id);
        launch.working_directory = directory.0.clone();
        launch.primary_project_root = directory.0.clone();
        launch.socket_path = directory.0.join("coterie.sock");

        supervisor
            .launch(&mut store, &launch)
            .expect("the Codex worker should launch");
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match supervisor
                .advance(&mut store, launch.scope.session_id, 11)
                .expect("the Codex event should be applied")
            {
                Some(event)
                    if event.observation().is_some_and(|observation| {
                        observation.lifecycle.is_terminal()
                    }) =>
                {
                    break;
                }
                Some(_) => {}
                None => {
                    assert!(
                        Instant::now() < deadline,
                        "Codex worker event stream timed out"
                    );
                    thread::sleep(Duration::from_millis(10));
                }
            }
        }

        let transcript = fs::read(directory.0.join(
            crate::transcript::TranscriptStore::relative_path(
                launch.scope.session_id,
            ),
        ))
        .expect("the worker transcript should be readable");
        assert!(transcript.starts_with(b"{\"type\":\"thread.started\""));
        assert!(
            transcript
                .windows(b"[REDACTED]".len())
                .any(|bytes| bytes == b"[REDACTED]")
        );
        assert!(
            !transcript
                .windows(b"cot1_".len())
                .any(|bytes| bytes == b"cot1_")
        );

        let environment =
            fs::read_to_string(directory.0.join("job-environment"))
                .expect("the worker environment should be captured");
        for expected in [
            format!("project_id={PROJECT_ID}"),
            format!("run_id={RUN_ID}"),
            format!("agent_id={AGENT_ID}"),
            format!("session_id={SESSION_ID}"),
            "role=worker".to_owned(),
            format!("task_id={TASK_ID}"),
            format!("socket={}", launch.socket_path.display()),
            "token=scoped".to_owned(),
        ] {
            assert!(environment.lines().any(|line| line == expected));
        }
        assert!(environment.contains("ambient_home=present"));

        store
            .transaction(|repositories| {
                let session = repositories
                    .session(launch.scope.session_id)?
                    .expect("the worker session should remain durable");
                assert_eq!(session.state, LifecycleState::Exited);
                assert_eq!(session.ended_at, Some(11));
                let exit_event = repositories
                    .events_after(run_id, 0, 100)?
                    .into_iter()
                    .find(|event| {
                        event.event_type == "session.lifecycle_changed"
                            && event.payload["data"]["state"] == "exited"
                    })
                    .expect("the process exit should emit a lifecycle event");
                assert_eq!(exit_event.payload["data"]["exit"]["code"], 17);
                assert_eq!(
                    exit_event.payload["data"]["exit"]["reason"],
                    "process"
                );
                Ok(())
            })
            .expect("the classified exit should be durable");
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
                    let session = repositories
                        .session(launch.scope.session_id)?
                        .expect("the session should remain durable");
                    assert_eq!(session.state, terminal);
                    assert_eq!(
                        session.reconciliation_state,
                        if terminal == LifecycleState::Lost {
                            ExternalResourceState::Lost
                        } else {
                            ExternalResourceState::Observed
                        }
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
            project_id: PROJECT_ID
                .parse::<ProjectId>()
                .expect("valid project ID"),
            primary_project_root: PathBuf::from("/tmp/project"),
            task_id: TASK_ID.parse::<TaskId>().expect("valid task ID"),
            socket_path: PathBuf::from("/tmp/coterie.sock"),
            permission_profile: *builtin_standard()
                .permission_profiles
                .get("worker")
                .expect("the worker permission profile should exist"),
            bootstrap_instruction: "Run `coterie prime`.".to_owned(),
            created_at: 10,
        }
    }

    fn store_with_run(directory: &TestDirectory) -> Store {
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
        store
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

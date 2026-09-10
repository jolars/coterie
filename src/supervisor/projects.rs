//! The supervisor retains every lease until the run's indexes are retired.

use super::*;
use crate::project::ProjectKey;

pub(super) struct AttachedProjects {
    directories: CoterieDirectories,
    primary_key: ProjectKey,
    leases: BTreeMap<ProjectKey, (ActiveRunEntry, ProjectLease)>,
}

impl AttachedProjects {
    #[cfg(test)]
    pub(super) fn empty_for_test(path: &Path) -> Self {
        Self {
            directories: CoterieDirectories::from_base_directories(path, path)
                .unwrap(),
            primary_key: ProjectKey::for_identity(
                &crate::project::ProjectIdentity::Directory {
                    canonical_directory: path.to_owned(),
                },
            ),
            leases: BTreeMap::new(),
        }
    }

    pub(super) fn new(
        directories: CoterieDirectories,
        active: ActiveRunEntry,
        lease: ProjectLease,
    ) -> Self {
        Self {
            directories,
            primary_key: active.project_key.clone(),
            leases: BTreeMap::from([(
                active.project_key.clone(),
                (active, lease),
            )]),
        }
    }

    pub(super) fn proof(
        &self,
        request: &HandshakeRequest,
    ) -> Result<ActiveRunEntry, RpcFailure> {
        let (entry, _) =
            self.leases.get(&request.project_key).ok_or_else(|| {
                RpcFailure::new(
                    RpcFailureCode::ProjectMismatch,
                    "socket does not own the expected project identity",
                )
            })?;
        if ActiveRunIndex::new(&self.directories)
            .lookup(&entry.project_identity)
            .map_err(SupervisorError::from)
            .map_err(attachment_failure)?
            .as_ref()
            != Some(entry)
        {
            return Err(conflict(
                "project attachment publication is incomplete",
            ));
        }
        if let Some(error) = validate_handshake(request, entry) {
            return Err(error);
        }
        Ok(entry.clone())
    }

    pub(super) fn recover(
        &mut self,
        store: &mut Store,
        run_id: RunId,
        stopped: bool,
    ) -> Result<(), SupervisorError> {
        let projects =
            store.transaction(|repositories| repositories.projects(run_id))?;
        for project in projects {
            if stopped
                && ActiveRunIndex::new(&self.directories)
                    .lookup(&project.identity)?
                    .is_none_or(|entry| entry.run_id != run_id)
            {
                continue;
            }
            self.acquire(&project)?;
            if !stopped && !project.is_primary {
                self.publish(&project)?;
            }
        }
        if !stopped {
            let operations = store.transaction(|repositories| {
                repositories.operations_requiring_reconciliation(run_id)
            })?;
            for operation in operations
                .into_iter()
                .filter(|operation| operation.kind == "project.attach")
            {
                let project = operation_result::<ProjectRecord>(&operation)
                    .map_err(conflict)?;
                let result = self.complete(
                    store,
                    &project,
                    operation.id,
                    operation.actor_agent_id,
                );
                // Failure remains durable and inspectable without granting the project.
                self.observe(store, operation.id, &result)?;
            }
        }
        Ok(())
    }

    fn acquire(
        &mut self,
        project: &ProjectRecord,
    ) -> Result<(), SupervisorError> {
        let discovered = DiscoveredProject::discover(&project.canonical_path)?;
        if discovered.canonical_path != project.canonical_path
            || discovered.identity != project.identity
        {
            return Err(conflict(format!("project `{}` moved or changed identity; restore its recorded root", project.alias)).into());
        }
        let key = ProjectKey::for_identity(&project.identity);
        if self.leases.contains_key(&key) {
            return Ok(());
        }
        let lease = match ProjectLease::try_acquire(&self.directories, &project.identity, project.run_id)? {
            LeaseAttempt::Acquired(lease) => lease,
            LeaseAttempt::Held => return Err(conflict(format!("project `{}` has an exclusive lease held by another supervisor", project.alias)).into()),
        };
        let entry = ActiveRunEntry::new(
            project.run_id,
            project.id,
            project.identity.clone(),
        );
        if let Some(previous) =
            ActiveRunIndex::new(&self.directories).lookup(&project.identity)?
            && previous != entry
        {
            return Err(conflict(format!("project `{}` is indexed to run {}; recover or stop that run before attaching", project.alias, previous.run_id)).into());
        }
        self.leases.insert(key, (entry, lease));
        Ok(())
    }

    pub(super) fn publish(
        &self,
        project: &ProjectRecord,
    ) -> Result<(), SupervisorError> {
        let key = ProjectKey::for_identity(&project.identity);
        let (entry, lease) = &self.leases[&key];
        let index = ActiveRunIndex::new(&self.directories);
        if index.lookup(&project.identity)?.as_ref() != Some(entry) {
            index.publish(entry, lease)?;
        }
        Ok(())
    }

    pub(super) fn retire(&self) -> Result<(), ProjectError> {
        // The primary index remains a recovery entrypoint until every secondary
        // index is retired, even if a crash interrupts shutdown.
        let mut leases = self.leases.values().collect::<Vec<_>>();
        leases.sort_by_key(|(entry, _)| entry.project_key == self.primary_key);
        for (entry, lease) in leases {
            ActiveRunIndex::new(&self.directories).retire(
                &entry.project_identity,
                entry.run_id,
                lease,
            )?;
        }
        Ok(())
    }

    pub(super) fn attach(
        &mut self,
        store: &mut Store,
        run_id: RunId,
        caller: &AuthenticatedCaller,
        request: RpcRequest,
    ) -> Result<RpcResponse, RpcFailure> {
        require_current_caller(store, run_id, caller)?;
        require_capability(store, run_id, caller, "project", "attach")?;
        let RpcRequest::ProjectAttach {
            operation_id,
            path,
            alias,
        } = request
        else {
            unreachable!("attachment dispatch requires an attachment request");
        };
        let now = rpc_timestamp()?;
        let mutation = Mutation {
            id: operation_id,
            run_id,
            kind: "project.attach".into(),
            actor_agent_id: caller.agent_id(),
            request: json!({ "path": std::os::unix::ffi::OsStrExt::as_bytes(path.as_os_str()), "alias": alias }),
            created_at: now,
        };
        let existing = store
            .transaction(|repositories| repositories.operation(operation_id))
            .map_err(rpc_state_failure)?;
        let observed = existing.as_ref().is_some_and(|operation| {
            operation.reconciliation_state
                == Some(ExternalResourceState::Observed)
        });
        if !observed
            && store
                .transaction(|repositories| {
                    Ok(repositories.run_shutdown(run_id)?.is_some())
                })
                .map_err(rpc_state_failure)?
        {
            return Err(conflict(
                "the run is stopping and no longer accepts project attachments",
            ));
        }
        let candidate = if existing.is_some() {
            None
        } else {
            if !path.is_absolute() {
                return Err(invalid_argument(
                    "attachment requires an absolute project path",
                ));
            }
            let project = DiscoveredProject::discover(&path)
                .map_err(SupervisorError::from)
                .map_err(attachment_failure)?;
            let policy =
                store.configuration(run_id).map_err(rpc_state_failure)?;
            if !caller.is_operator()
                && !root_allowed(
                    &project.canonical_path,
                    &policy.allowed_project_roots,
                )
            {
                return Err(RpcFailure::new(
                    RpcFailureCode::PermissionDenied,
                    "canonical project root is outside the trusted allowed_project_roots; the operator can explicitly attach it",
                ));
            }
            let alias = alias
                .or_else(|| {
                    project
                        .canonical_path
                        .file_name()?
                        .to_str()
                        .map(str::to_owned)
                })
                .ok_or_else(|| {
                    invalid_argument(
                        "this project root requires an explicit --alias",
                    )
                })?;
            if alias.is_empty()
                || !alias.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')
                })
            {
                return Err(invalid_argument(
                    "project alias must contain only ASCII letters, digits, `_`, or `-`",
                ));
            }
            let attached = store
                .transaction(|repositories| repositories.projects(run_id))
                .map_err(rpc_state_failure)?;
            if let Some(previous) = attached.iter().find(|item| {
                item.alias == alias || item.identity == project.identity
            }) {
                if previous.alias != alias
                    || previous.identity != project.identity
                {
                    return Err(conflict(format!(
                        "project alias or identity conflicts with attached project `{}`",
                        previous.alias
                    )));
                }
                Some(previous.clone())
            } else {
                // Until per-project overlays are implemented, attachment accepts only
                // restrictions already satisfied by the active run's policy.
                validate_restrictions(&project, &policy)?;
                Some(ProjectRecord {
                    id: ProjectId::generate(),
                    run_id,
                    alias,
                    original_path: project.original_path,
                    canonical_path: project.canonical_path,
                    identity: project.identity,
                    is_primary: false,
                    attached_at: now,
                })
            }
        };
        let project = mutation_value(
            store
                .mutate(&mutation, |repositories| {
                    repositories
                        .mark_operation_reconciliation_desired(operation_id)?;
                    Ok(candidate
                        .expect("a new attachment has a resolved project"))
                })
                .map_err(rpc_state_failure)?,
        );
        if observed {
            return Ok(RpcResponse::ProjectAttached {
                operation_id,
                project: project_summary(project),
            });
        }
        crate::fault::point("project.attach.intent.after");
        let result =
            self.complete(store, &project, operation_id, caller.agent_id());
        self.observe(store, operation_id, &result)
            .map_err(attachment_failure)?;
        result.map_err(attachment_failure)?;
        Ok(RpcResponse::ProjectAttached {
            operation_id,
            project: project_summary(project),
        })
    }

    fn complete(
        &mut self,
        store: &mut Store,
        project: &ProjectRecord,
        operation_id: OperationId,
        actor_agent_id: Option<AgentId>,
    ) -> Result<(), SupervisorError> {
        let attached = store.transaction(|repositories| {
            repositories.projects(project.run_id)
        })?;
        if attached.iter().any(|item| {
            item.id != project.id
                && (item.alias == project.alias
                    || item.identity == project.identity)
        }) {
            return Err(conflict(
                "project alias or identity was taken by another attachment",
            )
            .into());
        }
        self.acquire(project)?;
        crate::fault::point("project.attach.lease.after");
        store.transaction(|repositories| {
            if repositories.project(project.id)?.is_none() {
                repositories.insert_project(project)?;
                repositories.append_event(&NewEvent {
                    run_id: project.run_id, kind: EventKind::ProjectAttached,
                    actor: actor_agent_id.map_or_else(|| "operator".into(), |id| id.to_string()),
                    subject: project.id.to_string(), project_id: Some(project.id), agent_id: actor_agent_id,
                    task_id: None, operation_id: Some(operation_id), correlation_id: None, causation_id: None,
                    data: json!({ "alias": project.alias, "original_path": project.original_path.to_string_lossy(), "canonical_path": project.canonical_path.to_string_lossy(), "identity": project.identity, "operator_authorized": actor_agent_id.is_none() }),
                    summary: format!("Attached project `{}`.", project.alias), created_at: project.attached_at,
                })?;
            }
            Ok(())
        })?;
        crate::fault::point("project.attach.record.after");
        self.publish(project)?;
        crate::fault::point("project.attach.index.after");
        Ok(())
    }

    fn observe(
        &self,
        store: &mut Store,
        id: OperationId,
        result: &Result<(), SupervisorError>,
    ) -> Result<(), SupervisorError> {
        record_operation_reconciliation(
            store,
            id,
            if result.is_ok() {
                ExternalResourceState::Observed
            } else {
                ExternalResourceState::Unknown
            },
            result.as_ref().err().map(ToString::to_string),
            unix_timestamp()?,
        )
    }
}

fn attachment_failure(error: SupervisorError) -> RpcFailure {
    match error {
        SupervisorError::Config(_) | SupervisorError::ConfigLock(_) => {
            conflict(error.to_string())
        }
        SupervisorError::Project(
            ProjectError::Canonicalize { .. }
            | ProjectError::NotDirectory { .. }
            | ProjectError::BareRepository { .. }
            | ProjectError::GitDiscovery { .. },
        ) => invalid_argument(error.to_string()),
        error => supervisor_rpc_failure(error),
    }
}

fn root_allowed(project: &Path, allowed: &[PathBuf]) -> bool {
    allowed.iter().any(|root| project.starts_with(root))
}

fn validate_restrictions(
    project: &DiscoveredProject,
    policy: &EffectiveConfig,
) -> Result<(), RpcFailure> {
    let needs_validation =
        ["coterie.toml", "coterie.lock"].iter().any(|name| {
            !matches!(project.canonical_path.join(name).symlink_metadata(), Err(error) if error.kind() == io::ErrorKind::NotFound)
        });
    if needs_validation {
        let configured = load_configuration_with_overrides(
            project,
            &crate::cli::config::Overrides::default(),
        )
        .map_err(attachment_failure)?;
        if !RunConfiguration::new(policy.clone())
            .differences(&configured)
            .is_empty()
        {
            return Err(conflict(
                "attached project configuration differs from the active run; per-project restriction overlays are not yet supported",
            ));
        }
    }
    Ok(())
}

pub(super) fn list(
    store: &mut Store,
    run_id: RunId,
) -> Result<RpcResponse, RpcFailure> {
    let projects = store
        .transaction(|repositories| repositories.projects(run_id))
        .map_err(rpc_state_failure)?;
    Ok(RpcResponse::Projects {
        projects: projects.into_iter().map(project_summary).collect(),
    })
}

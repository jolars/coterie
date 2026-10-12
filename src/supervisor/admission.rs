//! Admission observations share the counters used by launch enforcement.

use super::*;
use crate::protocol::admission::{
    AdmissionBudget, AdmissionView, CapacityRelease, PolicySource,
    RoleAdmission,
};
use crate::protocol::context::TextPreview;

pub(super) const GUIDANCE: &str = "Worker exit releases active concurrency and role slots, never lifetime admissions. Spawn rate slots expire after the rolling minute. Recovery itself admits no agent; each fresh continuation or review spawn consumes another lifetime and rate slot. Inspect prime.admission before recovery or review. If authority is missing, ask an authorized coordinator or the operator. An exhausted lifetime budget requires the operator to preserve work, stop the run, and start a new run with suitable trusted policy, carrying retained work through explicit review, integration, validation, and acceptance. Editing configuration cannot increase an active snapshot.";

fn source(
    configuration: &EffectiveConfig,
    field: &str,
) -> Option<PolicySource> {
    configuration
        .provenance
        .get(field)
        .map(|value| PolicySource {
            layer: value.source.layer,
            field: TextPreview::new(&value.source.field),
        })
}

fn budget(
    configuration: &EffectiveConfig,
    field: &str,
    used: u64,
    limit: Option<u16>,
    released_by: CapacityRelease,
) -> AdmissionBudget {
    AdmissionBudget {
        used,
        limit,
        remaining: limit.map(|limit| u64::from(limit).saturating_sub(used)),
        exhausted: limit.is_some_and(|limit| used >= u64::from(limit)),
        released_by,
        source: source(configuration, field),
    }
}

pub(super) fn role_budget(
    configuration: &EffectiveConfig,
    agents: &[AgentRecord],
    role: &str,
) -> AdmissionBudget {
    budget(
        configuration,
        &format!("roles.{role}.max_instances"),
        agents
            .iter()
            .filter(|agent| agent.role == role && !agent.state.is_terminal())
            .count() as u64,
        configuration.roles[role].max_instances,
        CapacityRelease::AgentTerminal,
    )
}

pub(super) fn counters(
    store: &mut Store,
    run_id: RunId,
    now: i64,
) -> Result<(EffectiveConfig, Vec<AgentRecord>, AdmissionView), RpcFailure> {
    let (configuration, agents, recent) = store
        .transaction(|r| {
            Ok((
                r.configuration(run_id)?,
                r.agents(run_id)?,
                r.recent_spawns(run_id, now)?,
            ))
        })
        .map_err(rpc_state_failure)?;
    let limits = configuration.limits;
    let concurrent_agents = budget(
        &configuration,
        "limits.max_concurrent_agents",
        agents
            .iter()
            .filter(|agent| !agent.state.is_terminal())
            .count() as u64,
        Some(limits.max_concurrent_agents),
        CapacityRelease::AgentTerminal,
    );
    let agents_per_run = budget(
        &configuration,
        "limits.max_agents_per_run",
        agents.len() as u64,
        Some(limits.max_agents_per_run),
        CapacityRelease::Never,
    );
    let spawns_per_minute = budget(
        &configuration,
        "limits.max_spawns_per_minute",
        recent as u64,
        Some(limits.max_spawns_per_minute),
        CapacityRelease::RollingMinute,
    );
    let warning = agents_per_run.remaining.filter(|remaining| *remaining <= 2).map(|remaining| format!("Only {remaining} lifetime admission(s) remain. Reserve budget for fresh recovery continuations and reviews; worker exit does not restore it. {GUIDANCE}"));
    Ok((
        configuration,
        agents,
        AdmissionView {
            observed_at: now,
            concurrent_agents,
            agents_per_run,
            spawns_per_minute,
            roles: Vec::new(),
            omitted_roles: 0,
            warning,
            guidance: GUIDANCE.into(),
        },
    ))
}

pub(super) fn view(
    store: &mut Store,
    run_id: RunId,
    caller: &AuthenticatedCaller,
) -> Result<Option<AdmissionView>, RpcFailure> {
    require_current_caller(store, run_id, caller)?;
    let (configuration, agents, mut view) =
        counters(store, run_id, rpc_timestamp()?)?;
    let caller_role = caller
        .agent_id()
        .map(|id| agent_record(store, run_id, id).map(|agent| agent.role))
        .transpose()?;
    let allowed = |role: &str| {
        caller_role.as_ref().is_none_or(|caller_role| {
            configuration
                .archetype
                .authorize(caller_role, Capability::new("spawn", role))
                == AuthorizationDecision::Allowed
        })
    };
    let mut authorized = false;
    for (role, definition) in &configuration.archetype.roles {
        if !allowed(role) {
            continue;
        }
        authorized = true;
        if definition.mode != RoleMode::Job {
            continue;
        }
        if view.roles.len() == 16 {
            view.omitted_roles += 1;
            continue;
        }
        view.roles.push(RoleAdmission {
            role: TextPreview::new(role),
            enabled: configuration.roles[role].enabled,
            enabled_source: source(
                &configuration,
                &format!("roles.{role}.enabled"),
            ),
            active_instances: role_budget(&configuration, &agents, role),
        });
        if serde_json::to_vec(&view)
            .map_err(|error| rpc_state_failure(error.into()))?
            .len()
            > 32 * 1024
        {
            view.roles.pop();
            view.omitted_roles += 1;
        }
    }
    while serde_json::to_vec(&view)
        .map_err(|error| rpc_state_failure(error.into()))?
        .len()
        > 32 * 1024
    {
        if view.roles.pop().is_none() {
            return Err(RpcFailure::new(
                RpcFailureCode::Internal,
                "fixed admission metadata exceeds its byte budget",
            ));
        }
        view.omitted_roles += 1;
    }
    Ok(authorized.then_some(view))
}

pub(super) fn exhausted(name: &str, budget: &AdmissionBudget) -> RpcFailure {
    let source = budget.source.as_ref().map_or_else(
        || "unknown historical source".into(),
        |source| format!("{}:{}", source.layer, source.field.text),
    );
    let action = match budget.released_by {
        CapacityRelease::AgentTerminal => {
            "Wait for a recorded terminal agent state; unknown states still consume active slots."
        }
        CapacityRelease::RollingMinute => {
            "Retry after explicit spawns leave the rolling 60-second window; worker exit does not release these slots."
        }
        CapacityRelease::Never => GUIDANCE,
    };
    conflict(format!(
        "admission limit `{name}` exhausted: used {}, ceiling {}, source {source}. {action}",
        budget.used,
        budget.limit.expect("only bounded budgets can be exhausted")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Directory(PathBuf);
    impl Directory {
        fn new() -> Self {
            let path = std::env::temp_dir()
                .join(format!("coterie-admission-{}", RunId::generate()));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn fixture(config: &EffectiveConfig) -> (Directory, Store, RunId) {
        let directory = Directory::new();
        let mut store =
            Store::open(&directory.0.join("state.sqlite3")).unwrap();
        let run_id = RunId::generate();
        store
            .transaction(|r| {
                r.insert_run(&RunRecord {
                    id: run_id,
                    status: "active".into(),
                    created_at: 1,
                    stopped_at: None,
                })?;
                r.snapshot_configuration(run_id, config, 1)
            })
            .unwrap();
        (directory, store, run_id)
    }
    fn config() -> EffectiveConfig {
        crate::config::resolve(
            &Default::default(),
            &Default::default(),
            &Default::default(),
        )
        .unwrap()
    }
    fn prime(
        store: &mut Store,
        run: RunId,
        caller: &AuthenticatedCaller,
    ) -> Result<crate::protocol::context::PrimePage, RpcFailure> {
        let RpcResponse::Prime { page } =
            context::prime(store, run, caller, None, 20)?
        else {
            panic!("prime");
        };
        Ok(page)
    }
    fn caller(store: &mut Store, run: RunId) -> SessionScope {
        let RpcResponse::ForegroundPrepared {
            agent,
            session_id,
            generation,
            ..
        } = launch_foreground(
            store,
            run,
            &AuthenticatedCaller::Operator,
            OperationId::generate(),
            &AgentToken::generate().unwrap(),
        )
        .unwrap()
        else {
            panic!("foreground");
        };
        SessionScope {
            run_id: run,
            agent_id: agent.id,
            session_id,
            generation,
        }
    }

    #[test]
    fn prime_reports_admission_before_any_worker_is_created() {
        let (_directory, mut store, run) = fixture(&config());
        let page =
            prime(&mut store, run, &AuthenticatedCaller::Operator).unwrap();
        let capacity = page.admission.unwrap();
        assert_eq!(capacity.agents_per_run.used, 0);
        assert_eq!(capacity.agents_per_run.limit, Some(16));
        assert_eq!(
            capacity.agents_per_run.source.unwrap().layer,
            crate::config::ConfigLayer::Compiled
        );
        assert!(capacity.warning.is_none());
    }

    #[test]
    fn admission_authority_follows_capabilities_and_rechecks_generation() {
        for grants in [vec!["spawn:worker"], vec!["task:read"], vec!["spawn:*"]]
        {
            let mut config = config();
            let mut archetype =
                serde_json::to_value(&config.archetype).unwrap();
            archetype["roles"]["lead"]["capabilities"] = json!(
                grants
                    .iter()
                    .map(|grant| {
                        let (namespace, action) =
                            grant.split_once(':').unwrap();
                        json!({"namespace": namespace, "action": action})
                    })
                    .collect::<Vec<_>>()
            );
            config.archetype = serde_json::from_value(archetype).unwrap();
            let (_directory, mut store, run) = fixture(&config);
            let scope = caller(&mut store, run);
            let page =
                prime(&mut store, run, &AuthenticatedCaller::Agent(scope))
                    .unwrap();
            if grants == vec!["task:read"] {
                assert!(page.admission.is_none());
            } else {
                let capacity = page.admission.unwrap();
                assert_eq!(capacity.agents_per_run.used, 1);
                let roles: Vec<_> = capacity
                    .roles
                    .iter()
                    .map(|r| r.role.text.as_str())
                    .collect();
                assert_eq!(
                    roles,
                    if grants == vec!["spawn:worker"] {
                        vec!["worker"]
                    } else {
                        vec!["reviewer", "worker"]
                    }
                );
            }
            let stale = SessionScope {
                generation: scope.generation + 1,
                ..scope
            };
            assert_eq!(
                prime(&mut store, run, &AuthenticatedCaller::Agent(stale))
                    .unwrap_err()
                    .code,
                RpcFailureCode::Unauthenticated
            );
            store
                .transaction(|r| {
                    r.record_session_lifecycle(
                        scope,
                        LifecycleState::Lost,
                        rpc_timestamp().unwrap(),
                    )
                })
                .unwrap();
            assert_eq!(
                prime(&mut store, run, &AuthenticatedCaller::Agent(scope))
                    .unwrap_err()
                    .code,
                RpcFailureCode::Unauthenticated
            );
        }
    }

    #[test]
    fn admission_foreground_replacement_preserves_lifetime_usage_and_fences_old_scope()
     {
        let mut config = config();
        config.limits.max_agents_per_run = 1;
        config.limits.max_concurrent_agents = 1;
        let (_directory, mut store, run) = fixture(&config);
        let initial = caller(&mut store, run);
        store
            .transaction(|r| {
                r.record_session_lifecycle(
                    initial,
                    LifecycleState::Lost,
                    rpc_timestamp().unwrap(),
                )
            })
            .unwrap();
        let replacement = caller(&mut store, run);
        assert_eq!(replacement.agent_id, initial.agent_id);
        assert_eq!(replacement.generation, initial.generation + 1);
        let view =
            prime(&mut store, run, &AuthenticatedCaller::Agent(replacement))
                .unwrap()
                .admission
                .unwrap();
        assert_eq!(view.agents_per_run.used, 1);
        assert_eq!(view.concurrent_agents.used, 1);
        assert_eq!(view.spawns_per_minute.used, 0);
        assert!(view.warning.is_some());
        assert_eq!(
            prime(&mut store, run, &AuthenticatedCaller::Agent(initial))
                .unwrap_err()
                .code,
            RpcFailureCode::Unauthenticated
        );
    }

    #[test]
    fn admission_unknown_states_consume_active_slots_and_missing_provenance_stays_unknown()
     {
        let mut config = config();
        config.provenance.clear();
        config.roles.get_mut("worker").unwrap().max_instances = None;
        let (_directory, mut store, run) = fixture(&config);
        for state in [
            LifecycleState::Starting,
            LifecycleState::Running,
            LifecycleState::Unknown,
            LifecycleState::Exited,
            LifecycleState::Lost,
            LifecycleState::Quarantined,
        ] {
            store
                .transaction(|r| {
                    r.insert_agent(&AgentRecord {
                        id: AgentId::generate(),
                        run_id: run,
                        role: "worker".into(),
                        generation: 0,
                        state,
                        created_at: 1,
                    })
                })
                .unwrap();
        }
        let capacity = view(&mut store, run, &AuthenticatedCaller::Operator)
            .unwrap()
            .unwrap();
        assert_eq!(capacity.concurrent_agents.used, 3);
        assert_eq!(capacity.agents_per_run.used, 6);
        assert!(capacity.agents_per_run.source.is_none());
        let worker = capacity
            .roles
            .iter()
            .find(|r| r.role.text == "worker")
            .unwrap();
        assert_eq!(worker.active_instances.used, 3);
        assert!(worker.active_instances.limit.is_none());
        assert!(worker.active_instances.remaining.is_none());
        assert!(!worker.active_instances.exhausted);
    }

    #[test]
    fn admission_is_bounded_even_with_many_long_roles_and_sources() {
        let mut config = config();
        let role = config.archetype.roles["worker"].clone();
        let effective = config.roles["worker"].clone();
        for index in 0..40 {
            let name = format!("r{index:02}{}", "x".repeat(1000));
            config.archetype.roles.insert(name.clone(), role.clone());
            config.roles.insert(name.clone(), effective.clone());
            for field in ["enabled", "max_instances"] {
                let mut origin =
                    config.provenance[&format!("roles.worker.{field}")].clone();
                origin.source.file =
                    Some(PathBuf::from("/private/operator/config.toml"));
                origin.source.field = "\u{0001}".repeat(1000);
                config
                    .provenance
                    .insert(format!("roles.{name}.{field}"), origin);
            }
        }
        let (_directory, mut store, run) = fixture(&config);
        let capacity = view(&mut store, run, &AuthenticatedCaller::Operator)
            .unwrap()
            .unwrap();
        assert_eq!(capacity.roles.len() + capacity.omitted_roles, 42);
        assert!(capacity.roles.len() <= 16);
        let encoded = serde_json::to_vec(&capacity).unwrap();
        assert!(encoded.len() <= 32 * 1024, "{}", encoded.len());
        assert!(
            !String::from_utf8(encoded)
                .unwrap()
                .contains("/private/operator")
        );
        assert!(capacity.roles.iter().any(|r| r.role.truncated));
    }

    #[test]
    fn admission_rolling_rate_expires_at_sixty_seconds_and_replay_does_not_consume_twice()
     {
        let (_directory, mut store, run) = fixture(&config());
        let mutation = Mutation {
            id: OperationId::generate(),
            run_id: run,
            actor_agent_id: None,
            kind: "agent.spawn".into(),
            request: json!({"role": "worker"}),
            created_at: 100,
        };
        store
            .mutate(&mutation, |_| Ok(json!({"recorded": true})))
            .unwrap();
        store
            .mutate::<serde_json::Value>(&mutation, |_| panic!("replay"))
            .unwrap();
        assert_eq!(
            counters(&mut store, run, 159)
                .unwrap()
                .2
                .spawns_per_minute
                .used,
            1
        );
        assert_eq!(
            counters(&mut store, run, 160)
                .unwrap()
                .2
                .spawns_per_minute
                .used,
            0
        );
    }
}

//! The supervisor owns delivery intent; the foreground owner performs effects.

use super::*;
use crate::protocol::notifications::{NotificationAvailability, QueueOutcome};

pub(super) fn availability(
    store: &mut Store,
    caller: &AuthenticatedCaller,
) -> Result<NotificationAvailability, RpcFailure> {
    let AuthenticatedCaller::Agent(scope) = caller else {
        return Ok(NotificationAvailability::Unavailable);
    };
    let now = rpc_timestamp()?;
    store
        .transaction(|r| r.notification_availability(*scope, now))
        .map_err(rpc_state_failure)
}

pub(super) fn execute(
    store: &mut Store,
    run_id: RunId,
    caller: &AuthenticatedCaller,
    request: RpcRequest,
    peer_pid: Option<u32>,
) -> Result<RpcResponse, RpcFailure> {
    let (scope, operation_id) = match &request {
        RpcRequest::BindForegroundNotifications {
            operation_id,
            thread_id,
        } => {
            let AuthenticatedCaller::Agent(scope) = caller else {
                return Err(RpcFailure::new(
                    RpcFailureCode::PermissionDenied,
                    "only a provider-launched MCP bridge can bind notifications",
                ));
            };
            if !crate::protocol::notifications::valid_thread_id(thread_id) {
                return Err(invalid_argument(
                    "the provider thread ID must be a canonical UUID",
                ));
            }
            let identity = store
                .transaction(|r| {
                    r.foreground_identity(run_id, scope.session_id)
                })
                .map_err(rpc_state_failure)?;
            if !identity.is_some_and(|identity| {
                peer_pid.is_some_and(|pid| identity.owns_bridge(pid))
            }) {
                return Err(RpcFailure::new(
                    RpcFailureCode::PermissionDenied,
                    "notification metadata must originate from this foreground provider's host MCP bridge",
                ));
            }
            (*scope, Some(*operation_id))
        }
        RpcRequest::EnableForegroundNotifications {
            scope,
            operation_id,
        }
        | RpcRequest::ClaimForegroundNotification {
            scope,
            operation_id,
        }
        | RpcRequest::ObserveForegroundNotification {
            scope,
            operation_id,
            ..
        } => {
            require_operator(
                caller,
                "notification delivery belongs to the foreground process owner",
            )?;
            (*scope, Some(*operation_id))
        }
        RpcRequest::ForegroundNotificationPending { scope } => {
            require_operator(
                caller,
                "notification delivery belongs to the foreground process owner",
            )?;
            (*scope, None)
        }
        _ => return Err(invalid_argument("unsupported notification request")),
    };
    require_foreground_scope(store, run_id, scope)?;
    let role = agent_record(store, run_id, scope.agent_id)?.role;
    let progress = store
        .configuration(run_id)
        .map_err(rpc_state_failure)?
        .archetype
        .authorize(&role, Capability::new("task", "read"))
        == AuthorizationDecision::Allowed;
    let now = rpc_timestamp()?;
    let Some(operation_id) = operation_id else {
        return store
            .transaction(|r| {
                Ok(RpcResponse::ForegroundNotifications {
                    availability: r.notification_availability(scope, now)?,
                    pending: r.notification_pending(scope, progress)?,
                })
            })
            .map_err(rpc_state_failure);
    };
    let mutation = Mutation {
        id: operation_id,
        run_id,
        kind: "foreground.notification".into(),
        actor_agent_id: caller.agent_id(),
        request: serde_json::to_value(&request)
            .map_err(|error| rpc_state_failure(error.into()))?,
        created_at: now,
    };
    let result = store.mutate(&mutation, |r| {
        match &request {
            RpcRequest::EnableForegroundNotifications { .. } => r.enable_notifications(scope)?,
            RpcRequest::BindForegroundNotifications { thread_id, .. } => {
                if !r.bind_notifications(scope, thread_id)? {
                    return Err(StoreError::CorruptAgentState { id: scope.agent_id, reason: "notification binding is unavailable or already belongs to another provider thread".into() });
                }
            }
            RpcRequest::ClaimForegroundNotification { .. } => return Ok(RpcResponse::ForegroundNotificationClaimed {
                claim: r.claim_notification(scope, operation_id, progress, now)?,
            }),
            RpcRequest::ObserveForegroundNotification { delivery_id, outcome, .. } => r.observe_notification(scope, *delivery_id, *outcome, now)?,
            _ => unreachable!(),
        }
        Ok(RpcResponse::ForegroundNotifications { availability: r.notification_availability(scope, now)?, pending: false })
    }).map_err(rpc_state_failure)?;
    Ok(mutation_value(result))
}

pub(super) async fn deliver(
    project: &DiscoveredProject,
    directories: &CoterieDirectories,
    scope: SessionScope,
    overrides: &crate::cli::config::Overrides,
    queue: &crate::providers::notifications::CodexQueue,
) {
    let mut client = None;
    let mut pending = None;
    loop {
        sleep(Duration::from_millis(500)).await;
        if client.is_none() {
            let Some(entry) = ActiveRunIndex::new(directories)
                .lookup(&project.identity)
                .ok()
                .flatten()
                .filter(|e| e.run_id == scope.run_id)
            else {
                continue;
            };
            if let Ok(mut connection) =
                connect_for_observation(project, directories, &entry, overrides)
                    .await
                && connection.run_id() == scope.run_id
            {
                // Only the wrapper retaining the unreaped child can restore
                // this observation after a supervisor loses its process state.
                let Some(identity) = queue.live_identity() else {
                    return;
                };
                if connection
                    .request(RpcRequest::ForegroundStarted {
                        scope,
                        process_id: identity.process_id,
                        identity: Some(identity),
                    })
                    .await
                    .is_ok()
                {
                    client = Some(connection);
                }
            }
        }
        let Some(connection) = &mut client else {
            continue;
        };
        let request = pending
            .get_or_insert(RpcRequest::ForegroundNotificationPending { scope });
        let response = match connection.request(request.clone()).await {
            Ok(value) => value,
            Err(error) if error.is_transient_connection_failure() => {
                client = None;
                continue;
            }
            Err(error) => {
                eprintln!(
                    "Coterie automatic notification delivery stopped: {}",
                    crate::redaction::text(&error.to_string())
                );
                return;
            }
        };
        pending = None;
        match response {
            RpcResponse::ForegroundNotifications {
                availability: NotificationAvailability::Uncertain,
                ..
            } => {
                eprintln!(
                    "Coterie could not confirm foreground notification delivery. Check the inbox and progress manually; automatic delivery is suspended for this session."
                );
                return;
            }
            RpcResponse::ForegroundNotifications { pending: true, .. } => {
                pending = Some(RpcRequest::ClaimForegroundNotification {
                    operation_id: OperationId::generate(),
                    scope,
                });
            }
            RpcResponse::ForegroundNotificationClaimed {
                claim: Some(claim),
            } => {
                pending = Some(attempt(queue, scope, &claim).await);
            }
            _ => (),
        }
    }
}

pub(super) async fn attempt(
    queue: &crate::providers::notifications::CodexQueue,
    scope: SessionScope,
    claim: &crate::protocol::notifications::NotificationClaim,
) -> RpcRequest {
    crate::fault::point("notification.intent.committed");
    let outcome = queue.deliver(scope, claim).await;
    crate::fault::point("notification.effect.observed");
    if outcome != QueueOutcome::Accepted {
        eprintln!(
            "Coterie could not confirm a queued foreground notification; recording the uncertain delivery."
        );
    }
    RpcRequest::ObserveForegroundNotification {
        operation_id: OperationId::generate(),
        scope,
        delivery_id: claim.operation_id,
        outcome,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notification_rpcs_do_not_accept_agent_selected_destinations_or_effects()
    {
        let mut store = Store::open_in_memory().unwrap();
        let scope = SessionScope {
            run_id: RunId::generate(),
            agent_id: AgentId::generate(),
            session_id: SessionId::generate(),
            generation: 1,
        };
        let caller = AuthenticatedCaller::Agent(scope);
        for request in [
            RpcRequest::BindForegroundNotifications {
                operation_id: OperationId::generate(),
                thread_id: "01234567-89ab-cdef-0123-456789abcdef".into(),
            },
            RpcRequest::EnableForegroundNotifications {
                operation_id: OperationId::generate(),
                scope,
            },
            RpcRequest::ClaimForegroundNotification {
                operation_id: OperationId::generate(),
                scope,
            },
            RpcRequest::ObserveForegroundNotification {
                operation_id: OperationId::generate(),
                delivery_id: OperationId::generate(),
                scope,
                outcome: QueueOutcome::Accepted,
            },
        ] {
            assert_eq!(
                execute(&mut store, scope.run_id, &caller, request, None)
                    .unwrap_err()
                    .code,
                RpcFailureCode::PermissionDenied
            );
        }
        assert_eq!(
            execute(
                &mut store,
                scope.run_id,
                &AuthenticatedCaller::Operator,
                RpcRequest::BindForegroundNotifications {
                    operation_id: OperationId::generate(),
                    thread_id: "01234567-89ab-cdef-0123-456789abcdef".into()
                },
                None,
            )
            .unwrap_err()
            .code,
            RpcFailureCode::PermissionDenied
        );
    }
}

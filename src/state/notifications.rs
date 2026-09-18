//! Durable, generation-fenced foreground notification delivery.

use super::*;
use crate::auth::SessionScope;
use crate::protocol::notifications::{
    NotificationAvailability, NotificationClaim, QueueOutcome,
};

impl Repositories<'_, '_> {
    pub(crate) fn enable_notifications(
        &self,
        scope: SessionScope,
    ) -> Result<(), StoreError> {
        if !self.session_scope_is_current(scope)? {
            return Ok(());
        }
        self.transaction.execute(
            "INSERT INTO foreground_notifications (session_id, run_id, agent_id, generation, event_cursor)
             VALUES (?1, ?2, ?3, ?4, (SELECT COALESCE(MAX(sequence), 0) FROM events WHERE run_id = ?2))
             ON CONFLICT(session_id) DO NOTHING",
            params![scope.session_id, scope.run_id, scope.agent_id, scope.generation],
        )?;
        Ok(())
    }

    pub(crate) fn notifications_live(
        &self,
        scope: SessionScope,
    ) -> Result<bool, StoreError> {
        Ok(self.session_scope_is_current(scope)?
            && self.run_shutdown(scope.run_id)?.is_none()
            && !self
                .session_controls(scope.run_id)?
                .iter()
                .any(|c| c.scope == scope)
            && self.session(scope.session_id)?.is_some_and(|s| {
                s.process_owner == SessionProcessOwner::Foreground
                    && s.state == LifecycleState::Running
            }))
    }

    pub(crate) fn bind_notifications(
        &self,
        scope: SessionScope,
        thread: &str,
    ) -> Result<bool, StoreError> {
        if !self.notifications_live(scope)?
            || !crate::protocol::notifications::valid_thread_id(thread)
        {
            return Ok(false);
        }
        Ok(self.transaction.execute(
            "UPDATE foreground_notifications SET thread_id = ?2 WHERE session_id = ?1 AND (thread_id IS NULL OR thread_id = ?2)",
            params![scope.session_id, thread],
        )? == 1)
    }

    pub(crate) fn notification_availability(
        &self,
        scope: SessionScope,
        now: i64,
    ) -> Result<NotificationAvailability, StoreError> {
        if !self.notifications_live(scope)? {
            return Ok(NotificationAvailability::Unavailable);
        }
        let binding: Option<Option<String>> = self.transaction.query_row(
            "SELECT thread_id FROM foreground_notifications WHERE session_id = ?1", [scope.session_id], |row| row.get(0),
        ).optional()?;
        Ok(match binding {
            None => NotificationAvailability::Unavailable,
            Some(None) => NotificationAvailability::PendingBinding,
            Some(Some(_)) => {
                let uncertain: bool = self.transaction.query_row(
                    "SELECT EXISTS(SELECT 1 FROM notification_deliveries WHERE session_id = ?1 AND
                        (receipt_state = 'legacy' OR state IN ('failed', 'unknown') OR (state = 'attempting' AND created_at < ?2)))",
                    params![scope.session_id, now.saturating_sub(15)], |row| row.get(0),
                )?;
                if uncertain {
                    NotificationAvailability::Uncertain
                } else {
                    NotificationAvailability::Automatic
                }
            }
        })
    }

    fn pending_notification(
        &self,
        scope: SessionScope,
        progress: bool,
    ) -> Result<Option<(String, i64, i64)>, StoreError> {
        if !self.notifications_live(scope)? {
            return Ok(None);
        }
        let binding: Option<(Option<String>, i64, i64)> = self.transaction.query_row(
            "SELECT thread_id, event_cursor, message_cursor FROM foreground_notifications WHERE session_id = ?1
                AND NOT EXISTS(SELECT 1 FROM notification_deliveries WHERE session_id = ?1
                    AND (state <> 'accepted' OR receipt_state <> 'received'))",
            [scope.session_id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        ).optional()?;
        let Some((Some(thread), event_cursor, message_cursor)) = binding else {
            return Ok(None);
        };
        let messages: i64 = self.transaction.query_row(
            "SELECT COALESCE(MAX(sequence), ?3) FROM messages WHERE run_id = ?1 AND recipient_agent_id = ?2 AND sequence > ?3 AND acknowledged_at IS NULL",
            params![scope.run_id, scope.agent_id, message_cursor], |row| row.get(0),
        )?;
        // Only external lifecycle changes can wake a coordinator. Its own task
        // creation and closure must not create a notification feedback loop.
        let events: i64 = if progress {
            self.transaction.query_row(
            "SELECT COALESCE(MAX(sequence), ?3) FROM events WHERE run_id = ?1 AND sequence > ?3 AND actor <> ?2
                AND (agent_id IS NULL OR agent_id <> ?2) AND event_type IN
                ('task.lifecycle_changed', 'assignment.lifecycle_changed', 'session.lifecycle_changed', 'task.recovered')",
            params![scope.run_id, scope.agent_id, event_cursor], |row| row.get(0),
        )?
        } else {
            event_cursor
        };
        Ok((messages > message_cursor || events > event_cursor)
            .then_some((thread, events, messages)))
    }

    pub(crate) fn notification_pending(
        &self,
        scope: SessionScope,
        progress: bool,
    ) -> Result<bool, StoreError> {
        Ok(self.pending_notification(scope, progress)?.is_some())
    }

    pub(crate) fn claim_notification(
        &self,
        scope: SessionScope,
        id: OperationId,
        progress: bool,
        now: i64,
    ) -> Result<Option<NotificationClaim>, StoreError> {
        if !self.notifications_live(scope)? {
            return Ok(None);
        }
        if let Some(thread) = self.transaction.query_row(
            "SELECT n.thread_id FROM notification_deliveries AS d JOIN foreground_notifications AS n ON n.session_id = d.session_id
                WHERE d.operation_id = ?1 AND d.session_id = ?2", params![id, scope.session_id], |row| row.get(0),
        ).optional()? { return Ok(Some(NotificationClaim { operation_id: id, thread_id: thread })); }
        let Some((thread_id, events, messages)) =
            self.pending_notification(scope, progress)?
        else {
            return Ok(None);
        };
        self.transaction.execute(
            "INSERT INTO notification_deliveries (operation_id, session_id, event_cursor, message_cursor, state, created_at)
                VALUES (?1, ?2, ?3, ?4, 'attempting', ?5)", params![id, scope.session_id, events, messages, now],
        )?;
        self.transaction.execute("UPDATE foreground_notifications SET event_cursor = ?2, message_cursor = ?3 WHERE session_id = ?1", params![scope.session_id, events, messages])?;
        Ok(Some(NotificationClaim {
            operation_id: id,
            thread_id,
        }))
    }

    pub(crate) fn observe_notification(
        &self,
        scope: SessionScope,
        id: OperationId,
        outcome: QueueOutcome,
        now: i64,
    ) -> Result<(), StoreError> {
        if !self.session_scope_is_current(scope)? {
            return Ok(());
        }
        self.transaction.execute(
            "UPDATE notification_deliveries SET state = ?3, observed_at = ?4 WHERE operation_id = ?1 AND session_id = ?2 AND state = 'attempting'",
            params![id, scope.session_id, outcome.as_str(), now],
        )?;
        Ok(())
    }

    pub(crate) fn receive_notification(
        &self,
        scope: SessionScope,
        delivery_id: OperationId,
        now: i64,
    ) -> Result<bool, StoreError> {
        if !self.notifications_live(scope)? {
            return Ok(false);
        }
        let receipt: Option<String> = self.transaction.query_row(
            "SELECT receipt_state FROM notification_deliveries WHERE operation_id = ?1
                AND session_id = ?2 AND state IN ('attempting', 'accepted')",
            params![delivery_id, scope.session_id], |row| row.get(0),
        ).optional()?;
        match receipt.as_deref() {
            Some("received") => return Ok(true),
            Some("pending") => (),
            _ => return Ok(false),
        }
        self.transaction.execute(
            "UPDATE notification_deliveries SET receipt_state = 'received', received_at = ?2 WHERE operation_id = ?1",
            params![delivery_id, now],
        )?;
        // The recipient polls after receipt. That read covers updates that
        // arrived while this notice waited in the provider's queue. Replayed
        // receipts must never consume updates belonging to a later notice.
        self.transaction.execute(
            "UPDATE foreground_notifications SET
                event_cursor = (SELECT COALESCE(MAX(sequence), 0) FROM events WHERE run_id = ?2),
                message_cursor = (SELECT COALESCE(MAX(sequence), 0) FROM messages WHERE run_id = ?2 AND recipient_agent_id = ?3)
                WHERE session_id = ?1",
            params![scope.session_id, scope.run_id, scope.agent_id],
        )?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use crate::auth::SessionScope;
    use crate::protocol::notifications::{
        NotificationAvailability, QueueOutcome,
    };
    use crate::state::*;

    fn fixture() -> (Store, SessionScope) {
        let mut store = Store::open_in_memory().unwrap();
        let scope = SessionScope {
            run_id: RunId::generate(),
            agent_id: AgentId::generate(),
            session_id: SessionId::generate(),
            generation: 1,
        };
        store
            .transaction(|r| {
                r.insert_test_run(&RunRecord {
                    id: scope.run_id,
                    status: "active".into(),
                    created_at: 1,
                    stopped_at: None,
                })?;
                r.insert_agent(&AgentRecord {
                    id: scope.agent_id,
                    run_id: scope.run_id,
                    role: "custom".into(),
                    generation: 1,
                    state: LifecycleState::Running,
                    created_at: 1,
                })?;
                r.insert_session(&SessionRecord {
                    id: scope.session_id,
                    run_id: scope.run_id,
                    agent_id: scope.agent_id,
                    generation: 1,
                    provider: "codex".into(),
                    provider_session_id: Some("process:123".into()),
                    reconciliation_state: ExternalResourceState::Observed,
                    state: LifecycleState::Running,
                    transcript_path: "transcript".into(),
                    created_at: 1,
                    ended_at: None,
                    reconciled_at: Some(1),
                    process_owner: SessionProcessOwner::Foreground,
                })?;
                r.enable_notifications(scope)?;
                assert!(r.bind_notifications(
                    scope,
                    "01234567-89ab-cdef-0123-456789abcdef"
                )?);
                Ok(())
            })
            .unwrap();
        (store, scope)
    }

    fn message(store: &mut Store, scope: SessionScope, sequence: i64) {
        store
            .transaction(|r| {
                r.insert_message(&MessageRecord {
                    id: MessageId::generate(),
                    run_id: scope.run_id,
                    sender_agent_id: None,
                    recipient_agent_id: scope.agent_id,
                    sequence,
                    body: "untrusted worker text".into(),
                    created_at: 2,
                    acknowledged_at: None,
                })
            })
            .unwrap();
    }

    #[test]
    fn notification_claim_coalesces_messages_and_never_acknowledges_them() {
        let (mut store, scope) = fixture();
        message(&mut store, scope, 1);
        message(&mut store, scope, 2);
        let id = OperationId::generate();
        store
            .transaction(|r| {
                let claim = r.claim_notification(scope, id, false, 3)?.unwrap();
                assert_eq!(claim.operation_id, id);
                assert_eq!(
                    r.claim_notification(scope, id, false, 3)?,
                    Some(claim)
                );
                assert!(
                    r.claim_notification(
                        scope,
                        OperationId::generate(),
                        false,
                        3
                    )?
                    .is_none()
                );
                r.observe_notification(scope, id, QueueOutcome::Accepted, 4)?;
                assert!(
                    r.claim_notification(
                        scope,
                        OperationId::generate(),
                        false,
                        5
                    )?
                    .is_none()
                );
                assert!(
                    r.messages_after(scope.run_id, scope.agent_id, 0)?
                        .iter()
                        .all(|m| m.acknowledged_at.is_none())
                );
                Ok(())
            })
            .unwrap();
        message(&mut store, scope, 3);
        store
            .transaction(|r| {
                assert!(r.receive_notification(scope, id, 6)?);
                assert!(!r.notification_pending(scope, false)?);
                Ok(())
            })
            .unwrap();
        message(&mut store, scope, 4);
        assert!(
            store
                .transaction(|r| r.claim_notification(
                    scope,
                    OperationId::generate(),
                    false,
                    6
                ))
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn accepted_notification_bounds_the_provider_backlog_until_receipt() {
        let (mut store, scope) = fixture();
        message(&mut store, scope, 1);
        let delivery = OperationId::generate();
        store
            .transaction(|r| {
                assert!(
                    r.claim_notification(scope, delivery, true, 3)?.is_some()
                );
                r.observe_notification(
                    scope,
                    delivery,
                    QueueOutcome::Accepted,
                    4,
                )
            })
            .unwrap();
        // The foreground can read updates while its queued notice is still
        // waiting for the current provider turn to finish.
        for sequence in 2..=5 {
            message(&mut store, scope, sequence);
            store
                .transaction(|r| {
                    assert_eq!(
                        r.messages_after(scope.run_id, scope.agent_id, 0)?
                            .len(),
                        sequence as usize
                    );
                    assert!(!r.notification_pending(scope, true)?);
                    assert!(
                        r.claim_notification(
                            scope,
                            OperationId::generate(),
                            true,
                            5
                        )?
                        .is_none()
                    );
                    Ok(())
                })
                .unwrap();
        }
        store
            .transaction(|r| {
                assert!(r.receive_notification(scope, delivery, 6)?);
                assert!(!r.notification_pending(scope, true)?);
                assert!(
                    r.messages_after(scope.run_id, scope.agent_id, 0)?
                        .iter()
                        .all(|m| m.acknowledged_at.is_none())
                );
                Ok(())
            })
            .unwrap();
        message(&mut store, scope, 6);
        store.transaction(|r| {
            // Even a fresh operation ID for an old receipt cannot swallow a
            // later update or release a newer outstanding notice.
            assert!(r.receive_notification(scope, delivery, 7)?);
            assert!(r.notification_pending(scope, true)?);
            let next = OperationId::generate();
            assert!(r.claim_notification(scope, next, true, 7)?.is_some());
            r.observe_notification(scope, next, QueueOutcome::Accepted, 8)?;
            assert!(r.receive_notification(scope, delivery, 9)?);
            assert!(!r.notification_pending(scope, true)?);
            assert_eq!(r.transaction.query_row("SELECT receipt_state FROM notification_deliveries WHERE operation_id = ?1", [next], |row| row.get::<_, String>(0))?, "pending");
            Ok(())
        }).unwrap();
    }

    #[test]
    fn notification_receipts_are_scoped_and_do_not_resolve_uncertain_effects() {
        for outcome in [
            QueueOutcome::Accepted,
            QueueOutcome::Unknown,
            QueueOutcome::Failed,
        ] {
            let (mut store, scope) = fixture();
            message(&mut store, scope, 1);
            let delivery = OperationId::generate();
            store
                .transaction(|r| {
                    r.claim_notification(scope, delivery, true, 3)?.unwrap();
                    for invalid in [
                        SessionScope {
                            generation: 2,
                            ..scope
                        },
                        SessionScope {
                            session_id: SessionId::generate(),
                            ..scope
                        },
                        SessionScope {
                            agent_id: AgentId::generate(),
                            ..scope
                        },
                    ] {
                        assert!(!r.receive_notification(invalid, delivery, 4)?);
                    }
                    assert!(!r.receive_notification(
                        scope,
                        OperationId::generate(),
                        4
                    )?);
                    // Receipt can race the queue subprocess exit and its recorded
                    // observation. It proves receipt, not the subprocess outcome.
                    assert!(r.receive_notification(scope, delivery, 4)?);
                    assert!(!r.notification_pending(scope, true)?);
                    r.observe_notification(scope, delivery, outcome, 5)?;
                    Ok(())
                })
                .unwrap();
            message(&mut store, scope, 2);
            store
                .transaction(|r| {
                    assert_eq!(
                        r.notification_pending(scope, true)?,
                        outcome == QueueOutcome::Accepted
                    );
                    assert_eq!(
                        r.notification_availability(scope, 6)?,
                        if outcome == QueueOutcome::Accepted {
                            NotificationAvailability::Automatic
                        } else {
                            NotificationAvailability::Uncertain
                        }
                    );
                    Ok(())
                })
                .unwrap();
        }
    }

    #[test]
    fn legacy_delivery_receipt_stays_unknown() {
        let (mut store, scope) = fixture();
        message(&mut store, scope, 1);
        let delivery = OperationId::generate();
        store.transaction(|r| {
            r.claim_notification(scope, delivery, true, 3)?.unwrap();
            r.observe_notification(scope, delivery, QueueOutcome::Accepted, 4)?;
            r.transaction.execute("UPDATE notification_deliveries SET receipt_state = 'legacy'", [])?;
            assert_eq!(r.notification_availability(scope, 5)?, NotificationAvailability::Uncertain);
            assert!(!r.receive_notification(scope, delivery, 5)?);
            Ok(())
        }).unwrap();
        message(&mut store, scope, 2);
        store
            .transaction(|r| {
                assert!(!r.notification_pending(scope, true)?);
                assert!(
                    r.claim_notification(
                        scope,
                        OperationId::generate(),
                        true,
                        6
                    )?
                    .is_none()
                );
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn uncertain_delivery_is_not_retried_by_reconciliation() {
        let (mut store, scope) = fixture();
        message(&mut store, scope, 1);
        let id = OperationId::generate();
        store
            .transaction(|r| r.claim_notification(scope, id, false, 3))
            .unwrap()
            .unwrap();
        message(&mut store, scope, 2);
        for time in [4, 60, 120] {
            assert!(
                store
                    .transaction(|r| r.claim_notification(
                        scope,
                        OperationId::generate(),
                        false,
                        time
                    ))
                    .unwrap()
                    .is_none()
            );
        }
        store
            .transaction(|r| {
                r.observe_notification(scope, id, QueueOutcome::Unknown, 120)?;
                assert!(
                    r.claim_notification(
                        scope,
                        OperationId::generate(),
                        false,
                        121
                    )?
                    .is_none()
                );
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn notification_binding_cannot_move_or_outlive_its_generation() {
        let (mut store, scope) = fixture();
        message(&mut store, scope, 1);
        store.transaction(|r| {
            assert!(!r.bind_notifications(scope, "fedcba98-7654-3210-fedc-ba9876543210")?);
            let stale = SessionScope { generation: 2, ..scope };
            assert!(!r.bind_notifications(stale, "01234567-89ab-cdef-0123-456789abcdef")?);
            assert!(r.claim_notification(stale, OperationId::generate(), true, 3)?.is_none());
            r.transaction.execute("UPDATE sessions SET state = 'exited', ended_at = 3 WHERE id = ?1", [scope.session_id])?;
            assert!(r.claim_notification(scope, OperationId::generate(), true, 4)?.is_none());
            Ok(())
        }).unwrap();
    }

    #[test]
    fn notification_sources_respect_authority_and_ignore_own_changes() {
        let (mut store, scope) = fixture();
        let other = AgentId::generate();
        store
            .transaction(|r| {
                r.insert_agent(&AgentRecord {
                    id: other,
                    run_id: scope.run_id,
                    role: "worker".into(),
                    generation: 1,
                    state: LifecycleState::Running,
                    created_at: 1,
                })
            })
            .unwrap();
        for (actor, subject_agent, expected) in [
            (scope.agent_id.to_string(), Some(other), false),
            ("provider".into(), Some(scope.agent_id), false),
            ("provider".into(), Some(other), true),
        ] {
            store
                .transaction(|r| {
                    r.append_event(&NewEvent {
                        run_id: scope.run_id,
                        kind: EventKind::SessionLifecycleChanged,
                        actor,
                        subject: SessionId::generate().to_string(),
                        project_id: None,
                        agent_id: subject_agent,
                        task_id: None,
                        operation_id: None,
                        correlation_id: None,
                        causation_id: None,
                        data: json!({"state":"exited", "generation":1}),
                        summary: "Private worker detail.".into(),
                        created_at: 3,
                    })?;
                    assert!(!r.notification_pending(scope, false)?);
                    assert_eq!(r.notification_pending(scope, true)?, expected);
                    Ok(())
                })
                .unwrap();
        }
    }

    #[test]
    fn pending_shutdown_fences_notification_delivery() {
        let (mut store, scope) = fixture();
        message(&mut store, scope, 1);
        store
            .transaction(|r| {
                r.begin_run_shutdown(
                    scope.run_id,
                    OperationId::generate(),
                    3000,
                    250,
                    5000,
                )?;
                assert!(!r.notification_pending(scope, true)?);
                assert!(
                    r.claim_notification(
                        scope,
                        OperationId::generate(),
                        true,
                        3
                    )?
                    .is_none()
                );
                assert!(!r.bind_notifications(
                    scope,
                    "01234567-89ab-cdef-0123-456789abcdef"
                )?);
                Ok(())
            })
            .unwrap();
    }
}

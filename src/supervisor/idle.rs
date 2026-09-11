//! Idle shutdown uses durable observations and a monotonic grace period.

use super::*;

pub(super) struct IdleShutdown {
    policy: crate::config::SupervisionPolicy,
    observed: Option<(i64, Instant)>,
}

impl IdleShutdown {
    pub(super) fn new(policy: crate::config::SupervisionPolicy) -> Self {
        Self {
            policy,
            observed: None,
        }
    }

    fn due(&mut self, cursor: Option<i64>, now: Instant) -> bool {
        let Some(cursor) =
            cursor.filter(|_| self.policy.idle_timeout_seconds > 0)
        else {
            self.observed = None;
            return false;
        };
        let (previous, since) = self.observed.get_or_insert((cursor, now));
        if *previous != cursor {
            (*previous, *since) = (cursor, now);
        }
        now.duration_since(*since).as_secs()
            >= self.policy.idle_timeout_seconds as u64
    }

    pub(super) fn begin_if_due(
        &mut self,
        store: &mut Store,
        run_id: RunId,
        now: Instant,
    ) -> Result<bool, SupervisorError> {
        if self.policy.idle_timeout_seconds == 0 {
            return Ok(false);
        }
        let now_ms = unix_timestamp_ms()?;
        let started = store.transaction(|repositories| {
            if !self.due(repositories.idle_shutdown_cursor(run_id)?, now) {
                return Ok(false);
            }
            let operation_id = OperationId::generate();
            crate::fault::point("idle.shutdown.before");
            repositories.begin_run_shutdown(run_id, operation_id, now_ms,
                self.policy.interrupt_grace_ms, self.policy.shutdown_timeout_ms)?;
            repositories.append_event(&NewEvent {
                run_id, kind: EventKind::RunShutdownChanged,
                actor: "supervisor".into(), subject: run_id.to_string(),
                project_id: None, agent_id: None, task_id: None,
                operation_id: Some(operation_id), correlation_id: None, causation_id: None,
                data: json!({"reason": "idle_timeout", "idle_timeout_seconds": self.policy.idle_timeout_seconds}),
                summary: "Stopping the run after its configured idle period.".into(),
                created_at: now_ms / 1000,
            })?;
            crate::fault::point("idle.shutdown.written");
            Ok(true)
        })?;
        if started {
            crate::fault::point("idle.shutdown.committed");
        }
        Ok(started)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_timer_requires_a_full_interval_without_activity_or_uncertainty() {
        let mut idle =
            IdleShutdown::new(crate::config::compiled_defaults().supervision);
        let now = Instant::now();
        assert!(!idle.due(Some(1), now));
        assert!(!idle.due(Some(1), now + Duration::from_secs(59)));
        assert!(idle.due(Some(1), now + Duration::from_secs(60)));
        assert!(!idle.due(Some(2), now + Duration::from_secs(61)));
        assert!(!idle.due(Some(2), now + Duration::from_secs(120)));
        assert!(!idle.due(None, now + Duration::from_secs(121)));
        assert!(!idle.due(Some(2), now + Duration::from_secs(200)));
        assert!(!idle.due(Some(2), now + Duration::from_secs(259)));
        assert!(idle.due(Some(2), now + Duration::from_secs(260)));
        let mut restarted = IdleShutdown::new(idle.policy);
        assert!(!restarted.due(Some(2), now + Duration::from_secs(300)));
        restarted.policy.idle_timeout_seconds = 0;
        assert!(!restarted.due(Some(2), now + Duration::from_secs(600)));
    }
}

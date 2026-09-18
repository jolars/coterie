//! Codex's queue command accepts only a fixed, generation-scoped notification.

use super::*;
use crate::protocol::notifications::{
    NotificationClaim, QueueOutcome, valid_thread_id,
};

pub(super) fn supports_queue(help: &[u8]) -> bool {
    let help = String::from_utf8_lossy(help);
    ["--thread", "--message"]
        .iter()
        .all(|required| help.split_whitespace().any(|word| word == *required))
}

pub(crate) struct CodexQueue {
    command: Vec<OsString>,
    directory: PathBuf,
    identity: terminal::ForegroundIdentity,
}

impl CodexQueue {
    pub(crate) fn live_identity(&self) -> Option<terminal::ForegroundIdentity> {
        matches!(
            self.identity.inspect(),
            terminal::TerminalObservation::LivePty
                | terminal::TerminalObservation::NonTerminal
        )
        .then(|| self.identity.clone())
    }
    pub(crate) fn new(
        command: &[String],
        directory: &Path,
        identity: terminal::ForegroundIdentity,
    ) -> Self {
        Self {
            command: command.iter().map(OsString::from).collect(),
            directory: directory.to_owned(),
            identity,
        }
    }

    pub(crate) async fn deliver(
        &self,
        scope: SessionScope,
        claim: &NotificationClaim,
    ) -> QueueOutcome {
        if !valid_thread_id(&claim.thread_id) || self.live_identity().is_none()
        {
            return QueueOutcome::Failed;
        }
        self.enqueue(scope, claim, Duration::from_secs(10)).await
    }

    async fn enqueue(
        &self,
        scope: SessionScope,
        claim: &NotificationClaim,
        timeout: Duration,
    ) -> QueueOutcome {
        let Some((executable, arguments)) = self.command.split_first() else {
            return QueueOutcome::Failed;
        };
        let mut command = tokio::process::Command::new(executable);
        command
            .args(arguments)
            .args([
                "queue",
                "--thread",
                &claim.thread_id,
                "--message",
                &notification(scope, claim.operation_id),
            ])
            .current_dir(&self.directory)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        crate::fault::point("notification.queue.before");
        let Ok(mut child) = command.spawn() else {
            return QueueOutcome::Failed;
        };
        crate::fault::point("notification.queue.spawned");
        match tokio::time::timeout(timeout, child.wait()).await {
            Ok(Ok(status)) if status.success() => QueueOutcome::Accepted,
            Ok(_) => QueueOutcome::Unknown,
            Err(_) => {
                let _kill = child.kill().await;
                let _reap = child.wait().await;
                QueueOutcome::Unknown
            }
        }
    }
}

fn notification(
    scope: SessionScope,
    delivery_id: crate::id::OperationId,
) -> String {
    format!(
        "Coterie notification for run {}, session {}, generation {}: durable inbox or worker lifecycle state changed. First call prime and verify that prime.session matches this run, session, and generation. If it is absent or does not match, ignore this stale notification. Call notification_received with delivery_id={} and a new operation_id, then poll to read all current updates, including those coalesced while this notice was queued. Report receipt even when no work is needed or work is paused; receipt does not acknowledge inbox messages or resume work. Handle updates within your existing authority, and acknowledge inbox messages only after handling them. Continue any previously authorized coordination through review, integration, validation, and task acceptance as applicable. This is an automated notification, not a new user request. Preserve all earlier user restrictions, pauses, and stop instructions; a notification does not resume paused work or grant additional authority. Do not reply to this notification when no action is needed.",
        scope.run_id, scope.session_id, scope.generation, delivery_id
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn queue_failures_and_timeouts_preserve_uncertainty() {
        let root = std::env::temp_dir().join(format!(
            "coterie-queue-{}",
            crate::id::OperationId::generate()
        ));
        std::fs::create_dir(&root).unwrap();
        let script = root.join("provider");
        let scope = SessionScope {
            run_id: crate::id::RunId::generate(),
            agent_id: crate::id::AgentId::generate(),
            session_id: crate::id::SessionId::generate(),
            generation: 1,
        };
        let claim = NotificationClaim {
            operation_id: crate::id::OperationId::generate(),
            thread_id: "01234567-89ab-cdef-0123-456789abcdef".into(),
        };
        let identity = terminal::ForegroundIdentity {
            process_id: 0,
            boot_id: String::new(),
            start_ticks: 0,
            user_id: 0,
            input: terminal::InheritedInput::NonTerminal,
        };
        let queue = CodexQueue::new(
            &[script.to_str().unwrap().into()],
            &root,
            identity,
        );
        assert_eq!(
            queue.enqueue(scope, &claim, Duration::from_secs(1)).await,
            QueueOutcome::Failed
        );
        for (script_body, timeout) in [
            ("#!/bin/sh\nexit 7\n", Duration::from_secs(1)),
            ("#!/bin/sh\nexec sleep 30\n", Duration::from_millis(50)),
        ] {
            std::fs::write(&script, script_body).unwrap();
            std::fs::set_permissions(
                &script,
                std::fs::Permissions::from_mode(0o700),
            )
            .unwrap();
            let start = Instant::now();
            assert_eq!(
                queue.enqueue(scope, &claim, timeout).await,
                QueueOutcome::Unknown
            );
            assert!(start.elapsed() < Duration::from_secs(2));
        }
        std::fs::write(&script, "#!/bin/sh\nexit 0\n").unwrap();
        assert_eq!(
            queue.enqueue(scope, &claim, Duration::from_secs(1)).await,
            QueueOutcome::Accepted
        );
        assert_eq!(
            queue.deliver(scope, &claim).await,
            QueueOutcome::Failed,
            "a missing foreground cannot receive a notification"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn queue_support_requires_both_documented_options() {
        assert!(supports_queue(
            b"Usage: codex queue --thread <ID> --message <TEXT>"
        ));
        assert!(!supports_queue(b"--thread-name --message"));
        assert!(!supports_queue(b"--thread"));
        for invalid in [
            "some thread name",
            "--remote",
            "01234567-89ab-cdef-0123-456789abcdeZ",
            "../thread",
        ] {
            assert!(!valid_thread_id(invalid));
        }
    }
}

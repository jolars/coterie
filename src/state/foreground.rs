//! Startup evidence is owned by one session and cannot be replaced by a later PID.

use super::*;
use crate::auth::SessionScope;
use crate::providers::terminal::ForegroundIdentity;

impl Repositories<'_, '_> {
    pub(crate) fn foreground_identity(
        &self,
        run_id: RunId,
        session_id: SessionId,
    ) -> Result<Option<ForegroundIdentity>, StoreError> {
        let value: Option<String> = self.transaction.query_row(
            "SELECT identity_json FROM foreground_process_identity WHERE run_id = ?1 AND session_id = ?2",
            params![run_id, session_id], |row| row.get(0),
        ).optional()?;
        value
            .map(|value| serde_json::from_str(&value).map_err(StoreError::from))
            .transpose()
    }

    pub(crate) fn record_foreground_identity(
        &self,
        scope: SessionScope,
        identity: &ForegroundIdentity,
    ) -> Result<SessionTransitionOutcome, StoreError> {
        let session = self.session(scope.session_id)?;
        if !self.session_scope_is_current(scope)?
            || !session.is_some_and(|session| {
                session.process_owner == SessionProcessOwner::Foreground
                    && session.provider_session_id.as_deref()
                        == Some(&format!("process:{}", identity.process_id))
            })
        {
            return Ok(SessionTransitionOutcome::Stale);
        }
        if let Some(existing) =
            self.foreground_identity(scope.run_id, scope.session_id)?
        {
            return if existing == *identity {
                Ok(SessionTransitionOutcome::Unchanged)
            } else {
                Err(StoreError::CorruptAgentState {
                    id: scope.agent_id,
                    reason: "foreground process identity cannot be replaced"
                        .into(),
                })
            };
        }
        self.transaction.execute(
            "INSERT INTO foreground_process_identity (run_id, session_id, identity_json) VALUES (?1, ?2, ?3)",
            params![scope.run_id, scope.session_id, serde_json::to_string(identity)?],
        )?;
        Ok(SessionTransitionOutcome::Applied)
    }
}

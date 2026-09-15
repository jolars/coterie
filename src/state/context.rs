//! Read-only references for bounded current-state inspection.

use super::*;

impl Repositories<'_, '_> {
    pub(crate) fn task_page_ids(
        &self,
        run_id: RunId,
        after: Option<TaskId>,
        limit: u16,
    ) -> Result<Vec<TaskId>, StoreError> {
        let mut statement = self.transaction.prepare(
            "SELECT id FROM tasks WHERE run_id = ?1 AND (?2 IS NULL OR id > ?2) ORDER BY id LIMIT ?3",
        )?;
        Ok(statement
            .query_map(params![run_id, after, limit], |row| row.get(0))?
            .collect::<Result<_, _>>()?)
    }

    pub(crate) fn latest_assignment_for_agent(
        &self,
        run_id: RunId,
        agent_id: AgentId,
    ) -> Result<Option<AssignmentRecord>, StoreError> {
        let id = self.transaction.query_row(
            "SELECT id FROM assignments WHERE run_id = ?1 AND agent_id = ?2 ORDER BY created_at DESC, rowid DESC LIMIT 1",
            params![run_id, agent_id], |row| row.get(0),
        ).optional()?;
        id.map(|id| self.assignment(id))
            .transpose()
            .map(Option::flatten)
    }

    pub(crate) fn task_assignment_ids(
        &self,
        run_id: RunId,
        task_id: TaskId,
    ) -> Result<Vec<AssignmentId>, StoreError> {
        let mut statement = self.transaction.prepare(
            "SELECT id FROM assignments WHERE run_id = ?1 AND task_id = ?2 ORDER BY created_at, rowid",
        )?;
        Ok(statement
            .query_map(params![run_id, task_id], |row| row.get(0))?
            .collect::<Result<_, _>>()?)
    }

    pub(crate) fn latest_task_recovery(
        &self,
        run_id: RunId,
        task_id: TaskId,
    ) -> Result<Option<crate::protocol::RecoverySummary>, StoreError> {
        let source: Option<(String, Option<AssignmentId>)> = self.transaction.query_row(
            "SELECT json_extract(source.payload_json, '$.data'),
                (SELECT subject FROM events AS continuation WHERE continuation.run_id = source.run_id
                    AND continuation.event_type = 'assignment.continued'
                    AND json_extract(continuation.payload_json, '$.data.previous_assignment_id') = json_extract(source.payload_json, '$.data.assignment_id')
                    ORDER BY sequence LIMIT 1)
             FROM events AS source WHERE run_id = ?1 AND task_id = ?2 AND event_type = 'task.recovered' ORDER BY sequence DESC LIMIT 1",
            params![run_id, task_id], |row| Ok((row.get(0)?, row.get(1)?)),
        ).optional()?;
        source
            .map(|(source, continuation)| {
                let mut recovery: crate::protocol::RecoverySummary =
                    serde_json::from_str(&source)?;
                recovery.continuation_assignment_id = continuation;
                Ok(recovery)
            })
            .transpose()
    }
}

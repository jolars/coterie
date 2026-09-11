//! Atomic replacement of a submitted result, retaining its immutable history.

use super::*;

/// Explicit commit compare-and-set and the human account of the correction.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct Resubmission {
    pub(crate) assignment_id: AssignmentId,
    pub(crate) expected_result: String,
    pub(crate) result_commit: String,
    pub(crate) summary: String,
    pub(crate) reason: String,
}

impl Repositories<'_, '_> {
    pub(crate) fn resubmission_preflight(
        &self,
        run_id: RunId,
        submission: &Resubmission,
    ) -> Result<(AssignmentRecord, TaskRecord, WorkspaceRecord), StoreError>
    {
        let reject = |reason: &str| StoreError::ResubmissionConflict {
            assignment_id: submission.assignment_id,
            reason: reason.to_owned(),
        };
        let assignment = self
            .assignment(submission.assignment_id)?
            .filter(|assignment| assignment.run_id == run_id)
            .ok_or_else(|| {
                reject(
                    "assignment not found in this run; inspect `coterie prime`",
                )
            })?;
        let workspace = self.workspace_for_scope(assignment.scope())?;
        let task = self.task(assignment.task_id)?.ok_or_else(|| {
            reject("task not found; inspect `coterie doctor`")
        })?;
        if assignment.state != "completed"
            || assignment.completed_at.is_none()
            || task.status != TaskStatus::Submitted
            || self
                .latest_assignment_for_task(run_id, task.id)?
                .map(|latest| latest.id)
                != Some(assignment.id)
        {
            return Err(reject(
                "only the current submitted result can be superseded; inspect `coterie prime` or create a new task",
            ));
        }
        if workspace.kind != "worktree"
            || workspace.state != ExternalResourceState::Observed
        {
            return Err(reject(
                "an observed Git worktree is required; inspect `coterie doctor`",
            ));
        }
        if workspace.target_commit.is_some() {
            return Err(reject(
                "the result is already integrated; create a new task for further corrections",
            ));
        }
        let integration_intent: bool = self.transaction.query_row(
            "SELECT EXISTS (SELECT 1 FROM operations WHERE run_id = ?1 AND kind = 'workspace.integrate' AND json_extract(request_json, '$.assignment_id') = ?2)",
            params![run_id, assignment.id], |row| row.get(0),
        )?;
        if integration_intent {
            return Err(reject(
                "an integration intent already exists; inspect `coterie doctor` and retry `coterie workspace integrate` with its original operation ID",
            ));
        }
        if workspace.result_commit.as_deref()
            != Some(&submission.expected_result)
            || task
                .result
                .as_ref()
                .and_then(|result| result.get("result_commit"))
                .and_then(JsonValue::as_str)
                != Some(&submission.expected_result)
        {
            return Err(reject(
                "the recorded result changed; inspect `coterie prime` and use `coterie task resubmit` with the current --expected-result and a new operation ID",
            ));
        }
        Ok((assignment, task, workspace))
    }

    pub(crate) fn apply_resubmission(
        &self,
        mutation: &Mutation,
        submission: &Resubmission,
    ) -> Result<(JsonValue, JsonValue), StoreError> {
        let (assignment, task, workspace) =
            self.resubmission_preflight(mutation.run_id, submission)?;
        let previous_result = task.result.unwrap_or(JsonValue::Null);
        let mut result = previous_result.clone();
        result["result_commit"] = json!(submission.result_commit);
        result["summary"] = json!(submission.summary);
        self.append_event(&NewEvent {
            run_id: mutation.run_id,
            kind: EventKind::TaskResubmitted,
            actor: mutation
                .actor_agent_id
                .map_or_else(|| "operator".to_owned(), |id| id.to_string()),
            subject: task.id.to_string(),
            project_id: Some(task.project_id),
            agent_id: Some(assignment.agent_id),
            task_id: Some(task.id),
            operation_id: Some(mutation.id),
            correlation_id: None,
            causation_id: None,
            data: json!({
                "assignment_id": assignment.id,
                "generation": assignment.generation,
                "base_commit": workspace.base_commit,
                "previous_result": previous_result,
                "previous_summary": assignment.summary,
                "previous_completed_at": assignment.completed_at,
                "previous_updated_at": task.updated_at,
                "result": result,
                "reason": submission.reason,
                "status": "submitted",
            }),
            summary: format!(
                "Superseded the submitted result for task {}.",
                task.id
            ),
            created_at: mutation.created_at,
        })?;
        self.transaction.execute(
            "UPDATE workspaces SET result_commit = ?2 WHERE assignment_id = ?1",
            params![assignment.id, submission.result_commit],
        )?;
        self.transaction.execute(
            "UPDATE assignments SET summary = ?2 WHERE id = ?1",
            params![assignment.id, submission.summary],
        )?;
        self.transaction.execute(
            "UPDATE tasks SET result_json = ?2, updated_at = ?3 WHERE id = ?1",
            params![
                task.id,
                serde_json::to_string(&result)?,
                mutation.created_at
            ],
        )?;
        Ok((previous_result, result))
    }
}

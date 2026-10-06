//! Explicit submission of reviewed work after a verified worker exit.

use super::*;
use crate::protocol::recovery::ReportedEvidence;

#[derive(
    Clone, Debug, Eq, PartialEq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(deny_unknown_fields)]
pub(crate) struct RetainedSubmission {
    pub(crate) assignment_id: AssignmentId,
    pub(crate) result_commit: String,
    pub(crate) summary: String,
    pub(crate) reason: String,
    pub(crate) review: ReportedEvidence,
}

impl Repositories<'_, '_> {
    pub(crate) fn submit_retained(
        &self,
        mutation: &Mutation,
        submission: &RetainedSubmission,
    ) -> Result<JsonValue, StoreError> {
        let (assignment, workspace) = self.interrupted_assignment_preflight(
            mutation.run_id,
            submission.assignment_id,
            false,
            Some(&submission.result_commit),
        )?;
        let result = json!({
            "status": "completed", "summary": submission.summary,
            "base_commit": workspace.base_commit, "result_commit": submission.result_commit,
            "retained_submission": {
                "operation_id": mutation.id, "assignment_id": assignment.id,
                "session_id": assignment.session_id, "generation": assignment.generation,
                "submitted_by": mutation.actor_agent_id,
                "reason": submission.reason, "review": submission.review,
            },
        });
        crate::fault::point("retained.record.before");
        self.record_workspace_result_commit(
            assignment.scope(),
            &submission.result_commit,
        )?;
        self.apply_task_transition(&TaskTransitionMutation {
            operation_id: mutation.id,
            run_id: mutation.run_id,
            actor_agent_id: mutation.actor_agent_id,
            task_id: assignment.task_id,
            transition: TaskTransition::Submit,
            result: Some(result.clone()),
            operator_override: None,
            summary: Some(submission.summary.clone()),
            transitioned_at: mutation.created_at,
        })?;
        for (kind, subject, data) in [
            (
                EventKind::TaskLifecycleChanged,
                assignment.task_id.to_string(),
                json!({"previous_status": "in_progress", "status": "submitted",
                    "transition": "submit_retained", "result": result}),
            ),
            (
                EventKind::AssignmentLifecycleChanged,
                assignment.id.to_string(),
                json!({"previous_state": assignment.state, "state": "completed"}),
            ),
            (
                EventKind::ClaimReleased,
                assignment.claim_id.to_string(),
                json!({"claim_id": assignment.claim_id, "assignment_id": assignment.id}),
            ),
        ] {
            self.append_event(&NewEvent {
                run_id: mutation.run_id, kind, actor: mutation_actor(mutation.actor_agent_id),
                subject, project_id: Some(workspace.project_id), agent_id: Some(assignment.agent_id),
                task_id: Some(assignment.task_id), operation_id: Some(mutation.id),
                correlation_id: None, causation_id: None, data,
                summary: format!("Submitted retained commit for assignment {} after verified exit.", assignment.id),
                created_at: mutation.created_at,
            })?;
        }
        crate::fault::point("retained.record.after");
        Ok(result)
    }
}

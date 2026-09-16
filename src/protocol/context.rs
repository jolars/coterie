//! Bounded current context and revision-checked full document retrieval.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::id::{AgentId, AssignmentId, ProjectId, SessionId, TaskId};
use crate::providers::LifecycleState;
use crate::tasks::TaskStatus;

pub(crate) const CONTEXT_BUDGET: usize = 64 * 1024;
pub(crate) const PREVIEW_BYTES: usize = 512;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub(crate) struct PrimePage {
    pub(crate) session: Option<Box<crate::auth::SessionScope>>,
    pub(crate) notifications: super::notifications::NotificationAvailability,
    pub(crate) identity: super::CallerSummary,
    pub(crate) projects: Vec<super::ProjectSummary>,
    pub(crate) peers: Vec<super::AgentSummary>,
    #[serde(flatten)]
    pub(crate) context: TaskContext,
    pub(crate) commit_handoffs: Vec<super::CommitHandoff>,
    pub(crate) commands: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub(crate) struct LogsPage {
    pub(crate) agent: super::AgentSummary,
    pub(crate) session_id: SessionId,
    pub(crate) transcript: String,
    pub(crate) start_cursor: u64,
    pub(crate) total_bytes: u64,
    pub(crate) partial_head: bool,
    pub(crate) next_cursor: u64,
    pub(crate) eof: bool,
    pub(crate) terminal: bool,
    pub(crate) incomplete_tail: bool,
}

/// A lossless prefix, explicitly distinguished from the complete stored text.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub(crate) struct TextPreview {
    pub(crate) text: String,
    pub(crate) total_bytes: usize,
    pub(crate) truncated: bool,
}

impl TextPreview {
    pub(crate) fn new(text: &str) -> Self {
        let mut end = text.len().min(PREVIEW_BYTES);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        Self {
            text: text[..end].to_owned(),
            total_bytes: text.len(),
            truncated: end != text.len(),
        }
    }
}

/// The ID is the stable reference accepted by `task show` / `task_show`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub(crate) struct TaskBrief {
    pub(crate) id: TaskId,
    pub(crate) project_id: ProjectId,
    pub(crate) project: TextPreview,
    pub(crate) title: TextPreview,
    pub(crate) description: TextPreview,
    pub(crate) status: TaskStatus,
    pub(crate) ready: bool,
    pub(crate) unresolved_dependencies: Vec<TaskId>,
    pub(crate) omitted_dependencies: usize,
    pub(crate) result: Option<TextPreview>,
    pub(crate) assignment: Option<AssignmentBrief>,
    pub(crate) next_action: NextAction,
}

/// A recorded prerequisite, never an agent-quality judgment or live process probe.
#[derive(
    Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub(crate) enum NextAction {
    Ready,
    WaitForDependencies,
    ContinueAssignment,
    InspectProvider,
    SpawnContinuation,
    ReviewAndIntegrate,
    ValidateAndClose,
    None,
}

/// The ID is the stable reference accepted by `assignment show` / `assignment_show`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub(crate) struct AssignmentBrief {
    pub(crate) id: AssignmentId,
    pub(crate) agent_id: AgentId,
    pub(crate) session_id: Option<SessionId>,
    pub(crate) generation: i64,
    pub(crate) state: String,
    pub(crate) session_state: Option<LifecycleState>,
    pub(crate) summary: Option<TextPreview>,
    pub(crate) base_commit: Option<String>,
    pub(crate) result_commit: Option<String>,
    pub(crate) target_commit: Option<String>,
}

/// Full native paths and reasons can be fetched using the source assignment ID.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub(crate) struct RecoveryBrief {
    pub(crate) task_id: TaskId,
    pub(crate) assignment_id: AssignmentId,
    pub(crate) session_id: SessionId,
    pub(crate) generation: i64,
    pub(crate) workspace_path: TextPreview,
    pub(crate) base_commit: Option<String>,
    pub(crate) reason: TextPreview,
    pub(crate) continuation_assignment_id: Option<AssignmentId>,
    pub(crate) handoff: Option<Box<super::recovery::RecoveryHandoffBrief>>,
}

/// The bounded section of prime; identity and run configuration metadata are separate.
#[derive(
    Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, JsonSchema,
)]
pub(crate) struct TaskContext {
    pub(crate) tasks: Vec<TaskBrief>,
    pub(crate) ready_tasks: Vec<TaskId>,
    pub(crate) active_task: Option<Box<TaskBrief>>,
    pub(crate) current_task: Option<Box<TaskBrief>>,
    pub(crate) recoveries: Vec<RecoveryBrief>,
    pub(crate) next_task: Option<TaskId>,
    pub(crate) has_more: bool,
}

/// Concatenate text from one revision before decoding the complete JSON document.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub(crate) struct DetailPage {
    pub(crate) text: String,
    pub(crate) revision: String,
    pub(crate) next_cursor: u64,
    pub(crate) total_bytes: u64,
    pub(crate) eof: bool,
}

/// The unabridged document referenced by a compact task.
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub(crate) struct TaskDetail {
    pub(crate) task: super::TaskSummary,
    pub(crate) assignments: Vec<AssignmentId>,
}

/// The unabridged document referenced by an assignment or recovery source.
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub(crate) struct AssignmentDetail {
    pub(crate) assignment: crate::state::AssignmentRecord,
    pub(crate) workspace: Option<WorkspaceDetail>,
    pub(crate) recoveries: Vec<super::RecoverySummary>,
    pub(crate) recovery_handoffs: Vec<super::recovery::RecoveryHandoff>,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
pub(crate) struct WorkspaceDetail {
    pub(crate) project_id: ProjectId,
    pub(crate) kind: String,
    pub(crate) path: String,
    pub(crate) path_bytes: Vec<u8>,
    pub(crate) base_commit: Option<String>,
    pub(crate) result_commit: Option<String>,
    pub(crate) target_commit: Option<String>,
}

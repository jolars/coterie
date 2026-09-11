//! The compact, text-free view available to authorized progress readers.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::id::{AgentId, AssignmentId, ProjectId, RunId, SessionId, TaskId};
use crate::providers::LifecycleState;
use crate::tasks::TaskStatus;

/// One bounded page with a cursor scoped to its run and authenticated reader.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub(crate) struct ProgressPage {
    pub(crate) run_id: RunId,
    #[schemars(length(max = 100))]
    pub(crate) changes: Vec<ProgressChange>,
    pub(crate) next_cursor: String,
    pub(crate) has_more: bool,
    pub(crate) timed_out: bool,
}

/// The durable order is retained even when intervening events are excluded.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub(crate) struct ProgressChange {
    pub(crate) sequence: u64,
    #[serde(flatten)]
    pub(crate) state: ProgressState,
}

/// Only lifecycle data is projected; arbitrary event payloads never cross here.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum ProgressState {
    Task {
        task_id: TaskId,
        project_id: ProjectId,
        status: TaskStatus,
    },
    Assignment {
        assignment_id: AssignmentId,
        task_id: TaskId,
        agent_id: AgentId,
        state: AssignmentState,
    },
    AssignmentSession {
        assignment_id: AssignmentId,
        task_id: TaskId,
        agent_id: AgentId,
        session_id: SessionId,
    },
    Agent {
        agent_id: AgentId,
        generation: i64,
        state: LifecycleState,
    },
    Session {
        session_id: SessionId,
        agent_id: AgentId,
        generation: i64,
        state: LifecycleState,
    },
}

/// Assignment outcomes are independent of the provider lifecycle.
#[derive(
    Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AssignmentState {
    Active,
    Draining,
    Completed,
    Released,
    Canceled,
}

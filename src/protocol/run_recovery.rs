//! Operator reports for retained-run discovery and explicit reactivation.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::TaskCounts;
use crate::id::{OperationId, RunId};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub(crate) struct RunRecoveryReport {
    pub(crate) run_id: RunId,
    pub(crate) operation_id: OperationId,
    pub(crate) previous_stop_operation_id: OperationId,
    pub(crate) previous_stopped_at: i64,
    pub(crate) recovered_at: i64,
    pub(crate) next_step: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct RetainedRun {
    pub(crate) run_id: RunId,
    pub(crate) status: String,
    pub(crate) primary_root: String,
    pub(crate) primary_root_bytes: Vec<u8>,
    pub(crate) created_at: i64,
    pub(crate) stopped_at: Option<i64>,
    pub(crate) tasks: TaskCounts,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct RetainedRuns {
    pub(crate) runs: Vec<RetainedRun>,
}

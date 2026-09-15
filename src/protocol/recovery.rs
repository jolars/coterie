//! Recovery observations and explicitly attributed, unverified handoff reports.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::context::TextPreview;
use crate::id::{AgentId, AssignmentId, OperationId};

/// A reporter's statement and reference, not a check executed by Coterie.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReportedEvidence {
    pub(crate) text: String,
    pub(crate) source: String,
}

#[derive(
    Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, JsonSchema,
)]
#[serde(deny_unknown_fields)]
pub(crate) struct RecoveryReport {
    pub(crate) validation_evidence: Vec<ReportedEvidence>,
    pub(crate) unfinished_steps: Vec<ReportedEvidence>,
}

/// A lossless repository-relative path; display text may replace non-UTF-8 bytes.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub(crate) struct RecoveryPath {
    pub(crate) path: String,
    pub(crate) path_bytes: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub(crate) struct RecoverySnapshot {
    pub(crate) head_commit: String,
    pub(crate) operation_in_progress: bool,
    /// False when unreadable paths or hidden index entries limit the observation.
    pub(crate) complete: bool,
    pub(crate) dirty_paths: Vec<RecoveryPath>,
    pub(crate) staged_paths: Vec<RecoveryPath>,
    pub(crate) unstaged_paths: Vec<RecoveryPath>,
    pub(crate) untracked_paths: Vec<RecoveryPath>,
    pub(crate) conflicted_paths: Vec<RecoveryPath>,
    pub(crate) unreadable_paths: Vec<RecoveryPath>,
    pub(crate) hidden_index_paths: Vec<RecoveryPath>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub(crate) struct EvidencePreview {
    pub(crate) text: TextPreview,
    pub(crate) source: TextPreview,
}

/// Full details are retrieved with the source assignment ID, not repeated in prime.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub(crate) struct RecoveryHandoffBrief {
    pub(crate) operation_id: OperationId,
    pub(crate) source_assignment_id: AssignmentId,
    pub(crate) recorded_at: i64,
    pub(crate) reported_by: Option<AgentId>,
    pub(crate) head_commit: String,
    pub(crate) complete: bool,
    pub(crate) operation_in_progress: bool,
    pub(crate) dirty_paths: usize,
    pub(crate) staged_paths: usize,
    pub(crate) unstaged_paths: usize,
    pub(crate) untracked_paths: usize,
    pub(crate) conflicted_paths: usize,
    pub(crate) unreadable_paths: usize,
    pub(crate) hidden_index_paths: usize,
    pub(crate) validation_evidence_count: usize,
    pub(crate) unfinished_steps_count: usize,
    pub(crate) validation_evidence: Option<EvidencePreview>,
    pub(crate) unfinished_step: Option<EvidencePreview>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub(crate) struct RecoveryHandoff {
    pub(crate) operation_id: OperationId,
    pub(crate) source_assignment_id: AssignmentId,
    pub(crate) recorded_at: i64,
    /// None denotes the operator, as in other task records.
    pub(crate) reported_by: Option<AgentId>,
    pub(crate) mechanical: RecoverySnapshot,
    pub(crate) reported: RecoveryReport,
}

impl RecoveryHandoff {
    pub(crate) fn brief(&self) -> RecoveryHandoffBrief {
        let preview = |e: &ReportedEvidence| EvidencePreview {
            text: TextPreview::new(&e.text),
            source: TextPreview::new(&e.source),
        };
        RecoveryHandoffBrief {
            operation_id: self.operation_id,
            source_assignment_id: self.source_assignment_id,
            recorded_at: self.recorded_at,
            reported_by: self.reported_by,
            head_commit: self.mechanical.head_commit.clone(),
            complete: self.mechanical.complete,
            operation_in_progress: self.mechanical.operation_in_progress,
            dirty_paths: self.mechanical.dirty_paths.len(),
            staged_paths: self.mechanical.staged_paths.len(),
            unstaged_paths: self.mechanical.unstaged_paths.len(),
            untracked_paths: self.mechanical.untracked_paths.len(),
            conflicted_paths: self.mechanical.conflicted_paths.len(),
            unreadable_paths: self.mechanical.unreadable_paths.len(),
            hidden_index_paths: self.mechanical.hidden_index_paths.len(),
            validation_evidence_count: self.reported.validation_evidence.len(),
            unfinished_steps_count: self.reported.unfinished_steps.len(),
            validation_evidence: self
                .reported
                .validation_evidence
                .first()
                .map(preview),
            unfinished_step: self
                .reported
                .unfinished_steps
                .first()
                .map(preview),
        }
    }
}

//! Provider notifications contain references to durable state, never worker text.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::id::OperationId;

#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    Eq,
    PartialEq,
    Serialize,
    Deserialize,
    JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub(crate) enum NotificationAvailability {
    #[default]
    Unavailable,
    PendingBinding,
    Automatic,
    Uncertain,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum QueueOutcome {
    Accepted,
    Failed,
    Unknown,
}

impl QueueOutcome {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Failed => "failed",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct NotificationClaim {
    pub(crate) operation_id: OperationId,
    pub(crate) thread_id: String,
}

pub(crate) fn valid_thread_id(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            if [8, 13, 18, 23].contains(&index) {
                byte == b'-'
            } else {
                byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)
            }
        })
}

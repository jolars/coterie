//! Client mechanics shared by the MCP helpers; authority remains in the supervisor.

use std::collections::{BTreeSet, VecDeque};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::id::{AgentId, MessageId, OperationId, RunId};
use crate::protocol::progress::{ProgressChange, ProgressPage};
use crate::protocol::{
    MessageSummary, RpcFailureCode, RpcRequest, RpcResponse,
};
use crate::supervisor::{SupervisorClient, SupervisorError};

#[cfg(test)]
mod tests;

const MAX_PROGRESS_PAGES: usize = 16;
const MAX_CHANGES: usize = 100;
const MAX_RETRIES: usize = 64;
const MAX_RETRY_BYTES: usize = 4 * 1024 * 1024;

pub(super) trait Transport {
    async fn request(
        &mut self,
        request: RpcRequest,
    ) -> Result<RpcResponse, SupervisorError>;
}

impl Transport for SupervisorClient {
    async fn request(
        &mut self,
        request: RpcRequest,
    ) -> Result<RpcResponse, SupervisorError> {
        super::request_reconnecting(self, request).await
    }
}

/// A read checkpoint can be replayed after a lost response or bridge replacement.
/// It grants no authority and never represents a durable acknowledgement.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct PollCursor {
    pub run_id: RunId,
    pub agent_id: AgentId,
    pub progress: Option<String>,
    pub inbox: u64,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct Poll {
    pub cursor: Option<PollCursor>,
    #[schemars(range(min = 0, max = 5))]
    pub wait_seconds: Option<u8>,
    pub include_progress: Option<bool>,
}

#[derive(Serialize)]
pub(super) struct PollResult {
    pub cursor: PollCursor,
    pub changes: Vec<ProgressChange>,
    pub messages: Vec<MessageSummary>,
    pub has_more: bool,
    pub timed_out: bool,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct Handled {
    pub operation_id: OperationId,
    #[schemars(length(min = 1))]
    pub message_ids: Vec<MessageId>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct Retry {
    pub operation_id: OperationId,
}

struct SavedMutation {
    id: OperationId,
    request: RpcRequest,
    bytes: usize,
    succeeded: bool,
}

pub(super) struct Client<T> {
    transport: T,
    mutations: VecDeque<SavedMutation>,
}

fn failure(code: RpcFailureCode, message: &str) -> SupervisorError {
    SupervisorError::Rejected {
        code,
        message: message.to_owned(),
    }
}

fn unexpected() -> SupervisorError {
    SupervisorError::UnexpectedMessage {
        expected: "the requested client helper response",
    }
}

impl<T: Transport> Client<T> {
    pub fn new(transport: T) -> Self {
        Self {
            transport,
            mutations: VecDeque::new(),
        }
    }

    pub async fn request(
        &mut self,
        request: RpcRequest,
        operation_id: Option<OperationId>,
    ) -> Result<RpcResponse, SupervisorError> {
        if let Some(id) = operation_id {
            if let Some(saved) =
                self.mutations.iter().find(|saved| saved.id == id)
            {
                if saved.request != request {
                    return Err(failure(
                        RpcFailureCode::Conflict,
                        "Retry requires the original operation ID and identical arguments.",
                    ));
                }
            } else {
                let bytes = serde_json::to_vec(&request)?.len();
                while self.mutations.len() >= MAX_RETRIES
                    || self
                        .mutations
                        .iter()
                        .map(|saved| saved.bytes)
                        .sum::<usize>()
                        + bytes
                        > MAX_RETRY_BYTES
                {
                    let Some(index) =
                        self.mutations.iter().position(|saved| saved.succeeded)
                    else {
                        return Err(failure(
                            RpcFailureCode::Conflict,
                            "Client retry storage is full; resolve pending mutations before submitting more work.",
                        ));
                    };
                    self.mutations.remove(index);
                }
                // Save the exact typed request before any dispatch. Uncertain
                // requests cannot be evicted to make room for another mutation.
                self.mutations.push_back(SavedMutation {
                    id,
                    request: request.clone(),
                    bytes,
                    succeeded: false,
                });
            }
        }
        let result = self.transport.request(request).await;
        if let Some(id) = operation_id {
            self.mutations
                .iter_mut()
                .find(|saved| saved.id == id)
                .expect("saved before dispatch")
                .succeeded = result.is_ok();
        }
        result
    }

    pub async fn retry(
        &mut self,
        operation_id: OperationId,
    ) -> Result<RpcResponse, SupervisorError> {
        let request = self.mutations.iter().find(|saved| saved.id == operation_id).map(|saved| saved.request.clone()).ok_or_else(|| failure(RpcFailureCode::NotFound, "The bridge has no saved request for this operation. Retry the original tool with the same operation ID and identical arguments."))?;
        // Replaying through the supervisor rechecks current generation and
        // capabilities, even when the earlier call succeeded.
        self.request(request, Some(operation_id)).await
    }

    pub async fn poll(
        &mut self,
        arguments: Poll,
    ) -> Result<PollResult, SupervisorError> {
        let wait_seconds = arguments.wait_seconds.unwrap_or(0);
        if wait_seconds > 5 {
            return Err(failure(
                RpcFailureCode::InvalidArgument,
                "wait_seconds must be between zero and five.",
            ));
        }
        let RpcResponse::Identity {
            run_id,
            agent: Some(agent),
            ..
        } = self.transport.request(RpcRequest::Whoami).await?
        else {
            return Err(unexpected());
        };
        let mut cursor = arguments.cursor.unwrap_or(PollCursor {
            run_id,
            agent_id: agent.id,
            progress: None,
            inbox: 0,
        });
        if cursor.run_id != run_id || cursor.agent_id != agent.id {
            return Err(failure(
                RpcFailureCode::InvalidArgument,
                "The poll cursor belongs to a different run or agent.",
            ));
        }
        let mut changes = Vec::new();
        let mut has_more = false;
        let mut timed_out = false;
        if arguments.include_progress.unwrap_or(true) {
            for page_index in 0..MAX_PROGRESS_PAGES {
                let RpcResponse::Progress { page } = self
                    .transport
                    .request(RpcRequest::Progress {
                        after: cursor.progress.clone(),
                        limit: (MAX_CHANGES - changes.len()) as u16,
                        wait_seconds: if page_index == 0 {
                            wait_seconds
                        } else {
                            0
                        },
                    })
                    .await?
                else {
                    return Err(unexpected());
                };
                let ProgressPage {
                    run_id: observed_run,
                    changes: next,
                    next_cursor,
                    has_more: more,
                    timed_out: timeout,
                } = page;
                if observed_run != run_id
                    || (more && cursor.progress.as_ref() == Some(&next_cursor))
                    || next.len() > MAX_CHANGES - changes.len()
                {
                    return Err(SupervisorError::InvalidProof);
                }
                changes.extend(next);
                cursor.progress = Some(next_cursor);
                has_more = more;
                timed_out |= timeout;
                if !has_more || changes.len() == MAX_CHANGES {
                    break;
                }
            }
        }
        // Always read the inbox, including after an empty or timed-out progress
        // wait. Neither cursor can cause an acknowledgement mutation.
        let RpcResponse::Inbox { messages, .. } = self
            .transport
            .request(RpcRequest::Inbox {
                after: cursor.inbox,
            })
            .await?
        else {
            return Err(unexpected());
        };
        // Keep unhandled messages in subsequent polls, including after bridge
        // replacement. Reading a batch alone must not consume its inbox items.
        cursor.inbox = messages
            .iter()
            .take_while(|message| message.acknowledged)
            .last()
            .map_or(cursor.inbox, |message| message.sequence);
        let messages = messages
            .into_iter()
            .filter(|message| !message.acknowledged)
            .collect();
        Ok(PollResult {
            cursor,
            changes,
            messages,
            has_more,
            timed_out,
        })
    }

    pub async fn handled(
        &mut self,
        arguments: Handled,
    ) -> Result<RpcResponse, SupervisorError> {
        let ids: BTreeSet<_> = arguments.message_ids.iter().copied().collect();
        if ids.is_empty() || ids.len() != arguments.message_ids.len() {
            return Err(failure(
                RpcFailureCode::InvalidArgument,
                "Supply a nonempty list of unique handled message IDs.",
            ));
        }
        // Resolve immutable IDs from the authenticated inbox rather than trust
        // caller-supplied sequence numbers or cached acknowledgement state.
        let RpcResponse::Inbox { messages, .. } = self
            .transport
            .request(RpcRequest::Inbox { after: 0 })
            .await?
        else {
            return Err(unexpected());
        };
        let selected: Vec<_> = messages
            .iter()
            .filter(|message| ids.contains(&message.id))
            .collect();
        if selected.len() != ids.len() {
            return Err(failure(
                RpcFailureCode::InvalidArgument,
                "A handled message ID is absent from this agent's inbox.",
            ));
        }
        let through = selected
            .iter()
            .map(|message| message.sequence)
            .max()
            .expect("nonempty selection");
        if messages.iter().any(|message| {
            message.sequence <= through
                && !message.acknowledged
                && !ids.contains(&message.id)
        }) {
            return Err(failure(
                RpcFailureCode::Conflict,
                "Earlier messages remain unhandled. Acknowledge only a handled prefix, or finish the earlier messages first.",
            ));
        }
        self.request(
            RpcRequest::InboxAcknowledge {
                operation_id: arguments.operation_id,
                through,
            },
            Some(arguments.operation_id),
        )
        .await
    }
}

//! Versioned local RPC framing and ownership handshakes.

pub(crate) mod progress;

use std::io;

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::auth::AgentToken;
use crate::id::{
    AgentId, AssignmentId, EventId, MessageId, OperationId, ProjectId, RunId,
    SessionId, TaskId,
};
use crate::project::ProjectKey;
use crate::tasks::TaskStatus;

pub(crate) const PROTOCOL_VERSION: u16 = 7;
const MAXIMUM_FRAME_LENGTH: usize = 1024 * 1024;

/// A client-to-supervisor message on the local versioned transport.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "body", rename_all = "snake_case")]
pub(crate) enum ClientMessage {
    Handshake(HandshakeRequest),
    Request(VersionedRequest),
}

/// The first message on every local RPC connection.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HandshakeRequest {
    pub(crate) protocol_version: u16,
    pub(crate) expected_run_id: RunId,
    pub(crate) project_key: ProjectKey,
    pub(crate) channel: ConnectionChannel,
}

/// The intentionally separate caller path established for one connection.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ConnectionChannel {
    Operator,
    Agent,
}

/// A typed request with connection-local correlation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct VersionedRequest {
    pub(crate) protocol_version: u16,
    pub(crate) request_id: u64,
    pub(crate) authentication: RequestAuthentication,
    pub(crate) request: RpcRequest,
}

/// Credentials carried by each request after the channel handshake.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "caller", rename_all = "snake_case")]
pub(crate) enum RequestAuthentication {
    Operator,
    Agent {
        agent_id: AgentId,
        session_id: SessionId,
        token: AgentToken,
    },
}

/// Operations currently provided by the supervisor transport.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "method", content = "parameters", rename_all = "snake_case")]
pub(crate) enum RpcRequest {
    Ping,
    LaunchForeground {
        operation_id: OperationId,
        token: AgentToken,
    },
    ForegroundStarted {
        scope: crate::auth::SessionScope,
        process_id: u32,
    },
    WaitForegroundControl {
        scope: crate::auth::SessionScope,
    },
    ForegroundExited {
        scope: crate::auth::SessionScope,
        code: Option<i32>,
        signal: Option<i32>,
    },
    ForegroundLaunchFailed {
        scope: crate::auth::SessionScope,
    },
    ForegroundObservationLost {
        scope: crate::auth::SessionScope,
    },
    Status,
    Doctor,
    Whoami,
    Prime,
    Progress {
        after: Option<String>,
        limit: u16,
        wait_seconds: u8,
    },
    ProjectList,
    ProjectAttach {
        operation_id: OperationId,
        #[serde(with = "crate::project::path_bytes")]
        path: std::path::PathBuf,
        alias: Option<String>,
    },
    TaskCreate {
        operation_id: OperationId,
        title: String,
        description: String,
        project: String,
        group: Option<String>,
        dependencies: Vec<TaskId>,
    },
    TaskReady,
    TaskResubmit {
        operation_id: OperationId,
        submission: crate::state::resubmit::Resubmission,
    },
    TaskClose {
        operation_id: OperationId,
        task_id: TaskId,
        summary: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        operator_override: Option<Box<ClosureOverrideRequest>>,
    },
    Spawn {
        operation_id: OperationId,
        role: String,
        task_id: TaskId,
    },
    WorkspaceIntegrate {
        operation_id: OperationId,
        assignment_id: AssignmentId,
    },
    Finish {
        operation_id: OperationId,
        status: FinishStatus,
        summary: String,
    },
    Send {
        operation_id: OperationId,
        recipient: String,
        message: String,
    },
    Inbox {
        after: u64,
    },
    InboxAcknowledge {
        operation_id: OperationId,
        through: u64,
    },
    Logs {
        agent: String,
        after: u64,
        limit: u32,
        session_id: Option<SessionId>,
    },
    Events {
        after: u64,
        limit: u16,
    },
    Shutdown {
        operation_id: OperationId,
    },
}

/// Explicit operator acceptance of a result applied outside Coterie.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ClosureOverrideRequest {
    pub(crate) assignment_id: AssignmentId,
    pub(crate) result_commit: String,
    pub(crate) target_commit: String,
    pub(crate) reason: String,
}

/// The durable task outcome reported by an assigned agent.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FinishStatus {
    Completed,
    Failed,
}

/// A supervisor-to-client message on the local versioned transport.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "body", rename_all = "snake_case")]
pub(crate) enum ServerMessage {
    Handshake(HandshakeResponse),
    Rejected(RpcFailure),
    Response(VersionedResponse),
}

/// Proof returned by a live supervisor for the expected run and project.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HandshakeResponse {
    pub(crate) protocol_version: u16,
    pub(crate) run_id: RunId,
    pub(crate) project_id: ProjectId,
    pub(crate) project_key: ProjectKey,
}

/// A correlated response to a typed request.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct VersionedResponse {
    pub(crate) protocol_version: u16,
    pub(crate) request_id: u64,
    pub(crate) result: RpcResult,
}

/// The success or failure payload of one local RPC.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", content = "value", rename_all = "snake_case")]
pub(crate) enum RpcResult {
    Ok(Box<RpcResponse>),
    Err(RpcFailure),
}

/// Successful local RPC results.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub(crate) enum RpcResponse {
    Pong {
        run_id: RunId,
    },
    ForegroundPrepared {
        run_id: RunId,
        agent: AgentSummary,
        session_id: SessionId,
        generation: i64,
        bootstrap_instruction: String,
    },
    ForegroundObserved {
        session_id: SessionId,
        state: String,
    },
    ForegroundTerminationRequested {
        session_id: SessionId,
    },
    Status {
        run_id: RunId,
        status: String,
        projects: Vec<ProjectSummary>,
        agents: Vec<AgentSummary>,
        tasks: TaskCounts,
    },
    Doctor {
        report: crate::doctor::DoctorReport,
    },
    Identity {
        run_id: RunId,
        channel: CallerChannel,
        agent: Option<AgentSummary>,
    },
    Prime {
        identity: CallerSummary,
        projects: Vec<ProjectSummary>,
        peers: Vec<AgentSummary>,
        tasks: Vec<TaskSummary>,
        ready_tasks: Vec<TaskSummary>,
        active_task: Option<Box<TaskSummary>>,
        commands: Vec<String>,
    },
    Progress {
        #[serde(flatten)]
        page: progress::ProgressPage,
    },
    Projects {
        projects: Vec<ProjectSummary>,
    },
    ProjectAttached {
        operation_id: OperationId,
        project: ProjectSummary,
    },
    TaskCreated {
        operation_id: OperationId,
        task: TaskSummary,
    },
    ReadyTasks {
        tasks: Vec<TaskSummary>,
    },
    TaskClosed {
        operation_id: OperationId,
        task: TaskSummary,
    },
    Spawned {
        operation_id: OperationId,
        agent: AgentSummary,
        session_id: SessionId,
        assignment_id: AssignmentId,
        task_id: TaskId,
    },
    WorkspaceIntegrated {
        operation_id: OperationId,
        integration: IntegrationSummary,
    },
    AssignmentFinished {
        operation_id: OperationId,
        assignment_id: AssignmentId,
        task: TaskSummary,
    },
    TaskResubmitted {
        operation_id: OperationId,
        #[serde(flatten)]
        submission: ResubmissionSummary,
    },
    MessageSent {
        operation_id: OperationId,
        message_id: MessageId,
        recipient: AgentSummary,
        sequence: u64,
    },
    Inbox {
        messages: Vec<MessageSummary>,
        next_cursor: u64,
    },
    InboxAcknowledged {
        operation_id: OperationId,
        acknowledged_through: u64,
        acknowledged_count: u64,
    },
    Logs {
        agent: AgentSummary,
        session_id: SessionId,
        transcript: String,
        next_cursor: u64,
        eof: bool,
        terminal: bool,
        incomplete_tail: bool,
    },
    Events {
        events: Vec<EventSummary>,
        next_cursor: u64,
    },
    ShuttingDown {
        run_id: RunId,
        operation_id: OperationId,
    },
}

/// Whether a response describes the local operator or an authenticated agent.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CallerChannel {
    Operator,
    Agent,
}

/// Caller identity included in dynamic bootstrap context.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct CallerSummary {
    pub(crate) run_id: RunId,
    pub(crate) channel: CallerChannel,
    pub(crate) agent: Option<AgentSummary>,
}

/// A run-local agent name and its durable identity.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct AgentSummary {
    pub(crate) id: AgentId,
    pub(crate) name: String,
    pub(crate) role: String,
    pub(crate) state: String,
}

/// An attached project visible to the caller.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct ProjectSummary {
    pub(crate) id: ProjectId,
    pub(crate) alias: String,
    pub(crate) root: String,
    pub(crate) access: String,
}

/// A task's command-facing representation.
#[derive(
    Clone, Debug, Eq, PartialEq, Serialize, Deserialize, schemars::JsonSchema,
)]
pub(crate) struct TaskSummary {
    pub(crate) id: TaskId,
    pub(crate) project_id: ProjectId,
    pub(crate) project: String,
    pub(crate) title: String,
    pub(crate) description: String,
    pub(crate) status: TaskStatus,
    pub(crate) ready: bool,
    pub(crate) unresolved_dependencies: Vec<TaskId>,
    pub(crate) result: Option<serde_json::Value>,
}

/// The immutable response recorded by a successful submission correction.
#[derive(
    Clone, Debug, Eq, PartialEq, Serialize, Deserialize, schemars::JsonSchema,
)]
pub(crate) struct ResubmissionSummary {
    pub(crate) assignment_id: AssignmentId,
    pub(crate) previous_result: serde_json::Value,
    pub(crate) task: TaskSummary,
}

/// Exact identities recorded by one explicit guarded integration.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct IntegrationSummary {
    pub(crate) assignment_id: AssignmentId,
    pub(crate) project_id: ProjectId,
    pub(crate) target_reference: String,
    pub(crate) base_commit: String,
    pub(crate) result_commit: String,
    pub(crate) target_commit_before: String,
    pub(crate) target_commit: String,
}

/// Counts of tasks in each durable lifecycle state.
#[derive(
    Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize,
)]
pub(crate) struct TaskCounts {
    pub(crate) open: u64,
    pub(crate) in_progress: u64,
    pub(crate) submitted: u64,
    pub(crate) closed: u64,
    pub(crate) canceled: u64,
}

/// One message read from the caller's durable inbox.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct MessageSummary {
    pub(crate) id: MessageId,
    pub(crate) sequence: u64,
    pub(crate) sender: Option<AgentSummary>,
    pub(crate) body: String,
    pub(crate) created_at: i64,
    pub(crate) acknowledged: bool,
}

/// One immutable run event returned after a sequence cursor.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct EventSummary {
    pub(crate) id: EventId,
    pub(crate) run_id: RunId,
    pub(crate) sequence: u64,
    pub(crate) event_type: String,
    pub(crate) actor: String,
    pub(crate) subject: String,
    pub(crate) project_id: Option<ProjectId>,
    pub(crate) agent_id: Option<AgentId>,
    pub(crate) task_id: Option<TaskId>,
    pub(crate) operation_id: Option<OperationId>,
    pub(crate) correlation_id: Option<EventId>,
    pub(crate) causation_id: Option<EventId>,
    pub(crate) payload: serde_json::Value,
    pub(crate) summary: String,
    pub(crate) created_at: i64,
}

/// A stable local-protocol failure.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RpcFailure {
    pub(crate) code: RpcFailureCode,
    pub(crate) message: String,
}

impl RpcFailure {
    #[must_use]
    pub(crate) fn new(
        code: RpcFailureCode,
        message: impl Into<String>,
    ) -> Self {
        Self {
            code,
            message: crate::redaction::text(&message.into()),
        }
    }
}

/// Machine-readable failures at the local transport boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RpcFailureCode {
    ProtocolVersionMismatch,
    RunMismatch,
    ProjectMismatch,
    HandshakeRequired,
    InvalidRequestSequence,
    Unauthenticated,
    PermissionDenied,
    InvalidArgument,
    NotFound,
    Conflict,
    Unavailable,
    Internal,
}

/// A failure to encode, transmit, or decode one framed message.
#[derive(Debug, Error)]
pub(crate) enum FrameError {
    #[error("local RPC I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error(
        "local RPC frame has {length} bytes, exceeding the {maximum}-byte limit"
    )]
    TooLarge { length: usize, maximum: usize },
    #[error("could not encode local RPC frame: {0}")]
    Encode(#[source] serde_json::Error),
    #[error("could not decode local RPC frame: {0}")]
    Decode(#[source] serde_json::Error),
}

pub(crate) async fn write_frame<W, T>(
    writer: &mut W,
    value: &T,
) -> Result<(), FrameError>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let encoded = serde_json::to_vec(value).map_err(FrameError::Encode)?;
    if encoded.len() > MAXIMUM_FRAME_LENGTH {
        return Err(FrameError::TooLarge {
            length: encoded.len(),
            maximum: MAXIMUM_FRAME_LENGTH,
        });
    }
    let length =
        u32::try_from(encoded.len()).map_err(|_| FrameError::TooLarge {
            length: encoded.len(),
            maximum: MAXIMUM_FRAME_LENGTH,
        })?;
    writer.write_all(&length.to_be_bytes()).await?;
    writer.write_all(&encoded).await?;
    writer.flush().await?;
    Ok(())
}

pub(crate) async fn read_frame<R, T>(reader: &mut R) -> Result<T, FrameError>
where
    R: AsyncRead + Unpin,
    T: DeserializeOwned,
{
    let mut header = [0_u8; 4];
    reader.read_exact(&mut header).await?;
    let length = u32::from_be_bytes(header) as usize;
    if length > MAXIMUM_FRAME_LENGTH {
        return Err(FrameError::TooLarge {
            length,
            maximum: MAXIMUM_FRAME_LENGTH,
        });
    }
    let mut encoded = vec![0_u8; length];
    reader.read_exact(&mut encoded).await?;
    serde_json::from_slice(&encoded).map_err(FrameError::Decode)
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tokio::io::{AsyncWriteExt, duplex};

    use super::{
        ClientMessage, ConnectionChannel, FrameError, HandshakeRequest,
        MAXIMUM_FRAME_LENGTH, PROTOCOL_VERSION, RequestAuthentication,
        RpcRequest, VersionedRequest, read_frame, write_frame,
    };
    use crate::auth::AgentToken;
    use crate::id::{AgentId, AssignmentId, OperationId, RunId, SessionId};
    use crate::project::ProjectKey;

    const RUN_ID: &str = "cr-01ARZ3NDEKTSV4RRFFQ69G5FAV";

    #[test]
    fn mutating_request_wire_shape_carries_its_operation_id() {
        let operation_id = "co-01ARZ3NDEKTSV4RRFFQ69G5FAW"
            .parse::<OperationId>()
            .expect("valid operation ID");
        let message = ClientMessage::Request(VersionedRequest {
            protocol_version: PROTOCOL_VERSION,
            request_id: 7,
            authentication: RequestAuthentication::Operator,
            request: RpcRequest::Shutdown { operation_id },
        });

        assert_eq!(
            serde_json::to_value(message).expect("the request should encode"),
            json!({
                "type": "request",
                "body": {
                    "protocol_version": 7,
                    "request_id": 7,
                    "authentication": {
                        "caller": "operator"
                    },
                    "request": {
                        "method": "shutdown",
                        "parameters": {
                            "operation_id": "co-01ARZ3NDEKTSV4RRFFQ69G5FAW"
                        }
                    }
                }
            })
        );
    }

    #[test]
    fn guarded_integration_wire_shape_identifies_the_assignment() {
        let operation_id = "co-01ARZ3NDEKTSV4RRFFQ69G5FAW"
            .parse::<OperationId>()
            .expect("valid operation ID");
        let assignment_id = "ca-01ARZ3NDEKTSV4RRFFQ69G5FAX"
            .parse::<AssignmentId>()
            .expect("valid assignment ID");
        let request = RpcRequest::WorkspaceIntegrate {
            operation_id,
            assignment_id,
        };

        assert_eq!(
            serde_json::to_value(request).expect("the request should encode"),
            json!({
                "method": "workspace_integrate",
                "parameters": {
                    "operation_id": "co-01ARZ3NDEKTSV4RRFFQ69G5FAW",
                    "assignment_id": "ca-01ARZ3NDEKTSV4RRFFQ69G5FAX"
                }
            })
        );
    }

    #[tokio::test]
    async fn versioned_typed_frames_round_trip_without_delimiter_ambiguity() {
        let (mut writer, mut reader) = duplex(4096);
        let message = ClientMessage::Handshake(HandshakeRequest {
            protocol_version: PROTOCOL_VERSION,
            expected_run_id: RUN_ID.parse::<RunId>().expect("valid run ID"),
            project_key: ProjectKey::from_hex(
                "8da92545f14c259a7e013179f6d9709517f20fe830df519c48e21d393f53f7a5",
            )
            .expect("valid project key"),
            channel: ConnectionChannel::Operator,
        });

        write_frame(&mut writer, &message)
            .await
            .expect("the frame should be written");
        let decoded = read_frame::<_, ClientMessage>(&mut reader)
            .await
            .expect("the frame should be decoded");

        assert_eq!(decoded, message);
    }

    #[tokio::test]
    async fn request_envelopes_preserve_ids_and_typed_methods() {
        let (mut writer, mut reader) = duplex(4096);
        let message = ClientMessage::Request(VersionedRequest {
            protocol_version: PROTOCOL_VERSION,
            request_id: 42,
            authentication: RequestAuthentication::Operator,
            request: RpcRequest::Ping,
        });

        write_frame(&mut writer, &message)
            .await
            .expect("the frame should be written");

        assert_eq!(
            read_frame::<_, ClientMessage>(&mut reader)
                .await
                .expect("the request should decode"),
            message
        );
    }

    #[test]
    fn agent_request_wire_shape_carries_scoped_credentials_but_debug_redacts_them()
     {
        let token: AgentToken =
            "cot1_000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f"
                .parse()
                .expect("valid token");
        let message = ClientMessage::Request(VersionedRequest {
            protocol_version: PROTOCOL_VERSION,
            request_id: 9,
            authentication: RequestAuthentication::Agent {
                agent_id: "cg-01ARZ3NDEKTSV4RRFFQ69G5FAX"
                    .parse::<AgentId>()
                    .expect("valid agent ID"),
                session_id: "cs-01ARZ3NDEKTSV4RRFFQ69G5FAY"
                    .parse::<SessionId>()
                    .expect("valid session ID"),
                token: token.clone(),
            },
            request: RpcRequest::Ping,
        });

        assert_eq!(
            serde_json::to_value(&message).expect("the request should encode"),
            json!({
                "type": "request",
                "body": {
                    "protocol_version": 7,
                    "request_id": 9,
                    "authentication": {
                        "caller": "agent",
                        "agent_id": "cg-01ARZ3NDEKTSV4RRFFQ69G5FAX",
                        "session_id": "cs-01ARZ3NDEKTSV4RRFFQ69G5FAY",
                        "token": token.expose_secret(),
                    },
                    "request": { "method": "ping" }
                }
            })
        );
        assert!(!format!("{message:?}").contains(&token.expose_secret()));
    }

    #[tokio::test]
    async fn oversized_frames_are_rejected_before_payload_allocation() {
        let (mut writer, mut reader) = duplex(16);
        let oversized = u32::try_from(MAXIMUM_FRAME_LENGTH + 1)
            .expect("the frame limit should fit a u32");
        writer
            .write_all(&oversized.to_be_bytes())
            .await
            .expect("the frame header should be written");

        assert!(matches!(
            read_frame::<_, ClientMessage>(&mut reader).await,
            Err(FrameError::TooLarge {
                length,
                maximum: MAXIMUM_FRAME_LENGTH,
            }) if length == MAXIMUM_FRAME_LENGTH + 1
        ));
    }

    #[tokio::test]
    async fn malformed_json_is_a_framing_error() {
        let (mut writer, mut reader) = duplex(32);
        writer
            .write_all(&5_u32.to_be_bytes())
            .await
            .expect("the frame header should be written");
        writer
            .write_all(b"nope!")
            .await
            .expect("the malformed payload should be written");

        assert!(matches!(
            read_frame::<_, ClientMessage>(&mut reader).await,
            Err(FrameError::Decode(_))
        ));
    }
}

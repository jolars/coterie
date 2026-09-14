//! The MCP surface contains only agent operations, with schemas from typed inputs.

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::id::{AssignmentId, OperationId, TaskId};
use crate::protocol::{FinishStatus, RpcRequest};

// The catalog and decoder share the same input types so advertised arguments
// cannot drift from the requests the bridge actually accepts.
macro_rules! agent_tools {
    ($($args:ident ($name:literal, $description:literal, $read_only:literal) {
        $($field:ident: $ty:ty),* $(,)?
    } => $request:expr),* $(,)?) => {
        $(#[derive(Deserialize, JsonSchema)]
        #[serde(deny_unknown_fields)]
        struct $args { $($field: $ty),* })*

        pub(super) fn catalog() -> Vec<Value> {
            vec![$(json!({
                "name": $name,
                "description": $description,
                "inputSchema": schemars::schema_for!($args),
                "annotations": {
                    "readOnlyHint": $read_only,
                    "destructiveHint": !$read_only,
                    "idempotentHint": true
                }
            })),*]
        }

        pub(super) fn request(name: &str, arguments: Value) -> Result<RpcRequest, &'static str> {
            match name {
                $($name => {
                    let $args { $($field),* } = serde_json::from_value(arguments)
                        .map_err(|_| "Arguments do not match this tool's schema.")?;
                    let request = $request;
                    if matches!(&request, RpcRequest::ProjectAttach { path, .. } if !path.is_absolute()) {
                        return Err("Project attachment requires an absolute path.");
                    }
                    Ok(request)
                }),*
                _ => Err("Unknown Coterie agent tool."),
            }
        }
    }
}

agent_tools! {
    Whoami("whoami", "Report the authenticated agent and session identity.", true) {} => RpcRequest::Whoami,
    Prime("prime", "Read current orchestration context, capabilities, tasks, and assignment. Call at startup.", true) {} => RpcRequest::Prime,
    Status("status", "Inspect the run, agents, and tasks within this agent's authority.", true) {} => RpcRequest::Status,
    Progress("progress", "Read lifecycle changes. Save next_cursor, drain has_more, and keep the inbox cursor separate. wait_seconds is bounded by the supervisor.", true) {
        after: Option<String>, limit: u16, wait_seconds: u8
    } => RpcRequest::Progress { after, limit, wait_seconds },
    ProjectList("project_list", "List projects attached to the run.", true) {} => RpcRequest::ProjectList,
    ProjectAttach("project_attach", "Attach an absolute project path beneath an operator-allowed root. Requires project:attach. Reuse operation_id on retry.", false) {
        operation_id: OperationId, path: std::path::PathBuf, alias: Option<String>
    } => RpcRequest::ProjectAttach { operation_id, path, alias },
    TaskCreate("task_create", "Create a task with exactly one attached target project. Reuse operation_id on retry.", false) {
        operation_id: OperationId, title: String, description: String,
        project: String, group: Option<String>, dependencies: Vec<TaskId>
    } => RpcRequest::TaskCreate { operation_id, title, description, project, group, dependencies },
    TaskReady("task_ready", "List tasks ready for assignment.", true) {} => RpcRequest::TaskReady,
    TaskRecover("task_recover", "Retire an exited agent's unfinished assignment and preserve its work for continuation. Requires task:recover. Reuse operation_id on retry.", false) {
        operation_id: OperationId, assignment_id: AssignmentId, reason: String
    } => RpcRequest::TaskRecover { operation_id, assignment_id, reason },
    TaskResubmit("task_resubmit", "Supersede an unintegrated Git submission after validating and committing its correction. Requires task:resubmit. Reuse operation_id on retry.", false) {
        operation_id: OperationId, submission: crate::state::resubmit::Resubmission
    } => RpcRequest::TaskResubmit { operation_id, submission },
    TaskClose("task_close", "Accept a submitted task after review, integration when needed, and validation. Requires task:close. Reuse operation_id on retry.", false) {
        operation_id: OperationId, task_id: TaskId, summary: String
    } => RpcRequest::TaskClose { operation_id, task_id, summary, operator_override: None },
    Spawn("spawn", "Launch a configured role on a ready task within run limits. Reuse operation_id on retry.", false) {
        operation_id: OperationId, role: String, task_id: TaskId
    } => RpcRequest::Spawn { operation_id, role, task_id },
    WorkspaceIntegrate("workspace_integrate", "Integrate a reviewed Git result into its clean, unchanged target. Requires workspace:integrate. Reuse operation_id on retry.", false) {
        operation_id: OperationId, assignment_id: AssignmentId
    } => RpcRequest::WorkspaceIntegrate { operation_id, assignment_id },
    Finish("finish", "Submit this assignment. Before completed, validate and commit intended Git changes; dirty work rejects completion. Submission is separate from accepted task closure. Reuse operation_id on retry.", false) {
        operation_id: OperationId, status: FinishStatus, summary: String
    } => RpcRequest::Finish { operation_id, status, summary },
    Send("send", "Send a durable message to an authorized recipient. Reuse operation_id on retry.", false) {
        operation_id: OperationId, recipient: String, message: String
    } => RpcRequest::Send { operation_id, recipient, message },
    Inbox("inbox", "Read messages after the saved inbox cursor, starting at zero. Read at startup, task boundaries, each coordination cycle, and before finishing.", true) {
        after: u64
    } => RpcRequest::Inbox { after },
    InboxAcknowledge("inbox_acknowledge", "Acknowledge messages only after handling every message through the inbox cursor. Reuse operation_id on retry.", false) {
        operation_id: OperationId, through: u64
    } => RpcRequest::InboxAcknowledge { operation_id, through },
    Logs("logs", "Read a bounded page of an authorized agent's provider transcript.", true) {
        agent: String, after: u64, limit: u32, session_id: Option<crate::id::SessionId>
    } => RpcRequest::Logs { agent, after, limit, session_id },
    Events("events", "Read a bounded page of run events within the agent's authority.", true) {
        after: u64, limit: u16
    } => RpcRequest::Events { after, limit },
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct Empty {}

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
    NotificationReceived("notification_received", "Report receipt of the delivery_id in an automatic Coterie notice after verifying its scope with prime. Then poll to read all current updates. This only releases notification coalescing; it does not acknowledge inbox messages, accept tasks, or resume paused work. Reuse operation_id and delivery_id on retry. Ordinary prime and poll reads do not report receipt.", false) {
        operation_id: OperationId, delivery_id: OperationId
    } => RpcRequest::ReceiveForegroundNotification { operation_id, delivery_id },
    Whoami("whoami", "Report the authenticated agent and session identity.", true) {} => RpcRequest::Whoami,
    Prime("prime", "Read bounded current tasks, assignment summaries, recovery references, and commit_handoffs. Drain has_more using next_task as after_task; current_task stays pinned. Fetch full text with task_show and assignment_show. Establish an authorized coordinator for worktree commits before editing. Call at startup.", true) { after_task: Option<TaskId>, limit: Option<u16> } => RpcRequest::Prime { after_task, limit: limit.unwrap_or(20) },
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
    TaskShow("task_show", "Read a full task document, including description, result, and assignment references. Concatenate text pages before decoding JSON. Continue with after=next_cursor and the same revision; restart at zero on conflict. Requires task:read.", true) {
        task_id: TaskId, after: Option<u64>, limit: Option<u32>, revision: Option<String>
    } => RpcRequest::TaskShow { task_id, after: after.unwrap_or(0), limit: limit.unwrap_or(16384), revision },
    AssignmentShow("assignment_show", "Read a full assignment report, workspace identity, and recovery handoffs as JSON document pages. Handoffs separate recorded Git snapshots from reported checks and unfinished steps with source references; absent reports remain unknown. Continue with after=next_cursor and the same revision. Requires task:read.", true) {
        assignment_id: AssignmentId, after: Option<u64>, limit: Option<u32>, revision: Option<String>
    } => RpcRequest::AssignmentShow { assignment_id, after: after.unwrap_or(0), limit: limit.unwrap_or(16384), revision },
    TaskReady("task_ready", "List tasks ready for assignment.", true) {} => RpcRequest::TaskReady,
    TaskRecover("task_recover", "Retire an exited agent's unfinished assignment, snapshot preserved Git state, and record a recovery handoff. Supply reported validation_evidence and unfinished_steps with text and source references in report; omitted evidence remains unknown. Full handoffs are available through assignment_show. Requires task:recover and an active run. For a stopped run, ask the operator to use coterie run list and coterie run recover <run-id> --reason TEXT, then launch a fresh session. Active-run reconnects cannot reactivate stopped runs or revive credentials. Reuse operation_id and identical arguments on retry.", false) {
        operation_id: OperationId, assignment_id: AssignmentId, reason: String, report: Option<crate::protocol::recovery::RecoveryReport>
    } => RpcRequest::TaskRecover { operation_id, assignment_id, reason, report },
    TaskResubmit("task_resubmit", "Supersede an unintegrated Git submission after validating and committing its correction. Requires task:resubmit. Reuse operation_id on retry.", false) {
        operation_id: OperationId, submission: crate::state::resubmit::Resubmission
    } => RpcRequest::TaskResubmit { operation_id, submission },
    TaskClose("task_close", "Accept a submitted task after review, integration when needed, and validation. Requires task:close. Reuse operation_id on retry.", false) {
        operation_id: OperationId, task_id: TaskId, summary: String
    } => RpcRequest::TaskClose { operation_id, task_id, summary, operator_override: None },
    Spawn("spawn", "Launch a configured role on a ready task within run limits. Reuse operation_id on retry.", false) {
        operation_id: OperationId, role: String, task_id: TaskId
    } => RpcRequest::Spawn { operation_id, role, task_id },
    WorkspaceIntegrate("workspace_integrate", "Integrate a reviewed Git result into its clean, unchanged target. Defaults to rebase for linear history; merge is opt-in. Requires workspace:integrate. Reuse operation_id on retry.", false) {
        operation_id: OperationId, assignment_id: AssignmentId, strategy: Option<crate::workspace::IntegrationStrategy>
    } => RpcRequest::WorkspaceIntegrate { operation_id, assignment_id, strategy },
    Finish("finish", "Submit this assignment. Before completed, validate and commit intended Git changes through the worktree coordinator handoff from prime; confirm HEAD and cleanliness. Dirty work rejects completion. Submission is separate from accepted task closure. Reuse operation_id on retry.", false) {
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
    Logs("logs", "Read an authorized raw provider transcript. Set tail=true with after=0 to reach recent activity directly. Preserve session_id and next_cursor for subsequent reads with tail=false. Full transcripts remain available from after=0.", true) {
        agent: String, after: u64, limit: u32, session_id: Option<crate::id::SessionId>, tail: Option<bool>
    } => RpcRequest::Logs { agent, after, limit, session_id, tail: tail.unwrap_or(false) },
    Events("events", "Read a bounded page of run events within the agent's authority.", true) {
        after: u64, limit: u16
    } => RpcRequest::Events { after, limit },
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct Empty {}

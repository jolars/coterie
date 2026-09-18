//! Agent-only MCP access to the authenticated supervisor protocol.

use serde::Deserialize;
use serde_json::{Value, json};
use thiserror::Error;
use tokio::io::{
    AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt,
    BufReader,
};

use crate::id::OperationId;
use crate::protocol::RpcRequest;
use crate::supervisor::{SupervisorClient, SupervisorError};

mod client;
#[cfg(test)]
mod tests;
mod tools;

const MAXIMUM_MESSAGE_BYTES: usize = 1024 * 1024;
const PROTOCOL_VERSION: &str = "2025-06-18";
pub(crate) const INSTRUCTIONS: &str = "Use the Coterie MCP tools for all orchestration. Call prime at startup. If tools are deferred, discover them with tool_search using Coterie prime as the query. Call new_operation_id before each mutation. After uncertainty, call retry_mutation with that ID; if unavailable, repeat the original tool with the same ID and arguments. Tokens and caller identity are supplied by the bridge; never put them in tool arguments. Use poll for progress and pending inbox messages; inbox_handled acknowledges only explicitly handled messages. Shell commands retain their selected sandbox restrictions.";

#[derive(Debug, Error)]
pub(crate) enum McpError {
    #[error("Coterie MCP stream failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("Coterie MCP message exceeds the size limit")]
    MessageTooLarge,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Message {
    jsonrpc: String,
    #[serde(default)]
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Deserialize)]
struct Initialize {
    #[serde(rename = "protocolVersion")]
    protocol_version: String,
    capabilities: serde_json::Map<String, Value>,
    #[serde(rename = "clientInfo")]
    client_info: ClientInfo,
}

#[derive(Deserialize)]
struct ClientInfo {
    name: String,
    version: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Call {
    name: String,
    #[serde(default = "empty_object")]
    arguments: Value,
    #[serde(rename = "_meta", default)]
    meta: Option<Value>,
}

fn empty_object() -> Value {
    json!({})
}

pub(crate) fn catalog() -> Vec<Value> {
    let mut catalog = tools::catalog();
    for (name, description, schema, read_only) in [
        (
            "poll",
            "Read progress and inbox together without acknowledging messages. Pass the returned cursor unchanged on the next call; omit it to replay from the start. Drains up to 16 progress pages and 100 changes, including empty pages with has_more. Repeat while has_more. Set include_progress=false without task:read. wait_seconds defaults to zero and is at most five.",
            schemars::schema_for!(client::Poll),
            true,
        ),
        (
            "inbox_handled",
            "Acknowledge explicitly handled message IDs. Partial handling is allowed for a prefix of the unacknowledged inbox; skipping an unhandled message fails safely. Reuse operation_id and identical arguments on retry.",
            schemars::schema_for!(client::Handled),
            false,
        ),
        (
            "retry_mutation",
            "Resend a saved mutation with its original operation ID and identical arguments. Available across supervisor reconnections in this bridge. If the bridge restarted or the saved successful request expired, repeat the original tool and arguments with the same ID.",
            schemars::schema_for!(client::Retry),
            false,
        ),
    ] {
        catalog.push(json!({"name":name,"description":description,"inputSchema":schema,"annotations":{"readOnlyHint":read_only,"destructiveHint":!read_only,"idempotentHint":true,"openWorldHint":false}}));
    }
    catalog.push(json!({
        "name": "new_operation_id",
        "description": "Allocate one operation ID before a mutation. Save it and reuse it with identical arguments when retrying an uncertain outcome.",
        "inputSchema": schemars::schema_for!(tools::Empty),
        "annotations": {"readOnlyHint": true, "destructiveHint": false, "idempotentHint": false, "openWorldHint": false}
    }));
    catalog
}

/// Missing credentials always fail; this entrypoint never discovers an operator run.
pub(crate) async fn run() -> Result<(), SupervisorError> {
    let mut client = crate::supervisor::connect_from_agent_environment()
        .await?
        .ok_or(SupervisorError::IncompleteAgentEnvironment)?;
    // The socket handshake establishes run identity; Whoami also authenticates
    // the token and checks that its session generation is still current.
    client.request(RpcRequest::Whoami).await?;
    serve(
        BufReader::new(tokio::io::stdin()),
        tokio::io::stdout(),
        client,
    )
    .await
}

async fn serve<R: AsyncBufRead + Unpin, W: AsyncWrite + Unpin>(
    mut input: R,
    mut output: W,
    client: SupervisorClient,
) -> Result<(), SupervisorError> {
    let mut client = client::Client::new(client);
    let mut initialized = false;
    let mut ready = false;
    let mut registered_thread: Option<String> = None;
    loop {
        let mut frame = Vec::new();
        let count = (&mut input)
            .take((MAXIMUM_MESSAGE_BYTES + 1) as u64)
            .read_until(b'\n', &mut frame)
            .await
            .map_err(McpError::from)?;
        if count == 0 {
            return Ok(());
        }
        if count > MAXIMUM_MESSAGE_BYTES {
            return Err(McpError::MessageTooLarge.into());
        }
        let value = match serde_json::from_slice::<Value>(&frame) {
            Ok(value) => value,
            Err(_) => {
                write(&mut output, error(Value::Null, -32700, "Invalid JSON."))
                    .await?;
                continue;
            }
        };
        if value.get("id").is_some_and(|id| !valid_id(id)) {
            write(
                &mut output,
                error(Value::Null, -32600, "Invalid request ID."),
            )
            .await?;
            continue;
        }
        let message = match serde_json::from_value::<Message>(value) {
            Ok(message)
                if message.jsonrpc == "2.0"
                    && message.id.as_ref().is_none_or(valid_id) =>
            {
                message
            }
            _ => {
                write(
                    &mut output,
                    error(Value::Null, -32600, "Invalid JSON-RPC request."),
                )
                .await?;
                continue;
            }
        };
        let Some(id) = message.id else {
            if message.method == "notifications/initialized" && initialized {
                ready = true;
            }
            // Unknown notifications have no response and cannot execute a tool.
            continue;
        };
        let result = match message.method.as_str() {
            "ping" => Ok(json!({})),
            "initialize" if !initialized => {
                match serde_json::from_value::<Initialize>(message.params) {
                    Ok(parameters)
                        if !parameters.protocol_version.is_empty()
                            && !parameters.client_info.name.is_empty()
                            && !parameters.client_info.version.is_empty() =>
                    {
                        let _capabilities = parameters.capabilities;
                        initialized = true;
                        Ok(json!({
                            "protocolVersion": PROTOCOL_VERSION,
                            "capabilities": {"tools": {"listChanged": false}},
                            "serverInfo": {"name": "coterie", "version": env!("CARGO_PKG_VERSION")},
                            "instructions": INSTRUCTIONS
                        }))
                    }
                    _ => Err((-32602, "Invalid initialization parameters.")),
                }
            }
            _ if !ready => Err((-32600, "Complete MCP initialization first.")),
            "tools/list" => {
                if !message.params.is_null() && !message.params.is_object() {
                    Err((-32602, "Tool-list parameters must be an object."))
                } else if message
                    .params
                    .get("cursor")
                    .is_some_and(|cursor| !cursor.is_null())
                {
                    Err((-32602, "This catalog has no continuation cursor."))
                } else {
                    Ok(json!({"tools": catalog()}))
                }
            }
            "tools/call" => {
                match serde_json::from_value::<Call>(message.params) {
                    Ok(call) => {
                        if let Some(thread) = call
                            .meta
                            .as_ref()
                            .and_then(|meta| meta.get("threadId"))
                            .and_then(Value::as_str)
                            && crate::protocol::notifications::valid_thread_id(
                                thread,
                            )
                        {
                            if registered_thread
                                .as_deref()
                                .is_some_and(|id| id != thread)
                            {
                                return Err(SupervisorError::InvalidProof);
                            }
                            if registered_thread.is_none()
                                && matches!(client.request(RpcRequest::BindForegroundNotifications {
                                    operation_id: OperationId::generate(), thread_id: thread.to_owned(),
                                }, None).await, Ok(crate::protocol::RpcResponse::ForegroundNotifications {
                                    availability: crate::protocol::notifications::NotificationAvailability::Automatic,
                                    ..
                                }))
                            { registered_thread = Some(thread.to_owned()); }
                        }
                        call_tool(&mut client, call).await
                    }
                    Err(_) => Err((-32602, "Invalid tool-call parameters.")),
                }
            }
            _ => Err((-32601, "Unsupported MCP method.")),
        };
        let response = match result {
            Ok(result) => json!({"jsonrpc":"2.0", "id":id, "result":result}),
            Err((code, message)) => error(id, code, message),
        };
        write(&mut output, response).await?;
    }
}

fn valid_id(id: &Value) -> bool {
    id.is_string() || id.is_i64() || id.is_u64()
}

async fn request_reconnecting(
    client: &mut SupervisorClient,
    request: RpcRequest,
) -> Result<crate::protocol::RpcResponse, SupervisorError> {
    let result = client.request(request.clone()).await;
    if matches!(&result, Err(error) if error.is_transient_connection_failure())
    {
        let Some(mut replacement) =
            crate::supervisor::connect_from_agent_environment().await?
        else {
            return result;
        };
        if replacement.run_id() != client.run_id() {
            return Err(SupervisorError::InvalidProof);
        }
        replacement.request(RpcRequest::Whoami).await?;
        // Mutations retain their original operation IDs and arguments. A lost
        // response therefore cannot turn reconnection into a second mutation.
        let result = replacement.request(request).await;
        *client = replacement;
        return result;
    }
    result
}

async fn call_tool(
    client: &mut client::Client<SupervisorClient>,
    call: Call,
) -> Result<Value, (i32, &'static str)> {
    if call.name == "new_operation_id" {
        serde_json::from_value::<tools::Empty>(call.arguments)
            .map_err(|_| (-32602, "This tool accepts no arguments."))?;
        return Ok(tool_result(
            json!({"operation_id": OperationId::generate()}),
            false,
        ));
    }
    let operation_id = call.arguments.get("operation_id").and_then(|value| {
        serde_json::from_value::<OperationId>(value.clone()).ok()
    });
    let result = match call.name.as_str() {
        "poll" => {
            let arguments =
                serde_json::from_value::<client::Poll>(call.arguments)
                    .map_err(|_| {
                        (-32602, "Arguments do not match this tool's schema.")
                    })?;
            match client.poll(arguments).await {
                Ok(page) => {
                    return Ok(tool_result(
                        json!({"schema_version":1,"data":page}),
                        false,
                    ));
                }
                Err(error) => Err(error),
            }
        }
        "inbox_handled" => {
            let arguments =
                serde_json::from_value::<client::Handled>(call.arguments)
                    .map_err(|_| {
                        (-32602, "Arguments do not match this tool's schema.")
                    })?;
            client.handled(arguments).await
        }
        "retry_mutation" => {
            let arguments =
                serde_json::from_value::<client::Retry>(call.arguments)
                    .map_err(|_| {
                        (-32602, "Arguments do not match this tool's schema.")
                    })?;
            client.retry(arguments.operation_id).await
        }
        _ => {
            let request = tools::request(&call.name, call.arguments)
                .map_err(|message| (-32602, message))?;
            client.request(request, operation_id).await
        }
    };
    match result {
        Ok(mut response) => {
            if let crate::protocol::RpcResponse::Prime { page } = &mut response
            {
                for command in page.commands.iter_mut() {
                    *command = command.replace(' ', "_");
                }
                page.commands.extend([
                    "inbox_acknowledge".to_owned(),
                    "new_operation_id".to_owned(),
                    "poll".to_owned(),
                    "inbox_handled".to_owned(),
                    "retry_mutation".to_owned(),
                    "notification_received".to_owned(),
                ]);
            }
            Ok(tool_result(
                json!({"schema_version": 1, "data": response}),
                false,
            ))
        }
        Err(failure) => {
            let mut encoded = Vec::new();
            crate::cli::render_json_error(
                &mut std::io::sink(),
                &mut encoded,
                &match operation_id {
                    Some(id) => failure.diagnostic().for_operation(id),
                    None => failure.diagnostic(),
                },
            )
            .expect("an in-memory diagnostic can be serialized");
            Ok(tool_result(
                serde_json::from_slice(&encoded)
                    .expect("the diagnostic is JSON"),
                true,
            ))
        }
    }
}

fn tool_result(mut value: Value, failed: bool) -> Value {
    // Keep structured and textual results identical, including redaction.
    redact(&mut value);
    let text = value.to_string();
    json!({"content":[{"type":"text", "text":text}], "structuredContent":value, "isError":failed})
}

fn redact(value: &mut Value) {
    match value {
        Value::String(text) => *text = crate::redaction::text(text),
        Value::Array(values) => values.iter_mut().for_each(redact),
        Value::Object(values) => {
            *values = std::mem::take(values)
                .into_iter()
                .map(|(key, mut value)| {
                    redact(&mut value);
                    (crate::redaction::text(&key), value)
                })
                .collect();
        }
        _ => {}
    }
}

fn error(id: Value, code: i32, message: &str) -> Value {
    json!({"jsonrpc":"2.0", "id":id, "error":{"code":code,"message":message}})
}

async fn write<W: AsyncWrite + Unpin>(
    output: &mut W,
    value: Value,
) -> Result<(), McpError> {
    let mut bytes = value.to_string().into_bytes();
    // A result includes the bounded RPC output in both text and structured form.
    if bytes.len() > 4 * MAXIMUM_MESSAGE_BYTES {
        return Err(McpError::MessageTooLarge);
    }
    bytes.push(b'\n');
    output.write_all(&bytes).await?;
    output.flush().await?;
    Ok(())
}

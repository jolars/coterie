//! Provider-supported agent transport configuration; credentials never enter argv.

use super::*;

pub(super) const AGENT_ENVIRONMENT: &[&str] = &[
    "COTERIE_PROJECT_ID",
    "COTERIE_PRIMARY_PROJECT_ROOT",
    "COTERIE_RUN_ID",
    "COTERIE_AGENT_ID",
    "COTERIE_SESSION_ID",
    "COTERIE_SOCKET",
    "COTERIE_TOKEN",
];

pub(super) fn server_name(scope: SessionScope) -> String {
    format!("coterie_{}", scope.session_id)
}

pub(super) fn configure(
    command: &mut Command,
    specification: &LaunchSpecification,
    executable: &Path,
) -> Result<(), ProviderError> {
    let executable =
        executable.to_str().ok_or(ProviderError::McpConfiguration)?;
    // One server identity per generation prevents a catalog cached for another
    // session from establishing this session's transport readiness.
    let server = server_name(specification.scope);
    let names: Vec<_> = crate::mcp::catalog()
        .into_iter()
        .map(|tool| tool["name"].as_str().expect("tools have names").to_owned())
        .collect();
    let fields = [
        ("command", serde_json::json!(executable)),
        ("args", serde_json::json!(["__mcp"])),
        ("env_vars", serde_json::json!(AGENT_ENVIRONMENT)),
        ("enabled", serde_json::json!(true)),
        ("required", serde_json::json!(true)),
        ("enabled_tools", serde_json::json!(names)),
        ("disabled_tools", serde_json::json!([])),
        ("startup_timeout_sec", serde_json::json!(10)),
        ("tool_timeout_sec", serde_json::json!(60)),
    ];
    let mut table = fields
        .into_iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join(",");
    // These fixed RPC tools implement authority already granted by Coterie's
    // role policy. Provider shell approvals cannot authorize agent RPCs, and
    // a headless provider cannot ask for an additional MCP confirmation.
    // Preauthorize only this catalog; every call still authenticates at the
    // supervisor, and provider/managed policy may impose stricter restrictions.
    let approvals = names
        .iter()
        .map(|name| format!("{name}={{approval_mode=\"approve\"}}"))
        .collect::<Vec<_>>()
        .join(",");
    table.push_str(&format!(",tools={{{approvals}}}"));
    command
        .arg("--config")
        .arg(format!("mcp_servers.{server}={{{table}}}"));
    Ok(())
}

pub(super) fn probe_arguments() -> Vec<String> {
    vec![
        "--config".to_owned(),
        "mcp_servers.coterie_probe={command=\"coterie\",args=[\"__mcp\"],env_vars=[\"COTERIE_TOKEN\"],enabled=true,required=true}".to_owned(),
        "mcp".to_owned(), "get".to_owned(), "coterie_probe".to_owned(), "--json".to_owned(),
    ]
}

pub(super) fn supports_transport(output: &[u8]) -> bool {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(output) else {
        return false;
    };
    value["enabled"] == true
        && value["transport"]["type"] == "stdio"
        && value["transport"]["command"] == "coterie"
        && value["transport"]["args"] == serde_json::json!(["__mcp"])
        && value["transport"]["env_vars"]
            == serde_json::json!(["COTERIE_TOKEN"])
}

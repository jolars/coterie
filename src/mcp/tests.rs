use super::*;
use serde_json::json;

#[test]
fn tools_have_typed_arguments_and_exclude_operator_authority() {
    let tools = tools::catalog();
    assert!(tools.iter().any(|tool| tool["name"] == "prime"));
    assert!(tools.iter().any(|tool| tool["name"] == "task_create"));
    for name in [
        "shutdown",
        "doctor",
        "launch_foreground",
        "exec",
        "read_file",
    ] {
        assert!(tools::request(name, json!({})).is_err());
        assert!(!tools.iter().any(|tool| tool["name"] == name));
    }
    assert!(tools::request("prime", json!({"token": "forged"})).is_err());
    assert!(
        tools::request(
            "task_close",
            json!({
                "operation_id": crate::id::OperationId::generate(),
                "task_id": crate::id::TaskId::generate(),
                "summary": "Reviewed.",
                "operator_override": {}
            })
        )
        .is_err()
    );
    assert!(
        tools::request("task_create", json!({"title":"missing fields"}))
            .is_err()
    );
}

#[test]
fn mutation_retries_preserve_the_supplied_operation_id() {
    let operation_id = crate::id::OperationId::generate();
    let arguments = json!({
        "operation_id": operation_id,
        "recipient": "coordinator",
        "message": "Ready for review."
    });
    let first = tools::request("send", arguments.clone()).unwrap();
    let retry = tools::request("send", arguments).unwrap();
    assert_eq!(first, retry);
    assert!(
        matches!(first, RpcRequest::Send { operation_id: id, .. } if id == operation_id)
    );
}

#[test]
fn mcp_catalog_matches_the_generated_contract() {
    let expected: Value =
        serde_json::from_str(include_str!("../../schemas/mcp-tools-v1.json"))
            .unwrap();
    assert_eq!(json!({"schema_version": 1, "tools": catalog()}), expected);
}

#[test]
#[ignore = "explicit developer command to regenerate the reviewed MCP tool catalog"]
fn regenerate_mcp_catalog() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("schemas/mcp-tools-v1.json");
    let value = json!({"schema_version": 1, "tools": catalog()});
    std::fs::write(
        path,
        format!("{}\n", serde_json::to_string_pretty(&value).unwrap()),
    )
    .unwrap();
}

#[test]
fn mcp_results_redact_credentials_without_corrupting_json() {
    let token = format!("cot1_{}", "ab".repeat(32));
    let result = tool_result(
        json!({"message": format!("A quoted \"{token}\"\nvalue."), "nested": [{token.clone(): "secret key"}]}),
        false,
    );
    assert!(!result.to_string().contains(&token));
    assert_eq!(
        serde_json::from_str::<Value>(
            result["content"][0]["text"].as_str().unwrap()
        )
        .unwrap(),
        result["structuredContent"]
    );
}

#[test]
fn integration_strategy_is_optional_and_validated() {
    use crate::workspace::IntegrationStrategy;
    let arguments = json!({"operation_id": crate::id::OperationId::generate(), "assignment_id": crate::id::AssignmentId::generate()});
    assert!(matches!(
        tools::request("workspace_integrate", arguments.clone()).unwrap(),
        RpcRequest::WorkspaceIntegrate { strategy: None, .. }
    ));
    for (name, expected) in [
        ("rebase", IntegrationStrategy::Rebase),
        ("merge", IntegrationStrategy::Merge),
    ] {
        let mut arguments = arguments.clone();
        arguments["strategy"] = json!(name);
        let request = tools::request("workspace_integrate", arguments).unwrap();
        assert!(
            matches!(request, RpcRequest::WorkspaceIntegrate { strategy: Some(strategy), .. } if strategy == expected)
        );
        let wire = serde_json::to_vec(&request).unwrap();
        assert_eq!(
            serde_json::from_slice::<RpcRequest>(&wire).unwrap(),
            request
        );
    }
    let mut invalid = arguments;
    invalid["strategy"] = json!("unknown");
    assert!(tools::request("workspace_integrate", invalid).is_err());
}

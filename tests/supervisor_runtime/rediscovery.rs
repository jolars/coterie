use super::*;

fn captured_bridge(fixture: &TestEnvironment) -> (String, String) {
    let capture = fs::read(fixture.root.join("mcp-agent-environment")).unwrap();
    let configuration = capture
        .split(|byte| *byte == 0)
        .find_map(|record| record.strip_prefix(b"arg=mcp_servers."))
        .expect("the production launch must configure its session bridge");
    let configuration = std::str::from_utf8(configuration).unwrap();
    let (server, _) = configuration.split_once('=').unwrap();
    (server.to_owned(), format!("mcp_servers.{configuration}"))
}

fn replace_lead(
    fixture: &TestEnvironment,
    lead: &mut Child,
) -> (Child, Vec<(String, String)>) {
    lead.stdin.take();
    assert!(lead.wait().unwrap().success());
    // The capture path is a readiness signal, so the next launch must publish it.
    fs::remove_file(fixture.root.join("mcp-agent-environment")).unwrap();
    live_lead(fixture)
}

fn checked_prime(response: Value, environment: &[(String, String)]) -> Value {
    assert_eq!(response["result"]["isError"], false);
    let data = response["result"]["structuredContent"]["data"].clone();
    for (field, variable) in [
        ("run_id", "COTERIE_RUN_ID"),
        ("agent_id", "COTERIE_AGENT_ID"),
        ("session_id", "COTERIE_SESSION_ID"),
    ] {
        assert_eq!(
            data["session"][field],
            environment_value(environment, variable)
        );
    }
    assert_eq!(data["identity"]["run_id"], data["session"]["run_id"]);
    assert_eq!(data["identity"]["agent"]["id"], data["session"]["agent_id"]);
    data
}

#[test]
fn replacement_catalog_restores_identity_and_rejects_stale_credentials() {
    let fixture = TestEnvironment::new();
    let (mut lead, old_environment) = live_lead(&fixture);
    let _run = StopRun(&fixture);
    let (old_server, _) = captured_bridge(&fixture);
    let mut old = McpClient::start(fixture.agent_command(&old_environment));
    old.initialize();
    let before = checked_prime(old.call("prime", json!({})), &old_environment);
    assert_eq!(before["notifications"], "unavailable");

    // Reopening only the transport does not replace the authenticated session.
    drop(old);
    let mut old = McpClient::start(fixture.agent_command(&old_environment));
    old.initialize();
    let reopened =
        checked_prime(old.call("prime", json!({})), &old_environment);
    assert_eq!(reopened["session"], before["session"]);
    assert_eq!(reopened["notifications"], "unavailable");
    assert_eq!(captured_bridge(&fixture).0, old_server);

    let task =
        fixture.run_json(&["task", "create", "Survives reconnect", "--json"]);
    let (mut lead, current_environment) = replace_lead(&fixture, &mut lead);
    let (current_server, _) = captured_bridge(&fixture);
    assert_ne!(current_server, old_server);
    for (server, environment) in [
        (&old_server, &old_environment),
        (&current_server, &current_environment),
    ] {
        assert_eq!(
            server,
            &format!(
                "coterie_{}",
                environment_value(environment, "COTERIE_SESSION_ID")
            )
        );
    }

    let stale = old.call("prime", json!({}));
    assert_eq!(stale["result"]["isError"], true);
    assert_eq!(
        stale["result"]["structuredContent"]["error"]["code"],
        "unauthenticated"
    );

    for (scope, token_source) in [
        (&old_environment, &old_environment),
        (&current_environment, &old_environment),
        (&old_environment, &current_environment),
    ] {
        let output = fixture
            .agent_command(scope)
            .env(
                "COTERIE_TOKEN",
                environment_value(token_source, "COTERIE_TOKEN"),
            )
            .arg("__mcp")
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(6));
        assert!(
            output.stdout.is_empty(),
            "stale credentials must expose no catalog"
        );
        let diagnostic = String::from_utf8_lossy(&output.stderr);
        assert!(diagnostic.contains("Unauthenticated"));
        for environment in [&old_environment, &current_environment] {
            assert!(
                !diagnostic
                    .contains(&environment_value(environment, "COTERIE_TOKEN"))
            );
        }
    }

    let mut current =
        McpClient::start(fixture.agent_command(&current_environment));
    current.initialize();
    let catalog = current.request("tools/list", json!({}));
    let prime = catalog["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "prime")
        .expect("rediscovery must advertise prime on the current bridge");
    let after = checked_prime(
        current.call(prime["name"].as_str().unwrap(), json!({})),
        &current_environment,
    );
    assert_eq!(after["session"]["run_id"], before["session"]["run_id"]);
    assert_eq!(after["session"]["agent_id"], before["session"]["agent_id"]);
    assert_ne!(
        after["session"]["session_id"],
        before["session"]["session_id"]
    );
    assert_eq!(
        after["session"]["generation"].as_u64().unwrap(),
        before["session"]["generation"].as_u64().unwrap() + 1
    );
    assert!(
        after["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["id"] == task["data"]["task"]["id"])
    );
    assert_eq!(after["notifications"], "unavailable");
    assert_eq!(
        current.call(
            "prime",
            json!({"session_id":before["session"]["session_id"]})
        )["error"]["code"],
        -32602
    );
    lead.stdin.take();
    assert!(lead.wait().unwrap().success());
}

fn codex_host(
    fixture: &TestEnvironment,
    environment: &[(String, String)],
    home: &Path,
) -> (McpClient, Value) {
    let (_, configuration) = captured_bridge(fixture);
    let mut command = Command::new("codex");
    command
        .args([
            "--config",
            &configuration,
            "--config",
            "web_search=\"disabled\"",
            "app-server",
        ])
        .envs(environment.iter().cloned())
        .env("CODEX_HOME", home)
        .current_dir(&fixture.project);
    let mut host = McpClient::start_raw(command);
    assert!(host.request("initialize", json!({"clientInfo":{"name":"coterie-rediscovery","version":"1"},"capabilities":{"experimentalApi":true}})).get("result").is_some());
    host.send(json!({"method":"initialized"}));
    let started = host.request("thread/start", json!({"cwd":fixture.project,"sandbox":"read-only","approvalPolicy":"never","ephemeral":true,"experimentalRawEvents":false}));
    assert!(
        started.get("error").is_none(),
        "thread startup failed: {started}"
    );
    let thread = started["result"]["thread"]["id"].clone();
    assert!(thread.is_string());
    (host, thread)
}

#[test]
#[ignore = "requires explicit opt-in and installed authenticated Codex; exercises stale host identifiers and discovery without model work"]
fn installed_codex_rediscovers_bridge_after_session_replacement() {
    let fixture = TestEnvironment::new();
    let (mut lead, old_environment) = live_lead(&fixture);
    let _run = StopRun(&fixture);
    let home = isolated_authentication(&fixture);
    let (old_server, _) = captured_bridge(&fixture);
    let (mut old_host, old_thread) =
        codex_host(&fixture, &old_environment, &home);
    let old_arguments = json!({"threadId":old_thread,"server":old_server,"tool":"prime","arguments":{}});
    let before = checked_prime(
        old_host.request("mcpServer/tool/call", old_arguments.clone()),
        &old_environment,
    );

    let (mut lead, current_environment) = replace_lead(&fixture, &mut lead);
    let (mut host, thread) = codex_host(&fixture, &current_environment, &home);
    let stale_identifier = host.request("mcpServer/tool/call", json!({"threadId":thread,"server":old_server,"tool":"prime","arguments":{}}));
    assert!(
        stale_identifier.get("error").is_some(),
        "the old server identifier must not route to the new bridge: {stale_identifier}"
    );

    let inventory = host.request(
        "mcpServerStatus/list",
        json!({"threadId":thread,"limit":100}),
    );
    let servers = inventory["result"]["data"].as_array().unwrap();
    assert!(!servers.iter().any(|entry| entry["name"] == old_server));
    let current = servers
        .iter()
        .find(|entry| {
            entry["tools"]
                .as_object()
                .is_some_and(|tools| tools.contains_key("prime"))
        })
        .expect("the host must rediscover the current bridge and prime");
    assert_eq!(current["name"], captured_bridge(&fixture).0);
    let after = checked_prime(host.request("mcpServer/tool/call", json!({"threadId":thread,"server":current["name"],"tool":"prime","arguments":{}})), &current_environment);
    assert_eq!(after["session"]["run_id"], before["session"]["run_id"]);
    assert_eq!(after["session"]["agent_id"], before["session"]["agent_id"]);
    assert_ne!(
        after["session"]["session_id"],
        before["session"]["session_id"]
    );
    assert_eq!(after["notifications"], "unavailable");
    let stale_credentials =
        old_host.request("mcpServer/tool/call", old_arguments);
    assert_eq!(stale_credentials["result"]["isError"], true);
    assert_eq!(
        stale_credentials["result"]["structuredContent"]["error"]["code"],
        "unauthenticated"
    );
    lead.stdin.take();
    assert!(lead.wait().unwrap().success());
}

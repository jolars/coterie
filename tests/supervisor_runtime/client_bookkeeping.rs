use super::*;
use std::io::Read;
use std::os::unix::net::{UnixListener, UnixStream};

fn data(response: Value) -> Value {
    assert_eq!(response["result"]["isError"], false, "{response}");
    let structured = &response["result"]["structuredContent"];
    assert_eq!(
        serde_json::from_str::<Value>(
            response["result"]["content"][0]["text"].as_str().unwrap()
        )
        .unwrap(),
        *structured
    );
    structured["data"].clone()
}

#[test]
fn poll_and_partial_handling_survive_bridge_replacement_without_accepting_tasks()
 {
    let fixture = TestEnvironment::new();
    let (mut lead, environment) = live_lead(&fixture);
    let _run = StopRun(&fixture);
    let mut client = McpClient::start(fixture.agent_command(&environment));
    client.initialize();
    let prime = data(client.call("prime", json!({})));
    let name = prime["identity"]["agent"]["name"].as_str().unwrap();
    for text in ["First", "Second", "Third"] {
        fixture.run_json(&["send", name, text, "--json"]);
    }
    let task =
        fixture.run_json(&["task", "create", "Explicit acceptance", "--json"]);
    let events_before = fixture.run_json(&["events", "--json"]);
    let first = data(client.call("poll", json!({})));
    assert_eq!(first["messages"].as_array().unwrap().len(), 3);
    assert_eq!(first["cursor"]["inbox"], 0);
    assert!(!first["changes"].as_array().unwrap().is_empty());
    assert_eq!(fixture.run_json(&["events", "--json"]), events_before);
    let pending = data(client.call("poll", json!({"cursor":first["cursor"]})));
    assert_eq!(pending["messages"], first["messages"]);
    assert!(pending["changes"].as_array().unwrap().is_empty());
    let skipped = client.call("inbox_handled", json!({"operation_id":format!("co-{}",ulid::Ulid::generate()),"message_ids":[first["messages"][2]["id"]]}));
    assert_eq!(
        skipped["result"]["structuredContent"]["error"]["code"],
        "conflict"
    );
    let operation_id = format!("co-{}", ulid::Ulid::generate());
    let arguments = json!({"operation_id":operation_id,"message_ids":[first["messages"][0]["id"]]});
    let handled = data(client.call("inbox_handled", arguments.clone()));
    assert_eq!(handled["acknowledged_through"], 1);
    assert_eq!(handled["acknowledged_count"], 1);
    assert_eq!(
        data(
            client.call("retry_mutation", json!({"operation_id":operation_id}))
        ),
        handled
    );
    drop(client);

    let mut client = McpClient::start(fixture.agent_command(&environment));
    client.initialize();
    assert_eq!(
        client.call("retry_mutation", json!({"operation_id":operation_id}))["result"]
            ["structuredContent"]["error"]["code"],
        "not_found"
    );
    assert_eq!(data(client.call("inbox_handled", arguments)), handled);
    let next = data(client.call("poll", json!({"cursor":pending["cursor"]})));
    assert_eq!(next["cursor"]["inbox"], 1);
    assert_eq!(
        next["messages"].as_array().unwrap(),
        &first["messages"].as_array().unwrap()[1..]
    );
    data(client.call("inbox_handled", json!({"operation_id":format!("co-{}",ulid::Ulid::generate()),"message_ids":[first["messages"][1]["id"],first["messages"][2]["id"]]})));
    let empty = data(client.call("poll", json!({"cursor":next["cursor"]})));
    assert!(empty["messages"].as_array().unwrap().is_empty());
    assert_eq!(empty["cursor"]["inbox"], 3);
    assert_eq!(
        fixture.run_json(&["prime", "--json"])["data"]["tasks"][0]["status"],
        task["data"]["task"]["status"]
    );

    let mut wrong = next["cursor"].clone();
    wrong["run_id"] = json!(format!("cr-{}", ulid::Ulid::generate()));
    assert_eq!(
        client.call("poll", json!({"cursor":wrong}))["result"]["structuredContent"]
            ["error"]["code"],
        "invalid_argument"
    );
    lead.stdin.take();
    assert!(lead.wait().unwrap().success());
    for (tool, arguments) in [
        ("poll", json!({"cursor":empty["cursor"]})),
        ("retry_mutation", json!({"operation_id":operation_id})),
        (
            "inbox_handled",
            json!({"operation_id":operation_id,"message_ids":[first["messages"][0]["id"]]}),
        ),
    ] {
        assert_eq!(
            client.call(tool, arguments)["result"]["structuredContent"]["error"]
                ["code"],
            "unauthenticated"
        );
    }
    fs::remove_file(fixture.root.join("mcp-agent-environment")).unwrap();
    let (mut replacement, current_environment) = live_lead(&fixture);
    let mut current =
        McpClient::start(fixture.agent_command(&current_environment));
    current.initialize();
    let resumed = data(current.call("poll", json!({"cursor":empty["cursor"]})));
    assert!(resumed["messages"].as_array().unwrap().is_empty());
    assert_eq!(resumed["cursor"]["agent_id"], empty["cursor"]["agent_id"]);
    assert_ne!(
        environment_value(&environment, "COTERIE_SESSION_ID"),
        environment_value(&current_environment, "COTERIE_SESSION_ID")
    );
    replacement.stdin.take();
    assert!(replacement.wait().unwrap().success());
}

fn read_frame(stream: &mut UnixStream) -> std::io::Result<Vec<u8>> {
    let mut header = [0; 4];
    stream.read_exact(&mut header)?;
    let length = u32::from_be_bytes(header) as usize;
    assert!(length <= 1024 * 1024);
    let mut frame = vec![0; length + 4];
    frame[..4].copy_from_slice(&header);
    stream.read_exact(&mut frame[4..])?;
    Ok(frame)
}

#[test]
fn mutations_reconnect_and_retry_identically_before_and_after_commit() {
    for (before_dispatch, dropped_responses) in
        [(true, 1), (false, 1), (false, 2)]
    {
        let fixture = TestEnvironment::new();
        let (mut lead, environment) = live_lead(&fixture);
        let _run = StopRun(&fixture);
        let socket = fixture.root.join("proxy.sock");
        fs::set_permissions(&fixture.root, fs::Permissions::from_mode(0o700))
            .unwrap();
        let listener = UnixListener::bind(&socket).unwrap();
        fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))
            .unwrap();
        let upstream_path = environment_value(&environment, "COTERIE_SOCKET");
        let proxy = thread::spawn(move || {
            let mut mutations = Vec::new();
            for connection in 0..=dropped_responses {
                let (mut bridge, _) = listener.accept().unwrap();
                bridge
                    .set_read_timeout(Some(Duration::from_secs(20)))
                    .unwrap();
                let mut upstream = UnixStream::connect(&upstream_path).unwrap();
                upstream
                    .set_read_timeout(Some(Duration::from_secs(20)))
                    .unwrap();
                loop {
                    let frame = read_frame(&mut bridge).unwrap();
                    let request: Value =
                        serde_json::from_slice(&frame[4..]).unwrap();
                    let mutation =
                        request["body"]["request"]["method"] == "task_create";
                    if mutation {
                        mutations.push(request["body"]["request"].clone());
                        if before_dispatch && connection < dropped_responses {
                            break;
                        }
                    }
                    upstream.write_all(&frame).unwrap();
                    let response = read_frame(&mut upstream).unwrap();
                    if mutation && connection < dropped_responses {
                        break;
                    }
                    bridge.write_all(&response).unwrap();
                    if mutation {
                        break;
                    }
                }
            }
            mutations
        });
        let mut command = fixture.agent_command(&environment);
        command.env("COTERIE_SOCKET", &socket);
        let mut client = McpClient::start(command);
        client.initialize();
        let operation_id = format!("co-{}", ulid::Ulid::generate());
        let arguments = json!({"operation_id":operation_id,"title":"Exactly once","description":"Preserve this exact request.","project":"primary","group":null,"dependencies":[]});
        let first = client.call("task_create", arguments.clone());
        let result = if dropped_responses == 2 {
            assert_eq!(first["result"]["isError"], true);
            assert_eq!(
                first["result"]["structuredContent"]["operation_id"],
                operation_id
            );
            let mut changed = arguments;
            changed["title"] = json!("Changed request must fail locally");
            assert_eq!(
                client.call("task_create", changed)["result"]["structuredContent"]
                    ["error"]["code"],
                "conflict"
            );
            client.call("retry_mutation", json!({"operation_id":operation_id}))
        } else {
            first
        };
        assert_eq!(data(result)["operation_id"], operation_id);
        let mutations = proxy.join().unwrap();
        assert_eq!(mutations.len(), dropped_responses + 1);
        assert!(mutations.iter().all(|request| request == &mutations[0]));
        assert_eq!(
            fixture.run_json(&["prime", "--json"])["data"]["tasks"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        lead.stdin.take();
        assert!(lead.wait().unwrap().success());
    }
}

#[test]
fn inbox_only_poll_does_not_require_progress_authority() {
    let fixture = TestEnvironment::new();
    write_global(
        &fixture,
        &include_str!("../../examples/config/global.toml").replace(
            "\"spawn:builder\", \"send:*\", \"task:*\", \"logs:*\"",
            "\"send:*\"",
        ),
    );
    let (mut lead, environment) = live_lead(&fixture);
    let _run = StopRun(&fixture);
    let mut client = McpClient::start(fixture.agent_command(&environment));
    client.initialize();
    let cursor =
        data(client.call("poll", json!({"include_progress":false})))["cursor"]
            .clone();
    assert!(cursor["progress"].is_null());
    assert_eq!(
        client.call("poll", json!({"cursor":cursor}))["result"]["structuredContent"]
            ["error"]["code"],
        "permission_denied"
    );
    lead.stdin.take();
    assert!(lead.wait().unwrap().success());
}

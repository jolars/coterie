use super::*;
use serde_json::json;
use std::io::{BufRead, BufReader};
use std::process::{Child, ChildStdin};
use std::sync::mpsc::{Receiver, channel};

struct McpClient {
    child: Child,
    input: Option<ChildStdin>,
    responses: Receiver<Value>,
    next_id: u64,
}

impl McpClient {
    fn start(mut command: Command) -> Self {
        command.arg("__mcp");
        Self::start_raw(command)
    }

    fn start_raw(mut command: Command) -> Self {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let input = child.stdin.take();
        let output = child.stdout.take().unwrap();
        let (sender, responses) = channel();
        thread::spawn(move || {
            for line in BufReader::new(output).lines() {
                let Ok(line) = line else { break };
                let Ok(value) = serde_json::from_str(&line) else {
                    break;
                };
                if sender.send(value).is_err() {
                    break;
                }
            }
        });
        Self {
            child,
            input,
            responses,
            next_id: 1,
        }
    }

    fn send(&mut self, value: Value) {
        writeln!(self.input.as_mut().unwrap(), "{value}").unwrap();
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(
            json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}),
        );
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let response = self
                .responses
                .recv_timeout(
                    deadline.saturating_duration_since(Instant::now()),
                )
                .expect("protocol response timed out or process exited");
            if response["id"] == id {
                return response;
            }
        }
    }

    fn initialize(&mut self) {
        let response = self.request("initialize", json!({"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"coterie-test","version":"1"}}));
        assert_eq!(response["result"]["protocolVersion"], "2025-06-18");
        self.send(
            json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        );
    }

    fn call(&mut self, name: &str, arguments: Value) -> Value {
        self.request("tools/call", json!({"name":name,"arguments":arguments}))
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        self.input.take();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn live_lead(fixture: &TestEnvironment) -> (Child, Vec<(String, String)>) {
    let capture = fixture.root.join("mcp-agent-environment");
    let child = fixture
        .command()
        .env("COTERIE_FAKE_MODE", "contract")
        .env("COTERIE_FAKE_CAPTURE", &capture)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_until("the MCP fixture's live lead", || capture.exists());
    (child, captured_environment(&capture))
}

#[test]
fn mcp_authenticates_forwards_and_replays_agent_operations() {
    let fixture = TestEnvironment::new();
    let (mut lead, environment) = live_lead(&fixture);
    let _run = StopRun(&fixture);
    let mut client = McpClient::start(fixture.agent_command(&environment));
    assert_eq!(client.call("prime", json!({}))["error"]["code"], -32600);
    client.initialize();
    let tools = client.request("tools/list", json!({}));
    assert!(
        tools["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["name"] == "prime")
    );
    let prime = client.call("prime", json!({}));
    assert_eq!(prime["result"]["isError"], false);
    assert_eq!(
        prime["result"]["structuredContent"]["data"]["identity"]["run_id"],
        environment_value(&environment, "COTERIE_RUN_ID")
    );
    let operation = client.call("new_operation_id", json!({}));
    let operation_id =
        &operation["result"]["structuredContent"]["operation_id"];
    let arguments = json!({"operation_id":operation_id,"title":"MCP task","description":"Validate the bridge.","project":"primary","group":null,"dependencies":[]});
    let first = client.call("task_create", arguments.clone());
    assert_eq!(first["result"]["isError"], false);
    let retry = client.call("task_create", arguments.clone());
    let mut conflict = arguments;
    conflict["title"] = json!("A different task");
    let conflict = client.call("task_create", conflict);
    assert_eq!(conflict["result"]["isError"], true);
    assert_eq!(
        &conflict["result"]["structuredContent"]["operation_id"],
        operation_id
    );
    assert_eq!(first["result"], retry["result"]);
    assert_eq!(
        fixture.run_json(&["prime", "--json"])["data"]["tasks"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let uncertain_id = format!("co-{}", ulid::Ulid::generate());
    let uncertain_arguments = json!({"operation_id":uncertain_id,"title":"Lost MCP reply","description":"Replay after reconnect.","project":"primary","group":null,"dependencies":[]});
    client.send(json!({"jsonrpc":"2.0","id":999,"method":"tools/call","params":{"name":"task_create","arguments":uncertain_arguments}}));
    wait_until("the mutation whose MCP reply is discarded", || {
        fixture.run_json(&["prime", "--json"])["data"]["tasks"]
            .as_array()
            .unwrap()
            .len()
            == 2
    });
    drop(client);
    let mut client = McpClient::start(fixture.agent_command(&environment));
    client.initialize();
    let replay = client.call("task_create", uncertain_arguments);
    assert_eq!(replay["result"]["isError"], false);
    assert_eq!(
        fixture.run_json(&["prime", "--json"])["data"]["tasks"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    for name in ["shutdown", "doctor", "launch_foreground", "exec"] {
        assert_eq!(client.call(name, json!({}))["error"]["code"], -32602);
    }
    assert_eq!(
        client.call("prime", json!({"token":"forged"}))["error"]["code"],
        -32602
    );
    let denied = client.call("project_attach", json!({"operation_id":format!("co-{}",ulid::Ulid::generate()),"path":fixture.root,"alias":"forbidden"}));
    assert_eq!(denied["result"]["isError"], true);
    assert_eq!(
        denied["result"]["structuredContent"]["error"]["code"],
        "permission_denied"
    );
    lead.stdin.take().unwrap().write_all(b"done\n").unwrap();
    assert!(lead.wait().unwrap().success());
    let stale = client.call("prime", json!({}));
    assert_eq!(stale["result"]["isError"], true);
    assert_eq!(
        stale["result"]["structuredContent"]["error"]["code"],
        "unauthenticated"
    );
    fixture.run_json(&["stop", "--json"]);
}

#[test]
fn mcp_never_falls_back_to_operator_or_accepts_invalid_credentials() {
    let fixture = TestEnvironment::new();
    let (mut lead, environment) = live_lead(&fixture);
    let _run = StopRun(&fixture);
    for kind in ["absent", "partial", "wrong-token"] {
        let mut command = fixture.command();
        if kind != "absent" {
            command.envs(environment.iter().cloned());
        }
        if kind == "partial" {
            command.env_remove("COTERIE_TOKEN");
        }
        if kind == "wrong-token" {
            command.env("COTERIE_TOKEN", format!("cot1_{}", "00".repeat(32)));
        }
        let output = command.arg("__mcp").output().unwrap();
        assert!(!output.status.success(), "{kind}");
        assert!(
            output.stdout.is_empty(),
            "failed authentication must expose no MCP catalog"
        );
        assert!(
            !String::from_utf8_lossy(&output.stderr)
                .contains(&environment_value(&environment, "COTERIE_TOKEN"))
        );
    }
    lead.stdin.take().unwrap().write_all(b"done\n").unwrap();
    lead.wait().unwrap();
    fixture.run_json(&["stop", "--json"]);
}

#[test]
#[ignore = "requires explicit opt-in and an installed authenticated Codex CLI; uses actual MCP startup and calls without a model"]
fn installed_codex_mcp_routes_authenticated_rpc() {
    assert!(
        Command::new("codex")
            .args(["login", "status"])
            .output()
            .unwrap()
            .status
            .success(),
        "authenticate Codex before opting into provider checks"
    );
    for sandbox in ["workspace-write", "read-only"] {
        let fixture = TestEnvironment::new();
        let (mut lead, environment) = live_lead(&fixture);
        let _run = StopRun(&fixture);
        let home = isolated_authentication(&fixture);
        let capture =
            fs::read(fixture.root.join("mcp-agent-environment")).unwrap();
        let configuration = capture
            .split(|byte| *byte == 0)
            .find_map(|record| record.strip_prefix(b"arg=mcp_servers."))
            .expect("the production launch must inject an MCP server");
        let configuration = std::str::from_utf8(configuration).unwrap();
        let (server, _) = configuration.split_once('=').unwrap();
        let mut command = Command::new("codex");
        command
            .args([
                "--config",
                &format!("mcp_servers.{configuration}"),
                "--config",
                "sandbox_workspace_write.network_access=false",
                "--config",
                "web_search=\"disabled\"",
                "app-server",
            ])
            .envs(environment.iter().cloned())
            .env("CODEX_HOME", &home)
            .current_dir(&fixture.project);
        let mut app = McpClient::start_raw(command);
        assert!(app.request("initialize", json!({"clientInfo":{"name":"coterie-conformance","version":"1"},"capabilities":{"experimentalApi":true}})).get("result").is_some());
        app.send(json!({"method":"initialized"}));
        let started = app.request("thread/start", json!({"cwd":fixture.project,"sandbox":sandbox,"approvalPolicy":"never","ephemeral":true,"experimentalRawEvents":false}));
        assert!(
            started.get("error").is_none(),
            "thread startup failed: {started}"
        );
        let thread_id = &started["result"]["thread"]["id"];
        assert!(thread_id.is_string());
        let inventory = app.request(
            "mcpServerStatus/list",
            json!({"threadId":thread_id, "limit":100}),
        );
        assert!(
            inventory["result"]["data"].as_array().unwrap().iter().any(
                |entry| entry["name"] == server
                    && entry["tools"]
                        .as_object()
                        .is_some_and(|tools| !tools.is_empty())
            ),
            "the real provider must advertise the tool catalog: {inventory}"
        );
        for tool in ["whoami", "prime"] {
            let result = app.request("mcpServer/tool/call", json!({"threadId":thread_id,"server":server,"tool":tool,"arguments":{}}));
            assert!(
                result.get("error").is_none(),
                "{sandbox} MCP call failed: {result}"
            );
            assert_eq!(result["result"]["isError"], false);
        }
        let result = app.request("mcpServer/tool/call", json!({"threadId":thread_id,"server":server,"tool":"project_attach","arguments":{"operation_id":format!("co-{}",ulid::Ulid::generate()),"path":fixture.root,"alias":"forbidden"}}));
        assert_eq!(
            result["result"]["isError"], true,
            "{sandbox} must enforce capability denial: {result}"
        );
        assert_eq!(
            result["result"]["structuredContent"]["error"]["code"],
            "permission_denied"
        );
        lead.stdin.take().unwrap().write_all(b"done\n").unwrap();
        lead.wait().unwrap();
        let stale = app.request("mcpServer/tool/call", json!({"threadId":thread_id,"server":server,"tool":"prime","arguments":{}}));
        assert_eq!(
            stale["result"]["isError"], true,
            "{sandbox} must fence the old session: {stale}"
        );
        drop(app);
        fixture.run_json(&["stop", "--json"]);
    }
}

const POLICY_PROBE: &str = r#"import json, os, socket, sys
results = {}
for name, address, family in [
    ('supervisor', os.environ['COTERIE_SOCKET'], socket.AF_UNIX),
    ('unrelated', sys.argv[1], socket.AF_UNIX),
    ('tcp', ('127.0.0.1', int(sys.argv[2])), socket.AF_INET),
]:
    try:
        with socket.socket(family, socket.SOCK_STREAM) as connection:
            connection.settimeout(0.2)
            connection.connect(address)
        results[name] = True
    except OSError:
        results[name] = False
for name, path in [('workspace_write', os.path.join(os.getcwd(), 'mcp-policy-probe')), ('outside_write', sys.argv[3])]:
    try:
        with open(path, 'x') as output:
            output.write('policy probe\n')
        os.unlink(path)
        results[name] = True
    except OSError:
        results[name] = False
print('COTERIE_POLICY_OBSERVATION=' + json.dumps(results, sort_keys=True))
for name in ['supervisor', 'unrelated', 'tcp', 'outside_write']:
    assert results[name] is False, (name, results)
assert results['workspace_write'] is (sys.argv[4] == 'implementation'), results
"#;

fn isolated_authentication(fixture: &TestEnvironment) -> PathBuf {
    let source = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap()).join(".codex")
        });
    let home = fixture.root.join("real-codex-home");
    fs::DirBuilder::new().mode(0o700).create(&home).unwrap();
    fs::copy(source.join("auth.json"), home.join("auth.json")).expect(
        "this opt-in model test requires local Codex auth.json authentication",
    );
    fs::set_permissions(
        home.join("auth.json"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    home
}

fn installed_codex() -> PathBuf {
    std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|directory| directory.join("codex"))
        .find(|path| path.is_file())
        .expect("install Codex before opting in")
}

#[test]
#[ignore = "requires explicit opt-in, Codex authentication, and model access; exercises actual Coterie job launches"]
fn installed_codex_jobs_use_mcp_and_preserve_the_sandbox() {
    for profile in ["implementation", "inspect"] {
        let fixture = TestEnvironment::new();
        let home = isolated_authentication(&fixture);
        fs::write(
            home.join("config.toml"),
            "[sandbox_workspace_write]\nnetwork_access = true\n",
        )
        .unwrap();
        let global = include_str!("../../examples/config/global.toml")
            .replace(
                "roles.builder]\nprovider = \"codex\"",
                "roles.builder]\nprovider = \"real_codex\"",
            )
            .replace(
                "permission_profile = \"implementation\"",
                &format!("permission_profile = \"{profile}\""),
            );
        let global = format!(
            "{global}\n[providers.real_codex]\ncommand = [{}]\n",
            serde_json::to_string(&installed_codex()).unwrap()
        );
        write_global(&fixture, &global);
        let runtime = PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR").expect("the real test requires XDG_RUNTIME_DIR outside system temp directories"));
        let external =
            runtime.join(format!("ct-mcp-{}", ulid::Ulid::generate()));
        fs::DirBuilder::new().mode(0o700).create(&external).unwrap();
        let _external_cleanup = RemoveDirectory(external.clone());
        let socket = external.join("other.sock");
        let _unix = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let tcp = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let script = external.join("policy.py");
        fs::write(&script, POLICY_PROBE).unwrap();
        let capture = fixture.root.join("mcp-agent-environment");
        let mut lead = fixture
            .command()
            .env("CODEX_HOME", &home)
            .env("COTERIE_FAKE_MODE", "contract")
            .env("COTERIE_FAKE_CAPTURE", &capture)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        wait_until("real Codex test lead", || capture.exists());
        let _run = StopRun(&fixture);
        let instruction = format!(
            "Exercise Coterie's MCP bridge without changing the repository. Use only the Coterie MCP tools for orchestration. Call prime and inbox. Allocate an operation ID and call task_create with title=denied, description=capability test, project=primary, group=null, dependencies=[]; expect permission_denied and do not retry or escalate. Run this exact local policy observation with a non-login shell: python3 {} {} {} {} {profile}. Report its output in your finish summary. Allocate a fresh operation ID and call finish with status=completed. The worktree is clean and needs no commit. Do not perform any other work, change permissions, or print credentials.",
            script.display(),
            socket.display(),
            tcp.local_addr().unwrap().port(),
            external.join("outside-write").display()
        );
        let task = fixture.run_json(&[
            "task",
            "create",
            "Test MCP transport",
            "--description",
            &instruction,
            "--json",
        ]);
        let task_id = task["data"]["task"]["id"].as_str().unwrap();
        let spawned = fixture
            .run_json(&["spawn", "builder", "--task", task_id, "--json"]);
        let name = spawned["data"]["agent"]["name"].as_str().unwrap();
        let deadline = Instant::now() + Duration::from_secs(180);
        let mut transcript = String::new();
        let mut cursor = 0;
        loop {
            let logs = fixture.run_json(&[
                "logs",
                name,
                "--after",
                &cursor.to_string(),
                "--limit",
                "65536",
                "--json",
            ]);
            transcript.push_str(logs["data"]["transcript"].as_str().unwrap());
            cursor = logs["data"]["next_cursor"].as_u64().unwrap();
            if transcript.contains("\"type\":\"turn.completed\"")
                || transcript.contains("\"type\":\"turn.failed\"")
                || (logs["data"]["terminal"] == true
                    && logs["data"]["eof"] == true)
            {
                break;
            }
            if Instant::now() >= deadline {
                let status = fixture.run_json(&["status", "--json"]);
                fixture.run_json(&["stop", "--json"]);
                panic!(
                    "real {profile} Codex job timed out; status: {status}; transcript: {transcript}"
                );
            }
            thread::sleep(Duration::from_millis(250));
        }
        let events: Vec<Value> = transcript
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect();
        let completed: Vec<_> = events
            .iter()
            .filter(|event| event["type"] == "item.completed")
            .map(|event| &event["item"])
            .collect();
        for tool in ["prime", "inbox", "finish"] {
            assert!(
                completed.iter().any(|item| item["type"] == "mcp_tool_call"
                    && item["tool"] == tool
                    && item["status"] == "completed"),
                "{profile}: missing successful {tool}; {transcript}"
            );
        }
        assert!(
            completed.iter().any(|item| item["type"] == "mcp_tool_call"
                && item["tool"] == "task_create"
                && item.to_string().contains("permission_denied")),
            "{profile}: no capability denial; {transcript}"
        );
        // Unified-exec output can arrive after Codex emits the completed JSONL
        // item. The immutable helper asserts the policy itself, so its exact
        // invocation and successful exit prove enforcement even in that case.
        let policy_command = format!(
            "python3 {} {} {} {} {profile}",
            script.display(),
            socket.display(),
            tcp.local_addr().unwrap().port(),
            external.join("outside-write").display()
        );
        assert!(
            completed
                .iter()
                .any(|item| item["type"] == "command_execution"
                    && item["command"].as_str().is_some_and(
                        |command| command.contains(&policy_command)
                    )
                    && item["exit_code"] == 0
                    && item["status"] == "completed"),
            "{profile}: the exact policy assertion must exit successfully; {transcript}"
        );
        lead.stdin.take().unwrap().write_all(b"done\n").unwrap();
        lead.wait().unwrap();
        fixture.run_json(&["stop", "--json"]);
    }
}

struct StopRun<'a>(&'a TestEnvironment);
impl Drop for StopRun<'_> {
    fn drop(&mut self) {
        let _ = self
            .0
            .command()
            .args(["stop", "--json"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

#[test]
fn mcp_bounds_frames_rejects_invalid_requests_and_exits_on_eof() {
    let fixture = TestEnvironment::new();
    let (mut lead, environment) = live_lead(&fixture);
    let _run = StopRun(&fixture);
    let mut client = McpClient::start(fixture.agent_command(&environment));
    client
        .input
        .as_mut()
        .unwrap()
        .write_all(b"{malformed\n")
        .unwrap();
    assert_eq!(
        client
            .responses
            .recv_timeout(Duration::from_secs(5))
            .unwrap()["error"]["code"],
        -32700
    );
    client.send(json!({"jsonrpc":"2.0","id":null,"method":"tools/call","params":{"name":"prime"}}));
    assert_eq!(
        client
            .responses
            .recv_timeout(Duration::from_secs(5))
            .unwrap()["error"]["code"],
        -32600
    );
    client.initialize();
    assert_eq!(
        client.request("tools/list", json!(1))["error"]["code"],
        -32602
    );
    assert_eq!(
        client.request("tools/list", json!({"cursor":"invented"}))["error"]["code"],
        -32602
    );
    client.send(json!({"jsonrpc":"2.0","method":"tools/call","params":{"name":"task_create","arguments":{}}}));
    assert_eq!(client.request("ping", json!({}))["result"], json!({}));
    client.input.take();
    wait_until("MCP EOF shutdown", || {
        client.child.try_wait().unwrap().is_some()
    });
    assert!(client.child.wait().unwrap().success());
    drop(client);
    let mut client = McpClient::start(fixture.agent_command(&environment));
    let _ = client
        .input
        .as_mut()
        .unwrap()
        .write_all(&vec![b' '; 1024 * 1024 + 1]);
    wait_until("oversized MCP frame rejection", || {
        client.child.try_wait().unwrap().is_some()
    });
    assert!(!client.child.wait().unwrap().success());
    lead.stdin.take().unwrap().write_all(b"done\n").unwrap();
    lead.wait().unwrap();
}

#[test]
#[ignore = "requires explicit opt-in, Codex authentication, and model access; launches the actual foreground TUI in a PTY"]
fn installed_codex_foreground_uses_mcp_under_both_profiles() {
    use nix::pty::{Winsize, openpty};
    use nix::unistd::ttyname;
    for profile in ["implementation", "inspect"] {
        let fixture = TestEnvironment::new();
        let home = isolated_authentication(&fixture);
        fs::write(home.join("config.toml"), format!(
            "check_for_update_on_startup = false\n[projects.{}]\ntrust_level = \"trusted\"\n",
            serde_json::to_string(&fixture.project).unwrap()
        )).unwrap();
        let prompt = "Test Coterie's foreground MCP transport. Discover and call the Coterie prime tool. Then call new_operation_id and task_create with title=Foreground MCP verified, description=Authenticated foreground transport verified., project=primary, group=null, dependencies=[]. Do not spawn workers or run shell commands. After the tool succeeds, reply Done and wait.";
        let global = include_str!("../../examples/config/global.toml")
            .replace(
                "command = [\"codex\"]",
                &format!(
                    "command = [{}, \"--no-alt-screen\", {}]",
                    serde_json::to_string(&installed_codex()).unwrap(),
                    serde_json::to_string(prompt).unwrap()
                ),
            )
            .replace(
                "permission_profile = \"interactive\"",
                &format!("permission_profile = \"{profile}\""),
            );
        write_global(&fixture, &global);
        let pty = openpty(
            Some(&Winsize {
                ws_row: 40,
                ws_col: 120,
                ws_xpixel: 0,
                ws_ypixel: 0,
            }),
            None,
        )
        .unwrap();
        let slave = ttyname(&pty.slave).unwrap();
        let mut terminal = fs::File::from(pty.master);
        let mut reader = terminal.try_clone().unwrap();
        let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let capture = captured.clone();
        thread::spawn(move || {
            use std::io::Read;
            let mut bytes = [0; 4096];
            while let Ok(count) = reader.read(&mut bytes) {
                if count == 0 {
                    break;
                }
                let mut capture = capture.lock().unwrap();
                if capture.len() < 1024 * 1024 {
                    capture.extend_from_slice(&bytes[..count]);
                }
            }
        });
        let template = fixture.command();
        let mut command = Command::new(std::env::current_exe().unwrap());
        for (name, value) in template.get_envs() {
            match value {
                Some(value) => {
                    command.env(name, value);
                }
                None => {
                    command.env_remove(name);
                }
            }
        }
        let mut foreground = command
            .args([
                "--exact",
                "terminal::foreground_terminal_child",
                "--nocapture",
            ])
            .current_dir(&fixture.project)
            .env("COTERIE_TEST_TERMINAL", slave)
            .env("CODEX_HOME", &home)
            .env_remove("OPENAI_API_KEY")
            .env("TERM", "xterm-256color")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let _run = StopRun(&fixture);
        // Supply the standard cursor-position response expected during terminal setup.
        thread::sleep(Duration::from_millis(500));
        terminal.write_all(b"\x1b[1;1R").unwrap();
        let deadline = Instant::now() + Duration::from_secs(180);
        loop {
            let output = fixture
                .command()
                .args(["prime", "--json"])
                .output()
                .unwrap();
            if output.status.success() {
                let prime: Value =
                    serde_json::from_slice(&output.stdout).unwrap();
                if prime["data"]["tasks"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|task| task["title"] == "Foreground MCP verified")
                {
                    break;
                }
            }
            if foreground.try_wait().unwrap().is_some()
                || Instant::now() >= deadline
            {
                let screen = String::from_utf8_lossy(&captured.lock().unwrap())
                    .into_owned();
                panic!(
                    "{profile}: the real TUI did not create its MCP task; diagnostic terminal capture: {screen}"
                );
            }
            thread::sleep(Duration::from_millis(250));
        }
        fixture.run_json(&["stop", "--json"]);
        wait_until("real foreground shutdown", || {
            foreground.try_wait().unwrap().is_some()
        });
        foreground.wait().unwrap();
    }
}

#[test]
#[ignore = "requires explicit opt-in and installed authenticated Codex; verifies required MCP startup fails before model work"]
fn installed_codex_rejects_failed_required_mcp_initialization() {
    let fixture = TestEnvironment::new();
    let (mut lead, environment) = live_lead(&fixture);
    let _run = StopRun(&fixture);
    let home = isolated_authentication(&fixture);
    let capture = fs::read(fixture.root.join("mcp-agent-environment")).unwrap();
    let configuration = capture
        .split(|byte| *byte == 0)
        .find_map(|record| record.strip_prefix(b"arg=mcp_servers."))
        .unwrap();
    let configuration = format!(
        "mcp_servers.{}",
        std::str::from_utf8(configuration).unwrap()
    );
    let mut command = Command::new(installed_codex());
    command
        .args(["--config", &configuration, "app-server"])
        .envs(environment.iter().cloned())
        .env("COTERIE_TOKEN", format!("cot1_{}", "00".repeat(32)))
        .env("CODEX_HOME", &home)
        .current_dir(&fixture.project);
    let mut app = McpClient::start_raw(command);
    assert!(
        app.request(
            "initialize",
            json!({"clientInfo":{"name":"coterie-conformance","version":"1"}})
        )
        .get("result")
        .is_some()
    );
    app.send(json!({"method":"initialized"}));
    let started = app.request("thread/start", json!({"cwd":fixture.project,"sandbox":"read-only","approvalPolicy":"never","ephemeral":true}));
    assert!(
        started.get("error").is_some(),
        "required bridge authentication must fail session startup: {started}"
    );
    drop(app);
    let stdout = fixture.root.join("failed-startup.jsonl");
    let stderr = fixture.root.join("failed-startup.stderr");
    let mut exec = Command::new(installed_codex())
        .args([
            "exec",
            "--json",
            "--sandbox",
            "read-only",
            "--config",
            &configuration,
            "Reply with UNEXPECTED_MODEL_WORK.",
        ])
        .envs(environment.iter().cloned())
        .env("COTERIE_TOKEN", format!("cot1_{}", "00".repeat(32)))
        .env("CODEX_HOME", &home)
        .current_dir(&fixture.project)
        .stdin(Stdio::null())
        .stdout(fs::File::create(&stdout).unwrap())
        .stderr(fs::File::create(&stderr).unwrap())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    let status = loop {
        if let Some(status) = exec.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            exec.kill().unwrap();
            exec.wait().unwrap();
            panic!("required MCP startup did not fail within its deadline");
        }
        thread::sleep(Duration::from_millis(20));
    };
    assert!(!status.success());
    let output = fs::read_to_string(stdout).unwrap();
    let diagnostic = fs::read_to_string(stderr).unwrap();
    assert!(
        !output.contains("UNEXPECTED_MODEL_WORK")
            && !output.contains("turn.completed"),
        "failed initialization must prevent model work: {output}"
    );
    assert!(
        format!("{output}{diagnostic}")
            .contains("required MCP servers failed to initialize"),
        "startup must report its actual MCP failure: {output}{diagnostic}"
    );
    lead.stdin.take().unwrap().write_all(b"done\n").unwrap();
    lead.wait().unwrap();
}

struct RemoveDirectory(PathBuf);
impl Drop for RemoveDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

//! Opt-in linked-worktree acceptance through actual provider-launched jobs.

use super::*;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

const CHILD: &str = "mcp::commit_handoff::commit_policy_child";
const CONTENT: &str = "validated worker contribution\n";

#[test]
#[ignore = "helper invoked only by the commit policy fixtures"]
fn commit_policy_child() {
    let manifest = std::env::var_os("COTERIE_COMMIT_PROBE").unwrap();
    let manifest: Value =
        serde_json::from_slice(&fs::read(manifest).unwrap()).unwrap();
    let control = manifest["control"] == true;
    let workspace = std::env::current_dir().unwrap();
    let repository = Repository::open(&workspace).unwrap();
    assert!(repository.is_worktree());
    let content = if let Some(source) = manifest["source"].as_str() {
        fs::read_to_string(Path::new(source).join("contribution.txt")).unwrap()
    } else {
        CONTENT.to_owned()
    };
    fs::write("contribution.txt", &content).unwrap();
    assert_eq!(fs::read_to_string("contribution.txt").unwrap(), CONTENT);
    let mut observed = BTreeMap::new();
    for (name, path) in manifest["protected"].as_object().unwrap() {
        // Opening an existing file for write proves access without damaging it.
        let allowed = fs::OpenOptions::new()
            .write(true)
            .open(path.as_str().unwrap())
            .is_ok();
        observed.insert(name.clone(), allowed);
        assert_eq!(allowed, control, "write access to {name}");
    }
    let mut index = repository.index().unwrap();
    let stage = index
        .add_path(Path::new("contribution.txt"))
        .and_then(|()| index.write());
    observed.insert("stage".to_owned(), stage.is_ok());
    assert_eq!(stage.is_ok(), control, "stage: {stage:?}");
    if let Err(error) = &stage {
        println!("staging denial: {error}");
    }
    // Even committing an existing tree needs protected object/ref writes.
    let parent = repository.head().unwrap().peel_to_commit().unwrap();
    let tree = parent.tree().unwrap();
    let signature =
        Signature::now("Coterie Probe", "probe@example.invalid").unwrap();
    let commit = repository.commit(
        Some("HEAD"),
        &signature,
        &signature,
        "commit policy probe",
        &tree,
        &[&parent],
    );
    // libgit2 1.9.7 can return an OID after an object-write failure. A commit
    // succeeds only when a fresh repository handle observes it at HEAD.
    let reopened = Repository::open(&workspace).unwrap();
    let committed = commit.as_ref().is_ok_and(|oid| {
        reopened.find_commit(*oid).is_ok()
            && reopened.head().unwrap().target() == Some(*oid)
    });
    observed.insert("commit".to_owned(), committed);
    assert_eq!(committed, control, "commit: {commit:?}");
    if !control {
        assert_eq!(reopened.head().unwrap().target(), Some(parent.id()));
    }
    let observation = json!({"validated": true, "writes": observed, "workspace": workspace,
        "stage_error": stage.err().map(|error| error.to_string()),
        "commit_returned_oid": commit.as_ref().ok().map(ToString::to_string),
        "commit_error": commit.err().map(|error| error.to_string())});
    // Codex exec does not emit command_execution items for shell calls inside
    // its code tool. Retain the probe's own result before reporting completion.
    if let Some(path) = manifest["observation"].as_str() {
        fs::write(path, observation.to_string()).unwrap();
    }
    println!("COTERIE_COMMIT_OBSERVATION={observation}");
}

fn sibling(fixture: &TestEnvironment) -> PathBuf {
    let path = fixture.root.join("sibling");
    let repo = Repository::open(&fixture.project).unwrap();
    repo.worktree("sibling", &path, None).unwrap();
    path
}

fn protected(
    fixture: &TestEnvironment,
    sibling: &Path,
    source: Option<&Path>,
) -> Value {
    let repo = Repository::open(&fixture.project).unwrap();
    let other = Repository::open(sibling).unwrap();
    let mut paths = json!({
        "primary_checkout": fixture.project.join("README.md"),
        "primary_index": repo.path().join("index"),
        "shared_reference": repo.path().join(repo.head().unwrap().name().unwrap()),
        "repository_config": repo.path().join("config"),
        "sibling_checkout": sibling.join("README.md"),
        "sibling_index": other.path().join("index"),
        "sibling_head": other.path().join("HEAD"),
        "sibling_reference": repo.path().join(other.head().unwrap().name().unwrap()),
    });
    if let Some(source) = source {
        paths["preserved_source"] = json!(source.join("contribution.txt"));
        paths["preserved_index"] =
            json!(Repository::open(source).unwrap().path().join("index"));
    }
    paths
}

fn snapshot(paths: &Value) -> BTreeMap<String, Vec<u8>> {
    paths
        .as_object()
        .unwrap()
        .iter()
        .map(|(name, path)| {
            (name.clone(), fs::read(path.as_str().unwrap()).unwrap())
        })
        .collect()
}

#[test]
fn commit_policy_positive_control_can_write_and_stage_linked_worktree() {
    let fixture = TestEnvironment::new();
    let sibling = sibling(&fixture);
    let manifest = fixture.root.join("probe.json");
    fs::write(&manifest, json!({"control": true, "protected": protected(&fixture, &sibling, None)}).to_string()).unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .current_dir(&sibling)
        .env("COTERIE_COMMIT_PROBE", manifest)
        .args(["--exact", CHILD, "--ignored", "--nocapture"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .contains("COTERIE_COMMIT_OBSERVATION=")
    );
}

fn read_logs(
    fixture: &TestEnvironment,
    name: &str,
    cursor: &mut u64,
    transcript: &mut String,
) -> bool {
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
    *cursor = logs["data"]["next_cursor"].as_u64().unwrap();
    logs["data"]["terminal"] == true && logs["data"]["eof"] == true
}

fn coordinator_commit(path: &Path, reference: &str, base: &str) -> String {
    let repo = Repository::open(path).unwrap();
    let head = repo.head().unwrap();
    assert_eq!(head.name().unwrap(), reference);
    assert_eq!(head.target().unwrap().to_string(), base);
    assert_eq!(
        fs::read_to_string(path.join("contribution.txt")).unwrap(),
        CONTENT
    );
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("contribution.txt")).unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let parent = head.peel_to_commit().unwrap();
    let signature =
        Signature::now("Coterie Coordinator", "test@example.invalid").unwrap();
    repo.commit(
        Some("HEAD"),
        &signature,
        &signature,
        "test: add worker contribution",
        &tree,
        &[&parent],
    )
    .unwrap()
    .to_string()
}

#[test]
#[ignore = "requires explicit opt-in, installed Codex, local authentication, and model access; launches workers and a recovery continuation"]
fn installed_codex_linked_worktree_commit_handoff_and_recovery() {
    let runtime = PathBuf::from(
        std::env::var_os("XDG_RUNTIME_DIR")
            .expect("requires XDG_RUNTIME_DIR outside /tmp"),
    );
    assert!(!runtime.starts_with("/tmp") && !runtime.starts_with("/var/tmp"));
    let fixture = TestEnvironment::under(&runtime, true);
    let home = isolated_authentication(&fixture);
    // A conflicting network default must not expand the selected worker policy.
    fs::write(
        home.join("config.toml"),
        "[sandbox_workspace_write]\nnetwork_access = true\n",
    )
    .unwrap();
    let global = include_str!("../../examples/config/global.toml").replace(
        "roles.builder]\nprovider = \"codex\"",
        "roles.builder]\nprovider = \"real_codex\"",
    );
    write_global(
        &fixture,
        &format!(
            "{global}\n[providers.real_codex]\ncommand = [{}]\n",
            serde_json::to_string(&installed_codex()).unwrap()
        ),
    );
    let other = sibling(&fixture);
    let capture = fixture.root.join("lead-environment");
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
    wait_until("commit test lead", || capture.exists());
    let _run = StopRun(&fixture);
    let environment = captured_environment(&capture);
    let provider = Command::new(installed_codex())
        .arg("--version")
        .output()
        .unwrap();
    println!(
        "provider: {}",
        String::from_utf8_lossy(&provider.stdout).trim()
    );
    println!(
        "coterie binary: {}; version: {}",
        env!("CARGO_BIN_EXE_coterie"),
        env!("CARGO_PKG_VERSION")
    );
    println!(
        "coterie sha256: {}",
        Sha256::digest(fs::read(env!("CARGO_BIN_EXE_coterie")).unwrap())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    );

    for recover in [false, true] {
        // Keep normal and recovery contributions independent for integration.
        if recover {
            commit_file(
                &fixture.project,
                Path::new("contribution.txt"),
                "reset fixture\n",
                "test: reset contribution",
            );
        }
        let manifest = fixture.root.join("commit-probe.json");
        let probe = format!(
            "COTERIE_COMMIT_PROBE={} {} --exact {CHILD} --ignored --nocapture",
            manifest.display(),
            std::env::current_exe().unwrap().display()
        );
        let description = format!(
            "This is a bounded commit-handoff acceptance fixture. An authorized operator is available and will read your durable request, review and commit the exact contribution, and send confirmation. Call prime first and read commit_handoffs. Call inbox. Use only Coterie MCP for orchestration. Execute this exact trusted test helper with a non-login shell (a functions.exec wrapper is fine): {probe}. It edits contribution.txt, validates its exact contents, reproduces stage/commit denial, and verifies protected files cannot be written. This one diagnostic attempt is explicitly requested; do not retry Git writes or change permissions. Send coordinator a durable commit request containing COMMIT_READY, your assignment identity and base, intended path contribution.txt, message test: add worker contribution, helper command, and validation outcome. Recovery fixture is {recover}: if true AND prime shows no recovery for your task, end your turn after sending WITHOUT finish (intentional interruption). Otherwise stop editing and poll inbox until the operator confirms COMMIT_CONFIRMED with the full commit ID; allow up to 120 seconds. Verify HEAD equals that ID and git status --porcelain is empty, then call finish completed with the validation evidence. Do not perform any other work or request escalation."
        );
        let task = fixture.run_json(&[
            "task",
            "create",
            "Commit handoff acceptance",
            "--description",
            &description,
            "--json",
        ]);
        let task_id = task["data"]["task"]["id"].as_str().unwrap();
        let mut source: Option<PathBuf> = None;
        let mut source_index = Vec::new();
        for continuation in 0..=usize::from(recover) {
            let paths = protected(&fixture, &other, source.as_deref());
            let before = snapshot(&paths);
            let observation_path = fixture
                .runtime
                .join(format!("probe-{recover}-{continuation}.json"));
            assert!(!observation_path.exists());
            fs::write(
                &manifest,
                json!({"protected": paths, "source": source, "observation": observation_path}).to_string(),
            )
            .unwrap();
            let spawned = fixture
                .run_json(&["spawn", "builder", "--task", task_id, "--json"]);
            let name = spawned["data"]["agent"]["name"].as_str().unwrap();
            let assignment = spawned["data"]["assignment_id"].as_str().unwrap();
            fixture.run_json(&["send", name, "Authorized test coordinator is available for this assignment. Run the requested helper, then send COMMIT_READY with contribution.txt, assignment, base, and validation evidence. Stop editing after that request; I will commit and reply with COMMIT_CONFIRMED.", "--json"]);
            let prime = fixture.run_json(&["prime", "--json"]);
            let handoff = prime["data"]["commit_handoffs"]
                .as_array()
                .unwrap()
                .iter()
                .find(|handoff| handoff["assignment_id"] == assignment)
                .unwrap();
            assert_eq!(
                handoff["permission_profile"],
                json!({"filesystem":"workspace-write", "network":"deny", "approvals":"never"})
            );
            println!("handoff policy: {}", handoff["permission_profile"]);
            let workspace =
                PathBuf::from(handoff["workspace_path"].as_str().unwrap());
            assert!(Repository::open(&workspace).unwrap().is_worktree());
            let interrupted = recover && continuation == 0;
            let deadline = Instant::now() + Duration::from_secs(180);
            let mut transcript = String::new();
            let mut cursor = 0;
            let mut committed = None;
            let mut requested = false;
            loop {
                let terminal =
                    read_logs(&fixture, name, &mut cursor, &mut transcript);
                let inbox =
                    fixture.run_agent_json(&["inbox", "--json"], &environment);
                let request = inbox["data"]["messages"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|message| {
                        message["sender"]["id"]
                            == spawned["data"]["agent"]["id"]
                            && message["body"].as_str().is_some_and(|body| {
                                body.contains("COMMIT_READY")
                                    && body.contains("contribution.txt")
                                    && body.contains(assignment)
                            })
                    });
                if let Some(request) = request {
                    requested = true;
                    let observation: Value = serde_json::from_slice(
                        &fs::read(&observation_path).unwrap_or_else(|error| {
                            panic!(
                                "probe did not complete: {error}; {transcript}"
                            )
                        }),
                    )
                    .unwrap();
                    assert_eq!(observation["workspace"], json!(workspace));
                    assert_eq!(observation["validated"], true);
                    for name in paths
                        .as_object()
                        .unwrap()
                        .keys()
                        .map(String::as_str)
                        .chain(["stage", "commit"])
                    {
                        assert_eq!(
                            observation["writes"][name], false,
                            "{observation}"
                        );
                    }
                    assert!(
                        request["body"]
                            .as_str()
                            .unwrap()
                            .contains("contribution.txt")
                    );
                    if !interrupted && committed.is_none() {
                        assert_eq!(snapshot(&paths), before);
                        let oid = coordinator_commit(
                            &workspace,
                            handoff["owned_reference"].as_str().unwrap(),
                            handoff["base_commit"].as_str().unwrap(),
                        );
                        assert_repository_clean(&workspace);
                        fixture.run_json(&["send", name, &format!("COMMIT_CONFIRMED {oid}. Reviewed contribution.txt and reran its exact-content validation. Verify HEAD and cleanliness, then finish."), "--json"]);
                        committed = Some(oid);
                    }
                }
                if terminal {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "worker timed out: {transcript}"
                );
                thread::sleep(Duration::from_millis(250));
            }
            assert!(requested, "missing durable handoff: {transcript}");
            assert_eq!(snapshot(&paths), before);
            let events: Vec<Value> = transcript
                .lines()
                .filter_map(|line| serde_json::from_str(line).ok())
                .collect();
            let completed: Vec<_> = events
                .iter()
                .filter(|event| event["type"] == "item.completed")
                .map(|event| &event["item"])
                .collect();
            println!(
                "probe observation: {}",
                fs::read_to_string(&observation_path).unwrap()
            );
            for tool in ["prime", "send"] {
                assert!(
                    completed
                        .iter()
                        .any(|item| item["type"] == "mcp_tool_call"
                            && item["tool"] == tool
                            && item["status"] == "completed"),
                    "missing {tool}: {transcript}"
                );
            }
            if interrupted {
                assert!(committed.is_none());
                assert_eq!(
                    fs::read_to_string(workspace.join("contribution.txt"))
                        .unwrap(),
                    CONTENT
                );
                // Reproduce an interrupted staged artifact without granting its
                // continuation ownership of the preserved index.
                let repo = Repository::open(&workspace).unwrap();
                let mut index = repo.index().unwrap();
                index.add_path(Path::new("contribution.txt")).unwrap();
                index.write().unwrap();
                source_index = fs::read(repo.path().join("index")).unwrap();
                fixture.run_json(&[
                    "task",
                    "recover",
                    "--assignment",
                    assignment,
                    "--reason",
                    "Intentional exit awaiting coordinator commit",
                    "--json",
                ]);
                source = Some(workspace);
            } else {
                assert!(committed.is_some(), "missing operator commit");
                assert!(
                    completed
                        .iter()
                        .any(|item| item["type"] == "mcp_tool_call"
                            && item["tool"] == "finish"
                            && item["status"] == "completed"),
                    "missing worker submission: {transcript}"
                );
                let prime = fixture.run_json(&["prime", "--json"]);
                let task = prime["data"]["tasks"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|task| task["id"] == task_id)
                    .unwrap();
                assert_eq!(task["status"], "submitted", "{transcript}");
                assert!(
                    prime["data"]["commit_handoffs"]
                        .as_array()
                        .unwrap()
                        .is_empty()
                );
                fixture.run_json(&[
                    "workspace",
                    "integrate",
                    "--assignment",
                    assignment,
                    "--json",
                ]);
                assert_eq!(
                    fs::read_to_string(
                        fixture.project.join("contribution.txt")
                    )
                    .unwrap(),
                    CONTENT
                );
                assert_repository_clean(&fixture.project);
                fixture.run_json(&[
                    "task",
                    "close",
                    task_id,
                    "--summary",
                    "Validated exact contribution contents after integration.",
                    "--json",
                ]);
            }
        }
        if let Some(source) = source {
            assert_eq!(
                fs::read_to_string(source.join("contribution.txt")).unwrap(),
                CONTENT
            );
            assert_eq!(
                fs::read(
                    Repository::open(source).unwrap().path().join("index")
                )
                .unwrap(),
                source_index
            );
        }
    }
    lead.stdin.take().unwrap().write_all(b"done\n").unwrap();
    lead.wait().unwrap();
    fixture.run_json(&["stop", "--json"]);
}

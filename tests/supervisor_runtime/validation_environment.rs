//! Opt-in NixOS validation probes through an actual assigned Codex worker.

use super::*;
use sha2::{Digest, Sha256};

const PROBE: &str = include_str!("../fixtures/validation_probe.py");
const VALIDATE: &str = "import json, sys\nfrom pathlib import Path\nassert Path('value.txt').read_text() == 'fixture\\n'\nprint(json.dumps({'validated': True, 'python': sys.executable}))\n";

fn prepare(fixture: &TestEnvironment) {
    fs::create_dir(fixture.root.join("shared-state")).unwrap();
    for name in ["local", "shared"] {
        let directory = fixture.project.join(name);
        fs::create_dir(&directory).unwrap();
        for (file, content) in [
            (
                "devenv.nix",
                "{ pkgs, ... }: { packages = [ pkgs.python3 ]; }\n",
            ),
            ("devenv.yaml", include_str!("../../devenv.yaml")),
            ("devenv.lock", include_str!("../../devenv.lock")),
            ("validate.py", VALIDATE),
            ("value.txt", "fixture\n"),
        ] {
            commit_file(
                &fixture.project,
                &Path::new(name).join(file),
                content,
                "test: prepare validation fixture",
            );
        }
    }
    commit_file(
        &fixture.project,
        Path::new(".gitignore"),
        ".devenv\n.direnv\n",
        "test: ignore local environment state",
    );
    std::os::unix::fs::symlink(
        fixture.root.join("shared-state"),
        fixture.project.join("shared/.devenv"),
    )
    .unwrap();
    // Commit this deliberate layout so the assigned worktree reproduces an
    // environment state path resolving outside its writable file tree.
    let repo = Repository::open(&fixture.project).unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("shared/.devenv")).unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let parent = repo.head().unwrap().peel_to_commit().unwrap();
    let signature =
        Signature::now("Coterie Test", "test@example.invalid").unwrap();
    repo.commit(
        Some("HEAD"),
        &signature,
        &signature,
        "test: reproduce shared environment state",
        &tree,
        &[&parent],
    )
    .unwrap();
}

fn observation(path: &Path) -> Value {
    serde_json::from_slice(
        &fs::read(path).expect("the helper must write its own evidence"),
    )
    .unwrap()
}

fn assert_success(report: &Value, name: &str) {
    assert_eq!(
        report["observations"][name]["exit_code"], 0,
        "{name}: {report}"
    );
}

fn assert_denied(report: &Value, name: &str, diagnostic: &str) {
    let result = &report["observations"][name];
    assert!(
        result["exit_code"].as_i64().is_some_and(|code| code != 0),
        "{name}: {report}"
    );
    assert!(
        result["stderr"].as_str().unwrap().contains(diagnostic),
        "{name}: {report}"
    );
}

#[test]
#[ignore = "requires explicit opt-in, NixOS, a live Nix daemon, cached devenv inputs, installed Codex, local authentication, and model access"]
fn installed_codex_nixos_validation_environment_access() {
    assert!(
        fs::read_to_string("/etc/os-release")
            .unwrap()
            .lines()
            .any(|line| line == "ID=nixos")
    );
    let runtime = PathBuf::from(
        std::env::var_os("XDG_RUNTIME_DIR")
            .expect("requires XDG_RUNTIME_DIR outside system temp directories"),
    );
    assert!(
        runtime.is_absolute()
            && !runtime.starts_with("/tmp")
            && !runtime.starts_with("/var/tmp")
    );
    let mut fixture = TestEnvironment::under(&runtime, true);
    prepare(&fixture);
    let home = isolated_authentication(&fixture);
    fs::write(
        home.join("config.toml"),
        "[sandbox_workspace_write]\nnetwork_access = true\n",
    )
    .unwrap();
    let probe = fixture.root.join("validation-probe.py");
    fs::write(&probe, PROBE).unwrap();
    let control_path = fixture.runtime.join("validation-control.json");
    let control = Command::new("python3")
        .arg(&probe)
        .arg(&control_path)
        .current_dir(&fixture.project)
        .output()
        .unwrap();
    assert!(control.status.success(), "{control:?}");
    let control = observation(&control_path);
    println!("operator control: {control}");
    for name in [
        "inherited_validation",
        "nix_eval",
        "nix_daemon",
        "local_state",
        "shared_state",
        "devenv_local",
        "devenv_shared",
    ] {
        assert_success(&control, name);
    }
    let prepared: Value = serde_json::from_str(
        control["observations"]["devenv_local"]["stdout"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(prepared["validated"], true);
    // Enter the fixture environment as the operator, then retain its resolved
    // interpreter on PATH through Coterie's real supervisor/provider filtering.
    fixture.path = std::env::join_paths(
        std::iter::once(
            Path::new(prepared["python"].as_str().unwrap())
                .parent()
                .unwrap()
                .to_owned(),
        )
        .chain(std::env::split_paths(&fixture.path)),
    )
    .unwrap();
    let protected_state = fixture.root.join("shared-state/access-probe");
    let state_before = fs::read(&protected_state).unwrap();
    let repo = Repository::open(&fixture.project).unwrap();
    let index_before = fs::read(repo.path().join("index")).unwrap();
    let config_before = fs::read(repo.path().join("config")).unwrap();
    let head_before = repo.head().unwrap().target().unwrap();
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
    wait_until("validation test lead", || capture.exists());
    let _run = StopRun(&fixture);
    let environment = captured_environment(&capture);
    for (program, args) in [
        (installed_codex(), vec!["--version"]),
        (PathBuf::from("nix"), vec!["--version"]),
        (PathBuf::from("devenv"), vec!["--version"]),
        (PathBuf::from("uname"), vec!["-sr"]),
    ] {
        let output = Command::new(&program).args(args).output().unwrap();
        assert!(output.status.success());
        println!(
            "{program:?}: {}",
            String::from_utf8_lossy(&output.stdout).trim()
        );
    }
    println!(
        "coterie {} sha256: {}",
        env!("CARGO_PKG_VERSION"),
        Sha256::digest(fs::read(env!("CARGO_BIN_EXE_coterie")).unwrap())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    );
    let output_path = fixture.runtime.join("validation-worker.json");
    let argv = json!(["python3", probe, output_path]);
    let description = format!(
        "This is an explicit NixOS validation-access regression fixture. Call prime and inbox through Coterie MCP. An authorized coordinator is available. Execute this trusted helper once from your assigned worktree, with a non-login shell and these exact argument-array entries: {argv}. It records independent validation, Nix daemon, and local/shared .devenv state probes. Permission denials are expected observations. Do not change permissions, retry blocked commands, edit tracked files, or stage/commit anything. Send coordinator a durable message containing VALIDATION_OBSERVED, your assignment ID, selected policy, and each probe's passed or blocked outcome and diagnostic. Then finish completed, summarizing the successful fixture validation and the blocked environment commands. A clean unchanged worktree can submit its base commit. Do not claim the blocked checks passed."
    );
    let task = fixture.run_json(&[
        "task",
        "create",
        "Diagnose validation access",
        "--description",
        &description,
        "--json",
    ]);
    let task_id = task["data"]["task"]["id"].as_str().unwrap();
    let spawned =
        fixture.run_json(&["spawn", "builder", "--task", task_id, "--json"]);
    let name = spawned["data"]["agent"]["name"].as_str().unwrap();
    let assignment = spawned["data"]["assignment_id"].as_str().unwrap();
    let prime = fixture.run_json(&["prime", "--json"]);
    let handoff = prime["data"]["commit_handoffs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["assignment_id"] == assignment)
        .unwrap();
    assert_eq!(
        handoff["permission_profile"],
        json!({"filesystem":"workspace-write", "network":"deny", "approvals":"never"})
    );
    println!(
        "assignment: {assignment}; selected policy: {}",
        handoff["permission_profile"]
    );
    let workspace = PathBuf::from(handoff["workspace_path"].as_str().unwrap());
    assert!(Repository::open(&workspace).unwrap().is_worktree());
    let deadline = Instant::now() + Duration::from_secs(240);
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
        if logs["data"]["terminal"] == true && logs["data"]["eof"] == true {
            break;
        }
        assert!(Instant::now() < deadline, "worker timed out: {transcript}");
        thread::sleep(Duration::from_millis(250));
    }
    assert!(
        output_path.is_file(),
        "worker did not run the helper: {transcript}"
    );
    let report = observation(&output_path);
    println!("worker observations: {report}");
    assert_eq!(report["workspace"], json!(workspace));
    for name in ["inherited_validation", "nix_eval", "local_state"] {
        assert_success(&report, name);
    }
    let validated: Value = serde_json::from_str(
        report["observations"]["inherited_validation"]["stdout"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(validated, prepared);
    assert_denied(&report, "nix_daemon", "Operation not permitted");
    assert_denied(&report, "shared_state", "Read-only file system");
    assert_denied(&report, "devenv_shared", ".devenv");
    assert_denied(&report, "devenv_local", "Operation not permitted");
    assert_eq!(fs::read(protected_state).unwrap(), state_before);
    assert_eq!(fs::read(repo.path().join("index")).unwrap(), index_before);
    assert_eq!(fs::read(repo.path().join("config")).unwrap(), config_before);
    assert_eq!(repo.head().unwrap().target().unwrap(), head_before);
    assert_repository_clean(&workspace);
    let inbox = fixture.run_agent_json(&["inbox", "--json"], &environment);
    assert!(
        inbox["data"]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|message| message["sender"]["id"]
                == spawned["data"]["agent"]["id"]
                && message["body"].as_str().is_some_and(|body| body
                    .contains("VALIDATION_OBSERVED")
                    && body.contains(assignment)
                    && body.contains("nix_daemon")
                    && body.contains("devenv_local"))),
        "missing durable diagnosis: {inbox}; {transcript}"
    );
    let prime = fixture.run_json(&["prime", "--json"]);
    assert_eq!(
        prime["data"]["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|task| task["id"] == task_id)
            .unwrap()["status"],
        "submitted",
        "{transcript}"
    );
    lead.stdin.take().unwrap().write_all(b"done\n").unwrap();
    lead.wait().unwrap();
    fixture.run_json(&["stop", "--json"]);
}

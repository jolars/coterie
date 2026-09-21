use super::*;

const RECOVERY_ID: &str = "co-01ARZ3NDEKTSV4RRFFQ69G5FB8";

#[test]
fn stopped_run_discovery_and_reactivation_preserve_tasks_and_retry_history() {
    let fixture = TestEnvironment::new();
    let empty = fixture.run_json(&["run", "list", "--json"]);
    assert_eq!(empty["data"]["runs"], serde_json::json!([]));
    assert!(!fixture.state.join("coterie").exists());
    fixture.launch(&[]);
    let status = fixture.run_json(&["status", "--json"]);
    let run_id = status["data"]["run_id"].as_str().unwrap();
    let task = fixture.run_json(&["task", "create", "Retained task", "--json"]);
    let stop_args = [
        "stop",
        "--operation-id",
        "co-01ARZ3NDEKTSV4RRFFQ69G5FB7",
        "--json",
    ];
    let stopped = fixture.run_json(&stop_args);
    let retained = fixture.run_json(&["run", "list", "--json"]);
    assert_eq!(retained["data"]["runs"][0]["run_id"], run_id);
    assert_eq!(retained["data"]["runs"][0]["status"], "stopped");
    assert_eq!(retained["data"]["runs"][0]["tasks"]["open"], 1);
    let args = [
        "run",
        "recover",
        run_id,
        "--reason",
        "Continue retained work.",
        "--operation-id",
        RECOVERY_ID,
        "--json",
    ];
    let recovered = fixture.run_json(&args);
    assert_eq!(recovered["data"]["run_id"], run_id);
    assert_eq!(recovered["operation_id"], RECOVERY_ID);
    assert_eq!(fixture.run_json(&args), recovered);
    assert_eq!(fixture.run_json(&stop_args), stopped);
    assert_eq!(
        fixture.run_json(&["task", "ready", "--json"])["data"]["tasks"][0]["id"],
        task["data"]["task"]["id"]
    );
    rejected(
        &fixture,
        &[
            "run",
            "recover",
            run_id,
            "--reason",
            "Different.",
            "--operation-id",
            RECOVERY_ID,
            "--json",
        ],
        "conflict",
    );
    rejected(
        &fixture,
        &[
            "run",
            "recover",
            run_id,
            "--reason",
            "Already active.",
            "--json",
        ],
        "active",
    );
    fixture.launch(&[]);
    assert_eq!(
        fixture.run_json(&["status", "--json"])["data"]["run_id"],
        run_id
    );
    fixture.run_json(&["stop", "--json"]);
    assert_eq!(fixture.run_json(&args), recovered);
    assert_eq!(fixture.index_entry_count(), 0);
}

#[test]
fn run_recovery_refuses_policy_drift_and_a_replacement_run() {
    let fixture = TestEnvironment::new();
    fixture.launch(&[]);
    let first = fixture.run_json(&["status", "--json"]);
    let run_id = first["data"]["run_id"].as_str().unwrap();
    fixture.run_json(&["stop", "--json"]);
    let args = [
        "run",
        "recover",
        run_id,
        "--reason",
        "Retained work.",
        "--operation-id",
        RECOVERY_ID,
        "--json",
    ];
    write_global(&fixture, "[limits]\nmax_agents_per_run = 10\n");
    let output = run({
        let mut command = fixture.command();
        command.args(args);
        command
    });
    assert_eq!(output.status.code(), Some(3));
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("configuration conflicts")
    );
    assert_eq!(fixture.index_entry_count(), 0);
    fs::remove_file(fixture.root.join("config/coterie/config.toml")).unwrap();
    fixture.launch(&[]);
    let index = fixture.only_index_entry();
    let before = fs::read(&index).unwrap();
    let replacement = fixture.run_json(&["status", "--json"]);
    assert_ne!(replacement["data"]["run_id"], run_id);
    rejected(&fixture, &args, "replacement run");
    assert_eq!(fs::read(&index).unwrap(), before);
    assert_eq!(fixture.run_json(&["status", "--json"]), replacement);
    fixture.run_json(&["stop", "--json"]);
    fixture.run_json(&args);
    assert_eq!(
        fixture.run_json(&["status", "--json"])["data"]["run_id"],
        run_id
    );
    fixture.run_json(&["stop", "--json"]);
}

#[test]
fn run_recovery_acquires_all_project_leases_before_publishing_any_index() {
    let fixture = TestEnvironment::new();
    fixture.launch(&[]);
    let status = fixture.run_json(&["status", "--json"]);
    let run_id = status["data"]["run_id"].as_str().unwrap();
    let library = fixture.root.join("library");
    fs::create_dir(&library).unwrap();
    fixture.run_json(&[
        "project",
        "attach",
        library.to_str().unwrap(),
        "--json",
    ]);
    fixture.run_json(&["stop", "--json"]);
    let mut secondary = fixture.command();
    secondary.current_dir(&library);
    assert!(run(secondary).status.success());
    let secondary_index = fixture.only_index_entry();
    let original_index = fs::read(&secondary_index).unwrap();
    let args = [
        "run",
        "recover",
        run_id,
        "--reason",
        "Two-project continuation.",
        "--operation-id",
        RECOVERY_ID,
        "--json",
    ];
    rejected(&fixture, &args, "replacement run");
    assert_eq!(fixture.index_entry_count(), 1);
    assert_eq!(fs::read(&secondary_index).unwrap(), original_index);
    let retained = fixture.run_json(&["run", "list", "--json"]);
    assert_eq!(retained["data"]["runs"][0]["status"], "stopped");
    let mut stop = fixture.command();
    stop.current_dir(&library).arg("stop");
    assert!(run(stop).status.success());
    fs::write(
        library.join("coterie.toml"),
        "[roles.worker]\nmax_instances = 1\n",
    )
    .unwrap();
    rejected(&fixture, &args, "configuration differs");
    assert_eq!(fixture.index_entry_count(), 0);
    fs::remove_file(library.join("coterie.toml")).unwrap();
    let mut recover = fixture.command();
    recover.current_dir(&library).args(args);
    assert!(run(recover).status.success());
    assert_eq!(fixture.index_entry_count(), 2);
    let mut secondary_status = fixture.command();
    secondary_status
        .current_dir(&library)
        .args(["status", "--json"]);
    assert_eq!(
        serde_json::from_slice::<Value>(&run(secondary_status).stdout).unwrap()
            ["data"]["run_id"],
        run_id
    );
    fixture.run_json(&["stop", "--json"]);
}

#[test]
fn stopped_run_commands_require_operator_authority_even_with_partial_credentials()
 {
    let fixture = TestEnvironment::new();
    for args in [
        vec!["run", "list", "--json"],
        vec![
            "run",
            "recover",
            RUN_ID,
            "--reason",
            "Must not elevate.",
            "--json",
        ],
    ] {
        let mut command = fixture.command();
        command.env("COTERIE_TOKEN", "partial").args(args);
        let output = run(command);
        assert_eq!(output.status.code(), Some(6));
        assert!(output.stdout.is_empty());
        assert!(
            String::from_utf8(output.stderr)
                .unwrap()
                .contains("operator channel")
        );
    }
    assert!(!fixture.state.join("coterie").exists());
}

#[test]
fn run_recovery_starts_fresh_foreground_sessions_and_guides_stale_mcp_clients()
{
    let fixture = TestEnvironment::new();
    let capture = fixture.root.join("foreground-capture");
    let launch = || {
        let mut command = fixture.command();
        command
            .env("COTERIE_FAKE_MODE", "contract")
            .env("COTERIE_FAKE_CAPTURE", &capture);
        assert!(run(command).status.success());
        captured_environment(&capture)
    };
    let original = launch();
    let run_id = environment_value(&original, "COTERIE_RUN_ID");
    fixture.run_json(&["stop", "--json"]);
    for args in [vec!["prime", "--json"], vec!["__mcp"]] {
        let mut command = fixture.agent_command(&original);
        command.args(args);
        let output = run(command);
        assert_eq!(output.status.code(), Some(7));
        assert!(output.stdout.is_empty());
        let diagnostic = String::from_utf8(output.stderr).unwrap();
        assert!(diagnostic.contains("active run"));
        assert!(diagnostic.contains("stopped run"));
        assert!(diagnostic.contains("coterie run recover"));
    }
    fixture.run_json(&[
        "run",
        "recover",
        &run_id,
        "--reason",
        "Fresh session.",
        "--json",
    ]);
    let current = launch();
    assert_eq!(environment_value(&current, "COTERIE_RUN_ID"), run_id);
    assert_ne!(
        environment_value(&current, "COTERIE_SESSION_ID"),
        environment_value(&original, "COTERIE_SESSION_ID")
    );
    assert_ne!(
        environment_value(&current, "COTERIE_TOKEN"),
        environment_value(&original, "COTERIE_TOKEN")
    );
    let output = run({
        let mut command = fixture.agent_command(&original);
        command.arg("__mcp");
        command
    });
    assert_eq!(output.status.code(), Some(6));
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("fresh session")
    );
    fixture.run_json(&["stop", "--json"]);
}

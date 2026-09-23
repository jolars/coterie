use super::*;

#[test]
fn configured_reviewers_and_unrestricted_access_reach_launches_and_recovery() {
    for (filesystem, sandbox) in [
        ("project-write", "workspace-write"),
        ("unrestricted", "danger-full-access"),
    ] {
        let fixture = TestEnvironment::new();
        let provider = fixture.root.join("bin/codex");
        let script = FAKE_CODEX.replace("#!/bin/sh", "#!/bin/sh\nif [ -n \"${COTERIE_RUN_ID-}\" ]; then printf '%s\\n' \"$@\" > \"$0.$COTERIE_ROLE.capture\"; fi");
        fs::write(&provider, script).unwrap();
        let global = include_str!("../../examples/config/global.toml")
            .replace(
                "filesystem = \"project-write\"",
                &format!("filesystem = '{filesystem}'"),
            )
            .replace(
                "approvals = \"interactive\"",
                "approvals = 'interactive'\napproval_reviewer = 'auto-review'",
            );
        write_global(&fixture, &global);
        let home = fixture.root.join("codex-home");
        fs::create_dir(&home).unwrap();
        fs::write(home.join("config.toml"), "default_permissions = ':danger-full-access'\napproval_policy = 'never'\napprovals_reviewer = 'user'").unwrap();
        let mut command = fixture.command();
        command.env("CODEX_HOME", &home);
        assert!(run(command).status.success());
        let capture =
            fs::read_to_string(provider.with_extension("coordinator.capture"))
                .unwrap();
        assert!(
            capture.contains(&format!("--sandbox\n{sandbox}\n")),
            "{capture}"
        );
        assert!(capture.contains("approval_policy=\"on-request\""));
        assert!(capture.contains("approvals_reviewer=\"auto_review\""));
        let task =
            fixture.run_json(&["task", "create", "Worker policy", "--json"]);
        fixture.run_json(&[
            "spawn",
            "builder",
            "--task",
            task["data"]["task"]["id"].as_str().unwrap(),
            "--json",
        ]);
        let worker = provider.with_extension("builder.capture");
        wait_until("worker capture", || worker.exists());
        let worker = fs::read_to_string(worker).unwrap();
        assert!(worker.contains("--sandbox\nworkspace-write\n"));
        assert!(worker.contains("approval_policy=\"never\""));
        assert!(
            worker.contains("sandbox_workspace_write.network_access=false")
        );
        assert!(!worker.contains("approvals_reviewer="));
        let status = fixture.run_json(&["status", "--json"]);
        let run_id = status["data"]["run_id"].as_str().unwrap();
        fixture.run_json(&["stop", "--json"]);
        fixture.run_json(&[
            "run",
            "recover",
            run_id,
            "--reason",
            "Verify saved permissions.",
            "--json",
        ]);
        fixture.launch(&[]);
        let capture =
            fs::read_to_string(provider.with_extension("coordinator.capture"))
                .unwrap();
        assert!(capture.contains("approvals_reviewer=\"auto_review\""));
        assert!(capture.contains(&format!("--sandbox\n{sandbox}\n")));
        write_global(&fixture, &global.replace("auto-review", "user"));
        let rejected = run(fixture.command());
        assert_eq!(rejected.status.code(), Some(3));
        assert!(
            String::from_utf8_lossy(&rejected.stderr)
                .contains("saved snapshot")
        );
        fixture.run_json(&["stop", "--json"]);
    }
}

#[test]
fn project_cannot_launch_unrestricted_archetype_without_operator_selection() {
    let fixture = TestEnvironment::new();
    let global = include_str!("../../examples/config/global.toml")
        .replace("archetype = \"global:pair@1\"", "")
        .replace(
            "filesystem = \"project-write\"",
            "filesystem = 'unrestricted'",
        );
    write_global(&fixture, &global);
    fs::write(
        fixture.project.join("coterie.toml"),
        "archetype = 'global:pair@1'",
    )
    .unwrap();
    let output = run(fixture.command());
    assert_eq!(output.status.code(), Some(3));
    assert!(!fixture.state.exists());
    fixture.launch(&["--archetype", "global:pair@1"]);
    fixture.run_json(&["stop", "--json"]);
}

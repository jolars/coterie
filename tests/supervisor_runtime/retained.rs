use super::*;

#[test]
fn retained_commit_handoff_survives_exit_and_releases_dependencies_after_acceptance()
 {
    for acknowledged in [false, true] {
        let fixture = TestEnvironment::new();
        fs::write(
            fixture.root.join("bin/codex"),
            format!(
                "{}{}",
                FAKE_CODEX.split_once("is_job=false").unwrap().0,
                recovery::JOB
            ),
        )
        .unwrap();
        fixture.launch(&[]);
        let task = fixture.run_json(&[
            "task",
            "create",
            "Retain reviewed commit",
            "--json",
        ]);
        let task_id = task["data"]["task"]["id"].as_str().unwrap();
        let dependent = fixture.run_json(&[
            "task",
            "create",
            "Wait for acceptance",
            "--after",
            task_id,
            "--json",
        ]);
        let spawn =
            fixture.run_json(&["spawn", "worker", "--task", task_id, "--json"]);
        let assignment = spawn["data"]["assignment_id"].as_str().unwrap();
        let (capture, environment, workspace) =
            recovery::captured_job(&fixture, &spawn);
        let result = commit_file(
            &workspace,
            Path::new("result.txt"),
            "reviewed result\n",
            "Reviewed result",
        );
        let message = fixture.run_json(&[
            "send",
            spawn["data"]["agent"]["id"].as_str().unwrap(),
            &format!("Reviewed and committed {result}; validation passed."),
            "--json",
        ]);
        if acknowledged {
            let inbox =
                fixture.run_agent_json(&["inbox", "--json"], &environment);
            let through =
                inbox["data"]["next_cursor"].as_u64().unwrap().to_string();
            fixture.run_agent_json(
                &["inbox", "ack", &through, "--json"],
                &environment,
            );
        }
        let repository = Repository::open(&workspace).unwrap();
        let index = fs::read(repository.path().join("index")).unwrap();
        fs::write(format!("{}.exit", capture.display()), "exit").unwrap();
        wait_until("retained worker exit", || {
            fixture.run_json(&["status", "--json"])["data"]["agents"]
                .as_array()
                .unwrap()
                .iter()
                .any(|agent| {
                    agent["id"] == spawn["data"]["agent"]["id"]
                        && agent["state"] == "exited"
                })
        });
        let source = message["data"]["message_id"].as_str().unwrap();
        let args = [
            "task",
            "submit-retained",
            "--assignment",
            assignment,
            "--result",
            &result,
            "--summary",
            "Verified result.txt equals reviewed result; target validation pending.",
            "--reason",
            "Worker exited before finish.",
            "--review",
            "Independently reviewed exact commit and intended path.",
            "--review-source",
            source,
            "--operation-id",
            "co-01ARZ3NDEKTSV4RRFFQ69G5FC2",
            "--json",
        ];
        let submitted = fixture.run_json(&args);
        assert_eq!(submitted["data"]["task"]["status"], "submitted");
        assert_eq!(
            submitted["data"]["task"]["result"]["result_commit"],
            result
        );
        assert_eq!(fs::read(repository.path().join("index")).unwrap(), index);
        assert_eq!(
            fixture.run_json(&["task", "ready", "--json"])["data"]["tasks"],
            serde_json::json!([])
        );
        let mut close = fixture.command();
        close.args([
            "task",
            "close",
            task_id,
            "--summary",
            "Too early",
            "--json",
        ]);
        assert_eq!(run(close).status.code(), Some(5));
        fs::write(fixture.project.join("dirty.txt"), "preserve\n").unwrap();
        let mut integrate = fixture.command();
        integrate.args([
            "workspace",
            "integrate",
            "--assignment",
            assignment,
            "--json",
        ]);
        assert_eq!(run(integrate).status.code(), Some(5));
        fs::remove_file(fixture.project.join("dirty.txt")).unwrap();
        fixture.run_json(&[
            "workspace",
            "integrate",
            "--assignment",
            assignment,
            "--json",
        ]);
        assert_eq!(
            fs::read_to_string(fixture.project.join("result.txt")).unwrap(),
            "reviewed result\n"
        );
        assert_eq!(
            fixture.run_json(&["task", "ready", "--json"])["data"]["tasks"],
            serde_json::json!([])
        );
        fixture.run_json(&[
            "task",
            "close",
            task_id,
            "--summary",
            "Validated exact integrated result.txt contents.",
            "--json",
        ]);
        assert_eq!(
            fixture.run_json(&["task", "ready", "--json"])["data"]["tasks"][0]
                ["id"],
            dependent["data"]["task"]["id"]
        );
        assert_eq!(fixture.run_json(&args), submitted);
        assert_eq!(fs::read(repository.path().join("index")).unwrap(), index);
        fixture.run_json(&["stop", "--json"]);
    }
}

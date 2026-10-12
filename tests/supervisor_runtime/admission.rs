use super::*;

#[test]
fn concurrent_admissions_recheck_the_same_saved_ceiling() {
    let fixture = TestEnvironment::new();
    fs::write(
        fixture.project.join("coterie.toml"),
        "[limits]\nmax_concurrent_agents = 1",
    )
    .unwrap();
    fixture.launch(&[]);
    let first =
        fixture.run_json(&["task", "create", "First contender", "--json"]);
    let second =
        fixture.run_json(&["task", "create", "Second contender", "--json"]);
    let first_id = first["data"]["task"]["id"].as_str().unwrap();
    let second_id = second["data"]["task"]["id"].as_str().unwrap();
    let mut commands = Vec::new();
    for id in [first_id, second_id] {
        commands.push(
            fixture
                .command()
                .args(["spawn", "worker", "--task", id, "--json"])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        );
    }
    let outputs: Vec<_> = commands
        .into_iter()
        .map(|child| child.wait_with_output().unwrap())
        .collect();
    assert_eq!(
        outputs
            .iter()
            .filter(|output| output.status.success())
            .count(),
        1,
        "{outputs:?}"
    );
    let rejected = outputs
        .iter()
        .find(|output| !output.status.success())
        .unwrap();
    assert_eq!(rejected.status.code(), Some(5));
    assert!(rejected.stdout.is_empty());
    let error: Value = serde_json::from_slice(&rejected.stderr).unwrap();
    let message = error["error"]["message"].as_str().unwrap();
    for part in [
        "max_concurrent_agents",
        "used 1",
        "ceiling 1",
        "project:limits.max_concurrent_agents",
    ] {
        assert!(message.contains(part), "{message}");
    }
    let before = fixture.run_json(&["prime", "--json"]);
    assert_eq!(before["data"]["admission"]["concurrent_agents"]["used"], 1);
    assert_eq!(before["data"]["admission"]["agents_per_run"]["used"], 2);
    assert_eq!(before["data"]["admission"]["spawns_per_minute"]["used"], 1);
    let tasks = before["data"]["tasks"].as_array().unwrap();
    assert_eq!(
        tasks
            .iter()
            .filter(|task| task["status"] == "in_progress")
            .count(),
        1
    );
    assert_eq!(
        tasks.iter().filter(|task| task["status"] == "open").count(),
        1
    );
    // A changed project file cannot increase an active run's saved ceiling.
    fs::write(
        fixture.project.join("coterie.toml"),
        "[limits]\nmax_concurrent_agents = 2",
    )
    .unwrap();
    let after = fixture.run_json(&["prime", "--json"]);
    assert_eq!(after["data"]["admission"]["concurrent_agents"]["limit"], 1);
    fixture.run_json(&["stop", "--json"]);
}

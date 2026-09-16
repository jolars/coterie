use super::finish::FinishFixture;
use super::*;

const INTEGRATE: &str = "co-01ARZ3NDEKTSV4RRFFQ69G5FC8";
const CLOSE: &str = "co-01ARZ3NDEKTSV4RRFFQ69G5FC9";
const VALIDATION: &str = "Reviewed the submitted base; verified the current target and review scope.";

fn head(path: &Path) -> String {
    Repository::open(path)
        .unwrap()
        .head()
        .unwrap()
        .target()
        .unwrap()
        .to_string()
}

fn index_bytes(path: &Path) -> Vec<u8> {
    fs::read(Repository::open(path).unwrap().path().join("index")).unwrap()
}

fn assert_conflict(command: Command, diagnostic: &str) {
    let output = run(command);
    assert_eq!(output.status.code(), Some(5), "{output:?}");
    assert!(output.stdout.is_empty());
    let error: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["error"]["code"], "conflict");
    assert!(
        error["error"]["message"]
            .as_str()
            .unwrap()
            .contains(diagnostic),
        "{error}"
    );
}

#[test]
fn unchanged_worktree_review_requires_integration_and_validated_closure() {
    for advanced in [false, true] {
        for strategy in [None, Some("merge")] {
            // Workspace policy determines acceptance, independent of the task title.
            let review = FinishFixture::new("worker", "Review without edits");
            let fixture = &review.environment;
            let submitted = review.finish("completed");
            let task = submitted["data"]["task"]["id"].as_str().unwrap();
            let assignment =
                submitted["data"]["assignment_id"].as_str().unwrap();
            assert_eq!(submitted["data"]["task"]["status"], "submitted");
            let result = &submitted["data"]["task"]["result"];
            assert_eq!(result["base_commit"], review.base);
            assert_eq!(result["result_commit"], review.base);
            assert_repository_clean(&review.workspace);

            let dependent = fixture.run_json(&[
                "task",
                "create",
                "Use the accepted review",
                "--after",
                task,
                "--json",
            ]);
            assert_eq!(dependent["data"]["task"]["ready"], false);
            let close = [
                "task",
                "close",
                task,
                "--summary",
                VALIDATION,
                "--operation-id",
                CLOSE,
                "--json",
            ];
            let mut premature = fixture.command();
            premature.args([
                "task",
                "close",
                task,
                "--summary",
                VALIDATION,
                "--json",
            ]);
            assert_conflict(premature, "not been integrated");

            let target = if advanced {
                commit_file(
                    &fixture.project,
                    Path::new("README.md"),
                    "Newer implementation committed during review.\n",
                    "advance target during review",
                )
            } else {
                review.base.clone()
            };
            let contents = fs::read(fixture.project.join("README.md")).unwrap();
            let target_index = index_bytes(&fixture.project);
            let review_index = index_bytes(&review.workspace);
            let mut integrate = vec![
                "workspace",
                "integrate",
                "--assignment",
                assignment,
                "--operation-id",
                INTEGRATE,
                "--json",
            ];
            if let Some(strategy) = strategy {
                integrate.extend(["--strategy", strategy]);
            }
            let integrated = fixture.run_json(&integrate);
            let record = &integrated["data"]["integration"];
            assert_eq!(record["base_commit"], review.base);
            assert_eq!(record["result_commit"], review.base);
            assert_eq!(record["target_commit_before"], target);
            assert_eq!(record["target_commit"], target);
            assert_eq!(record["strategy"], strategy.unwrap_or("rebase"));
            assert_eq!(fixture.run_json(&integrate), integrated);
            assert_eq!(head(&fixture.project), target);
            assert_eq!(head(&review.workspace), review.base);
            assert_eq!(index_bytes(&fixture.project), target_index);
            assert_eq!(index_bytes(&review.workspace), review_index);
            assert_eq!(
                fs::read(fixture.project.join("README.md")).unwrap(),
                contents
            );
            assert_eq!(
                fs::read_to_string(review.workspace.join("README.md")).unwrap(),
                "fixture\n"
            );
            assert_repository_clean(&fixture.project);
            assert_repository_clean(&review.workspace);

            let detail = context::read_detail(fixture, "task", task);
            assert_eq!(detail["task"]["status"], "submitted");
            assert!(
                fixture.run_json(&["task", "ready", "--json"])["data"]["tasks"]
                    .as_array()
                    .unwrap()
                    .is_empty()
            );
            let mut empty_validation = fixture.command();
            empty_validation.args([
                "task",
                "close",
                task,
                "--summary",
                " ",
                "--json",
            ]);
            assert_eq!(run(empty_validation).status.code(), Some(2));

            let closed = fixture.run_json(&close);
            assert_eq!(closed["data"]["task"]["status"], "closed");
            let result = &closed["data"]["task"]["result"];
            assert_eq!(
                result["assignment_result"],
                submitted["data"]["task"]["result"]
            );
            for field in [
                "assignment_id",
                "base_commit",
                "result_commit",
                "target_commit",
            ] {
                assert_eq!(result["integration"][field], record[field]);
            }
            assert_eq!(result["validation_summary"], VALIDATION);
            assert!(result.get("operator_override").is_none());
            assert_eq!(fixture.run_json(&close), closed);
            let ready = fixture.run_json(&["task", "ready", "--json"]);
            assert_eq!(ready["data"]["tasks"].as_array().unwrap().len(), 1);
            assert_eq!(
                ready["data"]["tasks"][0]["id"],
                dependent["data"]["task"]["id"]
            );
            assert_eq!(head(&fixture.project), target);
            assert_eq!(
                fs::read(fixture.project.join("README.md")).unwrap(),
                contents
            );

            let events = fixture.run_json(&["events", "--json"]);
            let events = events["data"]["events"].as_array().unwrap();
            for (operation, event_type) in [
                (INTEGRATE, "workspace.integrated"),
                (CLOSE, "task.lifecycle_changed"),
            ] {
                assert_eq!(
                    events
                        .iter()
                        .filter(|event| event["operation_id"] == operation
                            && event["event_type"] == event_type)
                        .count(),
                    1
                );
            }
            fixture.run_json(&["stop", "--json"]);
        }
    }
}

#[test]
fn unchanged_review_integration_refuses_dirty_target_and_worktree() {
    let review = FinishFixture::new("worker", "Review with concurrent edits");
    let fixture = &review.environment;
    let submitted = review.finish("completed");
    let assignment = submitted["data"]["assignment_id"].as_str().unwrap();
    let integrate = [
        "workspace",
        "integrate",
        "--assignment",
        assignment,
        "--operation-id",
        INTEGRATE,
        "--json",
    ];
    for (path, diagnostic) in [
        (&fixture.project, "target"),
        (&review.workspace, "workspace"),
    ] {
        fs::write(path.join("README.md"), "Preserve pending tracked work.\n")
            .unwrap();
        fs::write(path.join("pending.txt"), "Preserve untracked work.\n")
            .unwrap();
        let index = index_bytes(path);
        let mut command = fixture.command();
        command.args([
            "workspace",
            "integrate",
            "--assignment",
            assignment,
            "--json",
        ]);
        assert_conflict(command, diagnostic);
        assert_eq!(head(&fixture.project), review.base);
        assert_eq!(head(&review.workspace), review.base);
        assert_eq!(index_bytes(path), index);
        assert_eq!(
            fs::read_to_string(path.join("README.md")).unwrap(),
            "Preserve pending tracked work.\n"
        );
        assert_eq!(
            fs::read_to_string(path.join("pending.txt")).unwrap(),
            "Preserve untracked work.\n"
        );
        assert!(review.database().query_row("SELECT target_commit IS NULL FROM workspaces WHERE assignment_id = ?1", [assignment], |row| row.get::<_, bool>(0)).unwrap());
        fs::write(path.join("README.md"), "fixture\n").unwrap();
        fs::remove_file(path.join("pending.txt")).unwrap();
    }
    let integrated = fixture.run_json(&integrate);
    assert_eq!(
        integrated["data"]["integration"]["target_commit"],
        review.base
    );
    fixture.run_json(&["stop", "--json"]);
}

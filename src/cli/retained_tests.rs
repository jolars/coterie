use super::*;
use crate::protocol::{RetainedSubmissionSummary, TaskSummary};
use crate::tasks::TaskStatus;

fn example() -> RetainedSubmissionSummary {
    RetainedSubmissionSummary {
        assignment_id: "ca-01ARZ3NDEKTSV4RRFFQ69G5FAV".parse().unwrap(),
        task: TaskSummary {
            id: "ct-01ARZ3NDEKTSV4RRFFQ69G5FAV".parse().unwrap(),
            project_id: "cp-01ARZ3NDEKTSV4RRFFQ69G5FAV".parse().unwrap(),
            project: "primary".into(),
            title: "Submit retained work".into(),
            description: "Preserve the reviewed commit after exit.".into(),
            status: TaskStatus::Submitted,
            ready: false,
            unresolved_dependencies: vec![],
            result: Some(
                serde_json::json!({"status": "completed", "summary": "Verified committed file contents; target validation pending.",
                "base_commit": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "result_commit": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                "retained_submission": {
                    "operation_id": "co-01ARZ3NDEKTSV4RRFFQ69G5FAV",
                    "assignment_id": "ca-01ARZ3NDEKTSV4RRFFQ69G5FAV",
                    "session_id": "cs-01ARZ3NDEKTSV4RRFFQ69G5FAV", "generation": 1,
                    "submitted_by": null, "reason": "Worker exited before finish.",
                    "review": {"text": "Independently reviewed the exact commit.", "source": "Operator inspection"}
                }}),
            ),
        },
    }
}

fn schema() -> Schema {
    generated_schema_for::<
        MutationSuccessEnvelope<'static, RetainedSubmissionSummary>,
    >()
}

#[test]
fn retained_output_schema_and_example_match_the_typed_contract() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let expected: Value = serde_json::from_str(
        &std::fs::read_to_string(
            root.join("schemas/cli-retained-v1.schema.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(serde_json::to_value(schema()).unwrap(), expected);
    let expected: Value = serde_json::from_str(
        &std::fs::read_to_string(root.join("examples/retained.json")).unwrap(),
    )
    .unwrap();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let operation_id = "co-01ARZ3NDEKTSV4RRFFQ69G5FAV".parse().unwrap();
    render_json_mutation_success(
        &mut stdout,
        &mut stderr,
        operation_id,
        &example(),
    )
    .unwrap();
    assert!(stderr.is_empty());
    assert_eq!(serde_json::from_slice::<Value>(&stdout).unwrap(), expected);
    let mut human = Vec::new();
    render_human_success(&mut human, &mut stderr, &example()).unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&human).unwrap(),
        expected["data"]
    );
    let response = crate::protocol::RpcResponse::TaskRetainedSubmitted {
        operation_id,
        submission: example(),
    };
    let mut wire = serde_json::to_value(response).unwrap();
    wire.as_object_mut().unwrap().remove("result");
    wire.as_object_mut().unwrap().remove("operation_id");
    assert_eq!(wire, expected["data"]);
}

#[test]
#[ignore = "regenerates the reviewed retained submission schema and example"]
fn regenerate_retained_contract() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    std::fs::write(
        root.join("schemas/cli-retained-v1.schema.json"),
        format!("{}\n", serde_json::to_string_pretty(&schema()).unwrap()),
    )
    .unwrap();
    let output = MutationSuccessEnvelope {
        schema_version: SchemaVersion,
        operation_id: "co-01ARZ3NDEKTSV4RRFFQ69G5FAV".parse().unwrap(),
        data: &example(),
    };
    std::fs::write(
        root.join("examples/retained.json"),
        format!("{}\n", serde_json::to_string_pretty(&output).unwrap()),
    )
    .unwrap();
}

#[test]
fn retained_cli_requires_exact_commit_and_review_inputs() {
    let assignment = "ca-01ARZ3NDEKTSV4RRFFQ69G5FAV";
    let args = [
        "coterie",
        "task",
        "submit-retained",
        "--assignment",
        assignment,
        "--result",
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "--summary",
        "Validation report",
        "--reason",
        "Worker exited",
        "--review",
        "Reviewed exact commit",
        "--review-source",
        "Review artifact",
    ];
    assert!(Arguments::try_parse_from(args).is_ok());
    assert!(Arguments::try_parse_from(&args[..args.len() - 2]).is_err());
}

use super::*;
use crate::protocol::{ResubmissionSummary, TaskSummary};
use crate::tasks::TaskStatus;

fn example() -> ResubmissionSummary {
    ResubmissionSummary {
        assignment_id: "ca-01ARZ3NDEKTSV4RRFFQ69G5FAV".parse().unwrap(),
        previous_result: serde_json::json!({"status": "completed", "summary": "Original submission.", "result_commit": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}),
        task: TaskSummary {
            id: "ct-01ARZ3NDEKTSV4RRFFQ69G5FAV".parse().unwrap(),
            project_id: "cp-01ARZ3NDEKTSV4RRFFQ69G5FAV".parse().unwrap(),
            project: "primary".into(),
            title: "Correct the submitted result".into(),
            description: "Include the final validated commit.".into(),
            status: TaskStatus::Submitted,
            ready: false,
            unresolved_dependencies: vec![],
            result: Some(
                serde_json::json!({"status": "completed", "summary": "Validated the correction.", "result_commit": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"}),
            ),
        },
    }
}

fn schema() -> Schema {
    generated_schema_for::<MutationSuccessEnvelope<'static, ResubmissionSummary>>(
    )
}

#[test]
fn resubmit_output_schema_and_example_match_the_typed_contract() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let expected: Value = serde_json::from_str(
        &std::fs::read_to_string(
            root.join("schemas/cli-resubmit-v1.schema.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(serde_json::to_value(schema()).unwrap(), expected);
    let expected: Value = serde_json::from_str(
        &std::fs::read_to_string(root.join("examples/resubmit.json")).unwrap(),
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
    let response = crate::protocol::RpcResponse::TaskResubmitted {
        operation_id,
        submission: example(),
    };
    let mut wire = serde_json::to_value(response).unwrap();
    wire.as_object_mut().unwrap().remove("result");
    wire.as_object_mut().unwrap().remove("operation_id");
    assert_eq!(wire, expected["data"]);
}

#[test]
#[ignore = "regenerates the reviewed resubmission schema and example"]
fn regenerate_resubmit_contract() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    std::fs::write(
        root.join("schemas/cli-resubmit-v1.schema.json"),
        format!("{}\n", serde_json::to_string_pretty(&schema()).unwrap()),
    )
    .unwrap();
    let output = MutationSuccessEnvelope {
        schema_version: SchemaVersion,
        operation_id: "co-01ARZ3NDEKTSV4RRFFQ69G5FAV".parse().unwrap(),
        data: &example(),
    };
    std::fs::write(
        root.join("examples/resubmit.json"),
        format!("{}\n", serde_json::to_string_pretty(&output).unwrap()),
    )
    .unwrap();
}

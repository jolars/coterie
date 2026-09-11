use super::*;
use crate::protocol::progress::{
    AssignmentState, ProgressChange, ProgressPage, ProgressState,
};
use crate::providers::LifecycleState;
use crate::tasks::TaskStatus;

fn example() -> ProgressPage {
    let run_id = "cr-01ARZ3NDEKTSV4RRFFQ69G5FAV".parse().unwrap();
    let task_id = "ct-01ARZ3NDEKTSV4RRFFQ69G5FAW".parse().unwrap();
    let project_id = "cp-01ARZ3NDEKTSV4RRFFQ69G5FAX".parse().unwrap();
    let agent_id = "cg-01ARZ3NDEKTSV4RRFFQ69G5FAY".parse().unwrap();
    let assignment_id = "ca-01ARZ3NDEKTSV4RRFFQ69G5FAZ".parse().unwrap();
    let session_id = "cs-01ARZ3NDEKTSV4RRFFQ69G5FB0".parse().unwrap();
    ProgressPage {
        run_id,
        changes: vec![
            ProgressChange {
                sequence: 41,
                state: ProgressState::Task {
                    task_id,
                    project_id,
                    status: TaskStatus::Submitted,
                },
            },
            ProgressChange {
                sequence: 42,
                state: ProgressState::Assignment {
                    assignment_id,
                    task_id,
                    agent_id,
                    state: AssignmentState::Completed,
                },
            },
            ProgressChange {
                sequence: 45,
                state: ProgressState::Session {
                    session_id,
                    agent_id,
                    generation: 1,
                    state: LifecycleState::Exited,
                },
            },
            ProgressChange {
                sequence: 46,
                state: ProgressState::Agent {
                    agent_id,
                    generation: 1,
                    state: LifecycleState::Exited,
                },
            },
        ],
        next_cursor: format!("p1:{run_id}:operator:46"),
        has_more: false,
        timed_out: false,
    }
}

fn schema() -> Schema {
    generated_schema_for::<SuccessEnvelope<'static, ProgressPage>>()
}

#[test]
fn progress_cli_accepts_bounded_reads_and_waits_without_operation_ids() {
    let parsed = Arguments::try_parse_from([
        "coterie",
        "progress",
        "--json",
        "--after",
        "opaque-cursor",
        "--limit",
        "2",
        "--wait",
        "5",
    ])
    .unwrap();
    assert!(parsed.json);
    assert!(
        matches!(parsed.command, Some(Command::Progress(ProgressArguments { after: Some(ref after), limit: 2, wait: 5 })) if after == "opaque-cursor")
    );
    for args in [
        vec!["--limit", "0"],
        vec!["--limit", "101"],
        vec!["--wait", "6"],
        vec!["--wait", "-1"],
        vec!["--operation-id", "co-01ARZ3NDEKTSV4RRFFQ69G5FAV"],
    ] {
        assert!(
            Arguments::try_parse_from(
                ["coterie", "progress"].into_iter().chain(args)
            )
            .is_err()
        );
    }
}

#[test]
fn progress_output_schema_and_example_match_the_typed_contract() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let expected: Value = serde_json::from_str(
        &std::fs::read_to_string(
            root.join("schemas/cli-progress-v1.schema.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(serde_json::to_value(schema()).unwrap(), expected);
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    assert_eq!(
        render_json_success(&mut stdout, &mut stderr, &example()).unwrap(),
        ExitCategory::Success
    );
    assert!(stderr.is_empty());
    let expected: Value = serde_json::from_str(
        &std::fs::read_to_string(root.join("examples/progress.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(serde_json::from_slice::<Value>(&stdout).unwrap(), expected);
    let mut human = Vec::new();
    render_human_success(&mut human, &mut stderr, &example()).unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&human).unwrap(),
        expected["data"]
    );
}

#[test]
fn progress_maximum_page_fits_the_documented_byte_budget() {
    let mut page = example();
    page.next_cursor = format!(
        "p1:{}:cg-01ARZ3NDEKTSV4RRFFQ69G5FAV:{}",
        page.run_id,
        i64::MAX
    );
    let longest = ProgressChange {
        sequence: i64::MAX as u64,
        state: ProgressState::AssignmentSession {
            assignment_id: "ca-01ARZ3NDEKTSV4RRFFQ69G5FAV".parse().unwrap(),
            task_id: "ct-01ARZ3NDEKTSV4RRFFQ69G5FAV".parse().unwrap(),
            agent_id: "cg-01ARZ3NDEKTSV4RRFFQ69G5FAV".parse().unwrap(),
            session_id: "cs-01ARZ3NDEKTSV4RRFFQ69G5FAV".parse().unwrap(),
        },
    };
    page.changes = vec![longest; 100];
    let mut stdout = Vec::new();
    render_json_success(&mut stdout, &mut Vec::new(), &page).unwrap();
    assert!(stdout.len() < 64 * 1024);
}

#[test]
#[ignore = "regenerates the reviewed progress schema and example"]
fn regenerate_progress_contract() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    std::fs::write(
        root.join("schemas/cli-progress-v1.schema.json"),
        format!("{}\n", serde_json::to_string_pretty(&schema()).unwrap()),
    )
    .unwrap();
    let output = SuccessEnvelope {
        schema_version: SchemaVersion,
        data: &example(),
    };
    std::fs::write(
        root.join("examples/progress.json"),
        format!("{}\n", serde_json::to_string_pretty(&output).unwrap()),
    )
    .unwrap();
}

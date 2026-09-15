use super::*;

#[test]
fn recovery_requires_assignment_reason_and_accepts_an_operation_id() {
    let assignment = "ca-01ARZ3NDEKTSV4RRFFQ69G5FAV";
    let operation = "co-01ARZ3NDEKTSV4RRFFQ69G5FAV";
    assert!(
        Arguments::try_parse_from([
            "coterie",
            "task",
            "recover",
            "--assignment",
            assignment,
            "--reason",
            "The provider exited before submission.",
            "--operation-id",
            operation,
            "--json",
        ])
        .is_ok()
    );
    for args in [
        vec!["coterie", "task", "recover", "--assignment", assignment],
        vec!["coterie", "task", "recover", "--reason", "Interrupted."],
    ] {
        assert!(Arguments::try_parse_from(args).is_err());
    }
}

fn example() -> crate::protocol::RecoverySummary {
    crate::protocol::RecoverySummary {
        task_id: "ct-01ARZ3NDEKTSV4RRFFQ69G5FAV".parse().unwrap(),
        assignment_id: "ca-01ARZ3NDEKTSV4RRFFQ69G5FAV".parse().unwrap(),
        session_id: "cs-01ARZ3NDEKTSV4RRFFQ69G5FAV".parse().unwrap(),
        project_id: "cp-01ARZ3NDEKTSV4RRFFQ69G5FAV".parse().unwrap(),
        generation: 0,
        workspace_path: "/state/preserved".into(),
        workspace_path_bytes: b"/state/preserved".to_vec(),
        base_commit: Some("a".repeat(40)),
        reason: "Provider exited before submission.".into(),
        continuation_assignment_id: None,
        handoff: Some(Box::new(handoff_example().brief())),
    }
}

fn handoff_example() -> crate::protocol::recovery::RecoveryHandoff {
    use crate::protocol::recovery::*;
    let staged = RecoveryPath {
        path: "result.json".into(),
        path_bytes: b"result.json".to_vec(),
    };
    RecoveryHandoff {
        operation_id: "co-01ARZ3NDEKTSV4RRFFQ69G5FAV".parse().unwrap(),
        source_assignment_id: "ca-01ARZ3NDEKTSV4RRFFQ69G5FAV".parse().unwrap(),
        recorded_at: 1_789_430_400,
        reported_by: None,
        mechanical: RecoverySnapshot {
            head_commit: "a".repeat(40), complete: true, operation_in_progress: false,
            dirty_paths: vec![staged.clone()], staged_paths: vec![staged],
            unstaged_paths: vec![], untracked_paths: vec![], conflicted_paths: vec![],
            unreadable_paths: vec![], hidden_index_paths: vec![],
        },
        reported: RecoveryReport {
            validation_evidence: vec![ReportedEvidence {
                text: "python3 validate.py passed in the source worktree; full suite blocked by Nix daemon access.".into(),
                source: "message cm-01ARZ3NDEKTSV4RRFFQ69G5FAW".into(),
            }],
            unfinished_steps: vec![ReportedEvidence {
                text: "Port result.json, rerun validation, and request a coordinator commit before submission.".into(),
                source: "message cm-01ARZ3NDEKTSV4RRFFQ69G5FAW".into(),
            }],
        },
    }
}

fn schema() -> Schema {
    generated_schema_for::<
        MutationSuccessEnvelope<'static, crate::protocol::RecoverySummary>,
    >()
}

fn report_schema() -> Schema {
    generated_schema_for::<crate::protocol::recovery::RecoveryReport>()
}

#[test]
fn recovery_output_matches_generated_schema_example_and_human_contract() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let expected: Value = serde_json::from_str(
        &std::fs::read_to_string(
            root.join("schemas/cli-recover-v1.schema.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(serde_json::to_value(schema()).unwrap(), expected);
    let report_contract: Value = serde_json::from_slice(
        &std::fs::read(root.join("schemas/recovery-report-v1.schema.json"))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        serde_json::to_value(report_schema()).unwrap(),
        report_contract
    );
    for (path, generated) in [
        (
            "examples/recovery-report.json",
            serde_json::to_value(handoff_example().reported).unwrap(),
        ),
        (
            "examples/recovery-handoff.json",
            serde_json::to_value(handoff_example()).unwrap(),
        ),
    ] {
        let expected: Value =
            serde_json::from_slice(&std::fs::read(root.join(path)).unwrap())
                .unwrap();
        assert_eq!(generated, expected);
    }
    let expected: Value = serde_json::from_str(
        &std::fs::read_to_string(root.join("examples/recover.json")).unwrap(),
    )
    .unwrap();
    let operation_id = "co-01ARZ3NDEKTSV4RRFFQ69G5FAV".parse().unwrap();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
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
    let mut wire =
        serde_json::to_value(crate::protocol::RpcResponse::TaskRecovered {
            operation_id,
            recovery: example(),
        })
        .unwrap();
    wire.as_object_mut().unwrap().remove("result");
    wire.as_object_mut().unwrap().remove("operation_id");
    assert_eq!(wire, expected["data"]);
}

#[test]
#[ignore = "regenerates the recovery schema and example"]
fn regenerate_recovery_contract() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    std::fs::write(
        root.join("schemas/cli-recover-v1.schema.json"),
        format!("{}\n", serde_json::to_string_pretty(&schema()).unwrap()),
    )
    .unwrap();
    std::fs::write(
        root.join("schemas/recovery-report-v1.schema.json"),
        format!(
            "{}\n",
            serde_json::to_string_pretty(&report_schema()).unwrap()
        ),
    )
    .unwrap();
    for (path, value) in [
        (
            "examples/recovery-report.json",
            serde_json::to_value(handoff_example().reported).unwrap(),
        ),
        (
            "examples/recovery-handoff.json",
            serde_json::to_value(handoff_example()).unwrap(),
        ),
    ] {
        std::fs::write(
            root.join(path),
            format!("{}\n", serde_json::to_string_pretty(&value).unwrap()),
        )
        .unwrap();
    }
    let output = MutationSuccessEnvelope {
        schema_version: SchemaVersion,
        operation_id: "co-01ARZ3NDEKTSV4RRFFQ69G5FAV".parse().unwrap(),
        data: &example(),
    };
    std::fs::write(
        root.join("examples/recover.json"),
        format!("{}\n", serde_json::to_string_pretty(&output).unwrap()),
    )
    .unwrap();
}

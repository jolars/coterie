use super::*;
use crate::protocol::CommitHandoff;

fn example() -> CommitHandoff {
    CommitHandoff {
        assignment_id: "ca-01ARZ3NDEKTSV4RRFFQ69G5FAV".parse().unwrap(),
        agent_id: "cg-01ARZ3NDEKTSV4RRFFQ69G5FAV".parse().unwrap(),
        task_id: "ct-01ARZ3NDEKTSV4RRFFQ69G5FAV".parse().unwrap(),
        generation: 0,
        workspace_path: "/state/worktree".into(),
        workspace_path_bytes: b"/state/worktree".to_vec(),
        owned_reference: "refs/heads/coterie/cr-01ARZ3NDEKTSV4RRFFQ69G5FAV/ca-01ARZ3NDEKTSV4RRFFQ69G5FAV".into(),
        base_commit: Some("a".repeat(40)),
        provider: "codex".into(),
        permission_profile: crate::config::PermissionProfile {
            filesystem: crate::config::FilesystemPolicy::WorkspaceWrite,
            network: crate::config::NetworkPolicy::Deny,
            approvals: crate::config::ApprovalPolicy::Never,
            approval_reviewer: crate::config::ApprovalReviewer::User,
        },
    }
}

#[test]
fn commit_handoff_schema_and_example_match_typed_context() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let schema: Value = serde_json::from_slice(
        &std::fs::read(root.join("schemas/commit-handoff-v1.schema.json"))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        serde_json::to_value(generated_schema_for::<CommitHandoff>()).unwrap(),
        schema
    );
    let expected: Value = serde_json::from_slice(
        &std::fs::read(root.join("examples/commit-handoff.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(serde_json::to_value(example()).unwrap(), expected);
}

#[test]
#[ignore = "regenerates the commit handoff schema and example"]
fn regenerate_commit_handoff_contract() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for (path, value) in [
        (
            "schemas/commit-handoff-v1.schema.json",
            serde_json::to_value(generated_schema_for::<CommitHandoff>())
                .unwrap(),
        ),
        (
            "examples/commit-handoff.json",
            serde_json::to_value(example()).unwrap(),
        ),
    ] {
        std::fs::write(
            root.join(path),
            format!("{}\n", serde_json::to_string_pretty(&value).unwrap()),
        )
        .unwrap();
    }
}

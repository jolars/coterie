use super::*;
use crate::state::OperatorClosureOverride;

fn arguments() -> Vec<&'static str> {
    vec![
        "coterie",
        "task",
        "close",
        "ct-01ARZ3NDEKTSV4RRFFQ69G5FAV",
        "--summary",
        "Tests passed.",
        "--override",
        "--assignment",
        "ca-01ARZ3NDEKTSV4RRFFQ69G5FAW",
        "--result-commit",
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "--target-commit",
        "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        "--reason",
        "Reviewed external cherry-pick.",
    ]
}

#[test]
fn closure_override_requires_all_flags_together() {
    let arguments = arguments();
    let parsed = Arguments::try_parse_from(&arguments).unwrap();
    assert!(matches!(
        parsed.command,
        Some(Command::Task(TaskArguments {
            command: TaskCommand::Close(TaskCloseArguments {
                operator_override: true,
                ..
            })
        }))
    ));
    for flag in [
        "--override",
        "--assignment",
        "--result-commit",
        "--target-commit",
        "--reason",
    ] {
        let mut missing = arguments.clone();
        let index = missing
            .iter()
            .position(|argument| *argument == flag)
            .unwrap();
        missing.remove(index);
        if flag != "--override" {
            missing.remove(index);
        }
        assert!(
            Arguments::try_parse_from(missing).is_err(),
            "missing {flag}"
        );
    }
}

#[test]
fn closure_override_schema_matches_typed_evidence() {
    let schema = generated_schema_for::<OperatorClosureOverride>();
    let expected: Value = serde_json::from_str(include_str!(
        "../../schemas/cli-closure-override-v1.schema.json"
    ))
    .unwrap();
    assert_eq!(serde_json::to_value(schema).unwrap(), expected);
}

#[test]
#[ignore = "explicit schema regeneration"]
fn regenerate_closure_override_schema() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("schemas/cli-closure-override-v1.schema.json");
    let schema = generated_schema_for::<OperatorClosureOverride>();
    std::fs::write(
        path,
        format!("{}\n", serde_json::to_string_pretty(&schema).unwrap()),
    )
    .unwrap();
}

use super::*;
use crate::protocol::run_recovery::{RetainedRuns, RunRecoveryReport};

fn contracts() -> [(&'static str, Schema); 2] {
    [
        (
            "schemas/cli-run-list-v1.schema.json",
            generated_schema_for::<SuccessEnvelope<'static, RetainedRuns>>(),
        ),
        (
            "schemas/cli-run-recover-v1.schema.json",
            generated_schema_for::<
                MutationSuccessEnvelope<'static, RunRecoveryReport>,
            >(),
        ),
    ]
}

#[test]
fn run_recovery_contracts_match_typed_output() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for (path, schema) in contracts() {
        let expected: Value =
            serde_json::from_slice(&std::fs::read(root.join(path)).unwrap())
                .unwrap();
        assert_eq!(serde_json::to_value(schema).unwrap(), expected);
    }
    for args in [
        vec!["coterie", "run", "recover"],
        vec!["coterie", "run", "recover", "cr-01ARZ3NDEKTSV4RRFFQ69G5FAV"],
        vec![
            "coterie",
            "run",
            "recover",
            "../run",
            "--reason",
            "Invalid ID",
        ],
    ] {
        assert!(Arguments::try_parse_from(args).is_err());
    }
}

#[test]
#[ignore = "explicitly regenerates retained-run CLI schemas"]
fn regenerate_run_recovery_contracts() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for (path, schema) in contracts() {
        std::fs::write(
            root.join(path),
            format!("{}\n", serde_json::to_string_pretty(&schema).unwrap()),
        )
        .unwrap();
    }
}

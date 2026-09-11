use super::*;

#[test]
fn coordination_bootstrap_follows_capabilities_for_any_role_name() {
    for role in ["coordinator", "lead", "worker", "custom_planner"] {
        for (grants, read, integrate, close) in [
            ("\"task:*\", \"workspace:*\"", true, true, true),
            (
                "\"task:read\", \"workspace:integrate\", \"task:close\"",
                true,
                true,
                true,
            ),
            ("\"task:read\"", true, false, false),
            ("\"task:close\"", false, false, true),
            ("\"workspace:integrate\"", false, true, false),
            ("\"spawn:builder\", \"send:*\"", false, false, false),
            ("", false, false, false),
        ] {
            let global = include_str!("../../examples/config/global.toml")
                .replace("coordinator", role)
                .replace(
                    "\"spawn:builder\", \"send:*\", \"task:*\", \"logs:*\"",
                    grants,
                );
            let config = crate::config::resolve(
                &toml::from_str(&global).unwrap(),
                &Default::default(),
                &Default::default(),
            )
            .unwrap();
            let bootstrap =
                bootstrap_instruction(RunId::generate(), role, &config);
            assert!(bootstrap.contains("Coordinate tasks through Coterie."));
            assert!(
                bootstrap.contains("If you are coordinating delegated work")
            );
            assert!(bootstrap.contains("unless the user pauses"));
            assert!(bootstrap.contains("inbox --after <inbox_cursor>"));
            assert!(bootstrap.contains("inbox ack <inbox_cursor>"));
            assert!(bootstrap.contains("only after handling"));
            assert!(bootstrap.contains("separate"));
            assert!(bootstrap.contains("review"));
            assert!(bootstrap.contains("validation"));
            assert!(bootstrap.contains("Report blockers"));
            assert!(
                bootstrap.contains("do not resume an idle foreground provider")
            );
            assert!(bootstrap.contains("capability-probed provider support"));
            assert_eq!(
                bootstrap
                    .contains("progress --after <progress_cursor> --wait 5"),
                read
            );
            assert_eq!(bootstrap.contains("has_more"), read);
            assert_eq!(
                bootstrap.contains("workspace integrate --assignment"),
                integrate
            );
            assert_eq!(
                bootstrap.contains("task close <task_id> --summary"),
                close
            );
        }
    }
}

#[test]
fn builtin_coordination_bootstrap_preserves_configured_instructions() {
    let config = crate::config::resolve(
        &Default::default(),
        &Default::default(),
        &Default::default(),
    )
    .unwrap();
    let bootstrap = bootstrap_instruction(RunId::generate(), "lead", &config);
    assert!(bootstrap.contains("Delegate independent implementation"));
    assert!(bootstrap.contains("progress --after <progress_cursor> --wait 5"));
    assert!(bootstrap.contains("workspace integrate --assignment"));
    assert!(bootstrap.contains("task close <task_id> --summary"));
    assert!(bootstrap.contains("AGENTS.md"));
    assert!(
        bootstrap
            .contains("commit any intended Git worktree changes successfully")
    );
}

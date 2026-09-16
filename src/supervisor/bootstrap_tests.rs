use super::*;

#[test]
fn worktree_bootstrap_diagnoses_commit_handoff_before_edits() {
    let global = include_str!("../../examples/config/global.toml");
    for read_only in [false, true] {
        let input = if read_only {
            global.replace(
                "permission_profile = \"implementation\"",
                "permission_profile = \"inspect\"",
            )
        } else {
            global.to_owned()
        };
        let config = crate::config::resolve(
            &toml::from_str(&input).unwrap(),
            &Default::default(),
            &Default::default(),
        )
        .unwrap();
        let bootstrap =
            bootstrap_instruction(RunId::generate(), "builder", &config);
        assert_eq!(
            bootstrap.contains(
                "Direct worker staging and committing is unsupported"
            ),
            !read_only
        );
        if !read_only {
            for expected in [
                "before editing",
                "commit_handoffs",
                "intended paths",
                "validation commands",
                "stop editing",
                "full commit ID",
                "common Git directory",
            ] {
                assert!(
                    bootstrap.contains(expected),
                    "missing {expected}: {bootstrap}"
                );
            }
        }
    }
}

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
            assert!(bootstrap.contains("`poll`"));
            assert!(
                bootstrap.contains("`inbox_handled` with their message_ids")
            );
            assert!(bootstrap.contains("only after handling"));
            assert!(bootstrap.contains("never acknowledges"));
            assert!(bootstrap.contains("review"));
            assert!(bootstrap.contains("validation"));
            assert!(bootstrap.contains("Report blockers"));
            assert!(bootstrap.contains("prime.notifications"));
            assert!(bootstrap.contains("automatic"));
            assert!(bootstrap.contains("may end your turn"));
            assert!(bootstrap.contains("polling fallback"));
            assert_eq!(
                bootstrap.contains("polling fallback with wait_seconds=5"),
                read
            );
            assert_eq!(bootstrap.contains("has_more"), read);
            assert_eq!(
                bootstrap.contains("`workspace_integrate` with assignment_id="),
                integrate
            );
            assert_eq!(
                bootstrap.contains(
                    "`task_close` with task_id=<task_id> and summary="
                ),
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
    assert!(bootstrap.contains("polling fallback with wait_seconds=5"));
    assert!(bootstrap.contains("`workspace_integrate` with assignment_id="));
    assert!(
        bootstrap.contains("`task_close` with task_id=<task_id> and summary=")
    );
    assert!(bootstrap.contains("AGENTS.md"));
    assert!(
        bootstrap
            .contains("commit any intended Git worktree changes successfully")
    );
}

use super::*;
use crate::protocol::context::{
    AssignmentDetail, DetailPage, LogsPage, PrimePage, TaskContext, TaskDetail,
};

fn schemas() -> Vec<(&'static str, Schema)> {
    vec![
        (
            "cli-prime-v1.schema.json",
            generated_schema_for::<SuccessEnvelope<'static, PrimePage>>(),
        ),
        (
            "cli-detail-v1.schema.json",
            generated_schema_for::<SuccessEnvelope<'static, DetailPage>>(),
        ),
        (
            "cli-logs-v1.schema.json",
            generated_schema_for::<SuccessEnvelope<'static, LogsPage>>(),
        ),
        (
            "task-detail-v1.schema.json",
            generated_schema_for::<TaskDetail>(),
        ),
        (
            "assignment-detail-v1.schema.json",
            generated_schema_for::<AssignmentDetail>(),
        ),
    ]
}

fn example() -> PrimePage {
    use crate::protocol::context::{NextAction, TaskBrief, TextPreview};
    let task = TaskBrief {
        id: "ct-01ARZ3NDEKTSV4RRFFQ69G5FAW".parse().unwrap(),
        project_id: "cp-01ARZ3NDEKTSV4RRFFQ69G5FAX".parse().unwrap(),
        project: TextPreview::new("primary"),
        title: TextPreview::new("Validate the parser"),
        description: TextPreview::new(&"Acceptance criteria. ".repeat(40)),
        status: crate::tasks::TaskStatus::Open,
        ready: true,
        unresolved_dependencies: Vec::new(),
        omitted_dependencies: 0,
        result: None,
        assignment: None,
        next_action: NextAction::Ready,
    };
    PrimePage {
        admission: Some(Box::new(admission_example())),
        session: None,
        notifications: Default::default(),
        identity: crate::protocol::CallerSummary {
            run_id: "cr-01ARZ3NDEKTSV4RRFFQ69G5FAV".parse().unwrap(),
            channel: crate::protocol::CallerChannel::Operator,
            agent: None,
        },
        projects: vec![crate::protocol::ProjectSummary {
            id: task.project_id,
            alias: "primary".into(),
            root: "/project".into(),
            access: "read_write".into(),
        }],
        peers: Vec::new(),
        context: TaskContext {
            tasks: vec![task.clone()],
            ready_tasks: vec![task.id],
            next_task: Some(task.id),
            ..TaskContext::default()
        },
        commit_handoffs: Vec::new(),
        commands: vec![
            "prime".into(),
            "task show".into(),
            "assignment show".into(),
            "logs".into(),
        ],
    }
}

fn admission_example() -> crate::protocol::admission::AdmissionView {
    use crate::protocol::admission::{
        AdmissionBudget, AdmissionView, CapacityRelease, PolicySource,
        RoleAdmission,
    };
    use crate::protocol::context::TextPreview;
    let config = crate::config::resolve(
        &Default::default(),
        &Default::default(),
        &Default::default(),
    )
    .unwrap();
    let source = |field: &str| {
        let source = &config.provenance[field].source;
        Some(PolicySource {
            layer: source.layer,
            field: TextPreview::new(&source.field),
        })
    };
    let budget = |field: &str, limit: u16, released_by| AdmissionBudget {
        used: 0,
        limit: Some(limit),
        remaining: Some(u64::from(limit)),
        exhausted: false,
        released_by,
        source: source(field),
    };
    AdmissionView {
        observed_at: 1_700_000_000,
        concurrent_agents: budget("limits.max_concurrent_agents", config.limits.max_concurrent_agents, CapacityRelease::AgentTerminal),
        agents_per_run: budget("limits.max_agents_per_run", config.limits.max_agents_per_run, CapacityRelease::Never),
        spawns_per_minute: budget("limits.max_spawns_per_minute", config.limits.max_spawns_per_minute, CapacityRelease::RollingMinute),
        roles: ["reviewer", "worker"].into_iter().map(|name| RoleAdmission {
            role: TextPreview::new(name),
            enabled: true,
            enabled_source: source(&format!("roles.{name}.enabled")),
            active_instances: budget(&format!("roles.{name}.max_instances"), config.roles[name].max_instances.unwrap(), CapacityRelease::AgentTerminal),
        }).collect(),
        omitted_roles: 0,
        warning: None,
        guidance: "Inspect admission before recovery or review. Worker exit releases active slots, while lifetime admissions remain consumed. Rate slots expire after the rolling minute. A fresh continuation or review consumes another lifetime admission. Ask the operator to preserve work, stop the run, and start a new run if the lifetime budget is exhausted.".into(),
    }
}

#[test]
fn context_schemas_and_example_match_typed_outputs() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    for (name, generated) in schemas() {
        let expected: Value = serde_json::from_slice(
            &std::fs::read(root.join("schemas").join(name)).unwrap(),
        )
        .unwrap();
        assert_eq!(
            serde_json::to_value(generated).unwrap(),
            expected,
            "{name}"
        );
    }
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    render_json_success(&mut stdout, &mut stderr, &example()).unwrap();
    assert!(stderr.is_empty());
    let expected: Value = serde_json::from_slice(
        &std::fs::read(root.join("examples/prime.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(serde_json::from_slice::<Value>(&stdout).unwrap(), expected);
    stdout.clear();
    render_human_success(
        &mut stdout,
        &mut stderr,
        &serde_json::to_value(example()).unwrap(),
    )
    .unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&stdout).unwrap(),
        expected["data"]
    );
}

#[test]
fn context_cli_rejects_invalid_bounds_and_conflicting_tail_cursors() {
    for args in [
        vec!["prime", "--limit", "0"],
        vec!["prime", "--limit", "51"],
        vec![
            "task",
            "show",
            "ct-01ARZ3NDEKTSV4RRFFQ69G5FAW",
            "--limit",
            "65537",
        ],
        vec![
            "assignment",
            "show",
            "ca-01ARZ3NDEKTSV4RRFFQ69G5FAW",
            "--limit",
            "0",
        ],
        vec!["logs", "worker-1", "--tail", "--after", "1"],
    ] {
        assert!(
            Arguments::try_parse_from(std::iter::once("coterie").chain(args))
                .is_err()
        );
    }
    assert!(
        Arguments::try_parse_from([
            "coterie", "logs", "worker-1", "--tail", "--follow"
        ])
        .is_ok()
    );
}

#[test]
#[ignore = "explicit regeneration of reviewed context schemas and example"]
fn regenerate_context_contracts() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    for (name, schema) in schemas() {
        std::fs::write(
            root.join("schemas").join(name),
            format!("{}\n", serde_json::to_string_pretty(&schema).unwrap()),
        )
        .unwrap();
    }
    std::fs::write(
        root.join("examples/prime.json"),
        format!(
            "{}\n",
            serde_json::to_string_pretty(&SuccessEnvelope {
                schema_version: SchemaVersion,
                data: &example()
            })
            .unwrap()
        ),
    )
    .unwrap();
}

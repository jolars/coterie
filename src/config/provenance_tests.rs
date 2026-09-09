use super::resolution_tests::{CUSTOM, Fixture};
use super::*;

use std::collections::BTreeSet;
use std::path::Path;

fn assert_source(
    effective: &EffectiveConfig,
    field: &str,
    layer: ConfigLayer,
    file: Option<&Path>,
    input_field: &str,
) {
    let source = &effective.provenance[field].source;
    assert_eq!(source.layer, layer, "{field}");
    assert_eq!(source.file.as_deref(), file, "{field}");
    assert_eq!(source.field, input_field, "{field}");
}

fn assert_complete(effective: &EffectiveConfig) {
    fn leaves(
        value: &serde_json::Value,
        prefix: &str,
        keys: &mut BTreeSet<String>,
    ) {
        if let serde_json::Value::Object(fields) = value {
            for (field, value) in fields {
                let path = if prefix.is_empty() {
                    field.clone()
                } else {
                    format!("{prefix}.{field}")
                };
                leaves(value, &path, keys);
            }
        } else {
            // Arrays replace atomically, and absent optional values still have a default origin.
            keys.insert(prefix.into());
        }
    }
    let mut expected = BTreeSet::new();
    leaves(&serde_json::to_value(effective).unwrap(), "", &mut expected);
    assert_eq!(
        effective
            .provenance
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>(),
        expected
    );
}

#[test]
fn absent_files_have_complete_compiled_and_builtin_provenance() {
    let fixture = Fixture::new();
    let effective = fixture.load().unwrap();
    assert_complete(&effective);
    assert_source(
        &effective,
        "providers.codex.command",
        ConfigLayer::Compiled,
        None,
        "providers.codex.command",
    );
    assert_source(
        &effective,
        "archetype.lead",
        ConfigLayer::Builtin,
        None,
        "archetypes.builtin:standard@1.lead",
    );
    assert_source(
        &effective,
        "roles.lead.max_instances",
        ConfigLayer::Builtin,
        None,
        "archetypes.builtin:standard@1.roles.lead.max_instances",
    );
    assert_source(
        &effective,
        "roles.worker.enabled",
        ConfigLayer::Compiled,
        None,
        "roles.worker.enabled",
    );
    assert_source(
        &effective,
        "roles.worker.permission_profile.network",
        ConfigLayer::Builtin,
        None,
        "archetypes.builtin:standard@1.permission_profiles.worker.network",
    );
    let selection = effective.provenance["archetype.reference"]
        .selected_by
        .as_ref()
        .unwrap();
    assert_eq!(selection.layer, ConfigLayer::Compiled);
    assert_eq!(selection.field, "archetype");
    assert!(selection.file.is_none());
    assert!(
        effective
            .provenance
            .values()
            .all(|value| value.source.file.is_none())
    );
}

fn layered_fixture() -> (Fixture, OperatorOverrides) {
    let fixture = Fixture::new();
    fixture.write("parts/first.toml", &format!("{CUSTOM}\n[providers.codex]\ncommand = ['first', '--old']\n[limits]\nmax_agents_per_run = 30\nmax_concurrent_agents = 4"));
    fixture.write("parts/second.toml", "[permission_profiles.safe]\nnetwork = 'deny'\n[archetypes.'global:custom@1'.roles.builder]\ninstructions = 'From include'\nmax_instances = 4\n[limits]\nmax_concurrent_agents = 6");
    fixture.write("config.toml", "includes = ['parts/first.toml', 'parts/second.toml']\n[providers.codex]\ncommand = ['main']\n[archetypes.'global:custom@1'.roles.builder]\ncapabilities = []\nmax_instances = 3\n[supervision]\njob_timeout_seconds = 123");
    fixture.write("coterie.toml", "[roles.builder]\nenabled = false\nmax_instances = 1\npermission_profile = 'safe'\n[limits]\nmax_concurrent_agents = 2");
    let operator = OperatorOverrides {
        roles: BTreeMap::from([(
            "builder".into(),
            RoleRestriction {
                max_instances: Some(3),
                ..RoleRestriction::default()
            },
        )]),
        ..OperatorOverrides::default()
    };
    (fixture, operator)
}

#[test]
fn provenance_survives_partial_merges_profile_selection_and_operator_restoration()
 {
    let (fixture, operator) = layered_fixture();
    let first = fixture.0.join("parts/first.toml");
    let second = fixture.0.join("parts/second.toml");
    let main = fixture.0.join("config.toml");
    let project = fixture.0.join("coterie.toml");
    let effective = load(&fixture.locations(), &operator).unwrap();
    assert_complete(&effective);
    for (field, file, input) in [
        ("providers.codex.command", &main, "providers.codex.command"),
        (
            "limits.max_agents_per_run",
            &first,
            "limits.max_agents_per_run",
        ),
        (
            "supervision.job_timeout_seconds",
            &main,
            "supervision.job_timeout_seconds",
        ),
        (
            "archetype.roles.builder.instructions",
            &second,
            "archetypes.global:custom@1.roles.builder.instructions",
        ),
        (
            "archetype.roles.builder.capabilities",
            &main,
            "archetypes.global:custom@1.roles.builder.capabilities",
        ),
        (
            "roles.builder.permission_profile.network",
            &second,
            "permission_profiles.safe.network",
        ),
        (
            "roles.builder.permission_profile.filesystem",
            &first,
            "permission_profiles.safe.filesystem",
        ),
    ] {
        assert_source(
            &effective,
            field,
            ConfigLayer::Global,
            Some(file),
            input,
        );
    }
    assert_source(
        &effective,
        "roles.builder.enabled",
        ConfigLayer::Project,
        Some(&project),
        "roles.builder.enabled",
    );
    assert_source(
        &effective,
        "limits.max_concurrent_agents",
        ConfigLayer::Project,
        Some(&project),
        "limits.max_concurrent_agents",
    );
    assert_source(
        &effective,
        "roles.builder.max_instances",
        ConfigLayer::Operator,
        None,
        "roles.builder.max_instances",
    );
    assert_source(
        &effective,
        "archetype.roles.coordinator.instructions",
        ConfigLayer::Compiled,
        None,
        "archetypes.global:custom@1.roles.coordinator.instructions",
    );
    let selection =
        effective.provenance["roles.builder.permission_profile.network"]
            .selected_by
            .as_ref()
            .unwrap();
    assert_eq!(selection.layer, ConfigLayer::Project);
    assert_eq!(selection.file.as_ref(), Some(&project));
    assert_eq!(selection.field, "roles.builder.permission_profile");
    assert_eq!(effective, load(&fixture.locations(), &operator).unwrap());
}

#[test]
fn equal_explicit_values_change_provenance_but_empty_tables_do_not() {
    let fixture = Fixture::new();
    let main = fixture.write("config.toml", "[providers.codex]\n[limits]\nmax_agents_per_run = 16\n[permission_profiles.worker]\nfilesystem = 'read-only'\nnetwork = 'deny'\napprovals = 'never'");
    let project = fixture.write(
        "coterie.toml",
        "archetype = 'builtin:standard@1'\n[roles.worker]\nmax_instances = 3",
    );
    let effective = fixture.load().unwrap();
    assert_complete(&effective);
    assert_source(
        &effective,
        "providers.codex.command",
        ConfigLayer::Compiled,
        None,
        "providers.codex.command",
    );
    assert_source(
        &effective,
        "limits.max_agents_per_run",
        ConfigLayer::Global,
        Some(&main),
        "limits.max_agents_per_run",
    );
    assert_source(
        &effective,
        "roles.worker.max_instances",
        ConfigLayer::Project,
        Some(&project),
        "roles.worker.max_instances",
    );
    assert_source(
        &effective,
        "roles.worker.permission_profile.filesystem",
        ConfigLayer::Builtin,
        None,
        "archetypes.builtin:standard@1.permission_profiles.worker.filesystem",
    );
    let selection = effective.provenance["archetype.reference"]
        .selected_by
        .as_ref()
        .unwrap();
    assert_eq!(selection.layer, ConfigLayer::Project);
    assert_eq!(selection.file.as_ref(), Some(&project));
}

#[test]
fn operator_selection_retains_the_trusted_definition_and_replaces_project_origins()
 {
    let (fixture, mut operator) = layered_fixture();
    fixture.write("coterie.toml", "archetype = 'builtin:standard@1'\n[roles.builder]\nenabled = false\npermission_profile = 'safe'\n[limits]\nmax_agents_per_run = 4");
    operator.archetype = Some("global:custom@1".into());
    operator.limits.max_agents_per_run = Some(30);
    let builder = operator.roles.get_mut("builder").unwrap();
    builder.enabled = Some(true);
    builder.permission_profile = Some("safe".into());
    let effective = load(&fixture.locations(), &operator).unwrap();
    assert_complete(&effective);
    for field in [
        "roles.builder.enabled",
        "roles.builder.max_instances",
        "limits.max_agents_per_run",
    ] {
        assert_source(&effective, field, ConfigLayer::Operator, None, field);
    }
    for field in [
        "archetype.reference",
        "roles.builder.permission_profile.filesystem",
        "roles.builder.permission_profile.network",
        "roles.builder.permission_profile.approvals",
    ] {
        let provenance = &effective.provenance[field];
        assert_eq!(provenance.source.layer, ConfigLayer::Global);
        let selector = provenance.selected_by.as_ref().unwrap();
        assert_eq!(selector.layer, ConfigLayer::Operator);
        assert!(selector.file.is_none());
    }
}

#[test]
fn source_files_keep_the_opened_symlink_path() {
    let fixture = Fixture::new();
    fixture.write("parts/actual.toml", "[limits]\nmax_agents_per_run = 10");
    let alias = fixture.0.join("alias.toml");
    std::os::unix::fs::symlink("parts/actual.toml", &alias).unwrap();
    fixture.write("config.toml", "includes = ['alias.toml']");
    assert_source(
        &fixture.load().unwrap(),
        "limits.max_agents_per_run",
        ConfigLayer::Global,
        Some(&alias),
        "limits.max_agents_per_run",
    );
}

#[test]
fn pure_resolution_has_layers_and_fields_without_inventing_files() {
    let global: GlobalConfig = toml::from_str(CUSTOM).unwrap();
    let effective = resolve(
        &global,
        &ProjectConfig::default(),
        &OperatorOverrides::default(),
    )
    .unwrap();
    assert_complete(&effective);
    assert_source(
        &effective,
        "archetype.lead",
        ConfigLayer::Global,
        None,
        "archetypes.global:custom@1.lead",
    );
    for value in effective.provenance.values() {
        assert!(value.source.file.is_none());
        assert!(
            value
                .selected_by
                .as_ref()
                .is_none_or(|source| source.file.is_none())
        );
    }
}

#[test]
fn provenance_contains_locations_without_copying_configuration_values() {
    let fixture = Fixture::new();
    fixture.write("config.toml", &format!("{CUSTOM}\n[providers.codex]\ncommand = ['provider', 'private-argument-marker']\n[archetypes.'global:custom@1'.roles.extra]\nprovider = 'codex'\nmode = 'job'\nworkspace = 'read-only'\npermission_profile = 'safe'\ninstructions = 'private-instruction-marker'\ncapabilities = []"));
    let effective = fixture.load().unwrap();
    assert_complete(&effective);
    let metadata = serde_json::to_string(&effective.provenance).unwrap();
    assert!(!metadata.contains("private-argument-marker"));
    assert!(!metadata.contains("private-instruction-marker"));
}

fn golden_provenance() -> [(std::path::PathBuf, String); 2] {
    let absent = Fixture::new();
    let (layered, overrides) = layered_fixture();
    [
        ("compiled", &absent, absent.load().unwrap()),
        (
            "layered",
            &layered,
            load(&layered.locations(), &overrides).unwrap(),
        ),
    ]
    .map(|(name, fixture, effective)| {
        assert_complete(&effective);
        let source_text = |source: &ConfigSource| {
            let file = source.file.as_ref().map_or_else(
                || "-".into(),
                |file| {
                    file.strip_prefix(&fixture.0).unwrap().display().to_string()
                },
            );
            format!("{}:{file}:{}", source.layer, source.field)
        };
        let mut text = String::new();
        for (field, value) in &effective.provenance {
            text.push_str(&format!(
                "{field} <- {}",
                source_text(&value.source)
            ));
            if let Some(selector) = &value.selected_by {
                text.push_str(&format!(
                    " (selected by {})",
                    source_text(selector)
                ));
            }
            text.push('\n');
        }
        (
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join(format!("tests/golden/config-provenance-{name}.txt")),
            text,
        )
    })
}

#[test]
fn provenance_matches_golden_contracts() {
    for (path, generated) in golden_provenance() {
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            generated,
            "{}",
            path.display()
        );
    }
}

#[test]
#[ignore = "explicit developer command to regenerate provenance snapshots"]
fn regenerate_provenance_snapshots() {
    for (path, generated) in golden_provenance() {
        std::fs::write(path, generated).unwrap();
    }
}

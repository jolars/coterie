use super::*;

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

pub(super) const CUSTOM: &str = r#"
archetype = "global:custom@1"
[permission_profiles.safe]
filesystem = "read-only"
network = "deny"
approvals = "never"
[archetypes."global:custom@1"]
lead = "coordinator"
[archetypes."global:custom@1".roles.coordinator]
provider = "codex"
mode = "interactive"
workspace = "project"
permission_profile = "safe"
capabilities = ["spawn:builder", "send:*"]
[archetypes."global:custom@1".roles.builder]
provider = "codex"
mode = "job"
workspace = "worktree"
permission_profile = "safe"
max_instances = 5
capabilities = ["send:coordinator", "task:read"]
"#;

pub(super) struct Fixture(pub(super) PathBuf);

impl Fixture {
    pub(super) fn new() -> Self {
        let path = std::env::temp_dir()
            .join(format!("coterie-config-{}", ulid::Ulid::generate()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    pub(super) fn write(&self, name: &str, contents: &str) -> PathBuf {
        let path = self.0.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, contents).unwrap();
        path
    }

    pub(super) fn locations(&self) -> ConfigLocations {
        ConfigLocations {
            global: Some(self.0.join("config.toml")),
            project: self.0.join("coterie.toml"),
        }
    }

    pub(super) fn load(&self) -> Result<EffectiveConfig, ConfigError> {
        load(&self.locations(), &OperatorOverrides::default())
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn absent_layers_resolve_to_the_compiled_policy() {
    let effective = resolve(
        &GlobalConfig::default(),
        &ProjectConfig::default(),
        &OperatorOverrides::default(),
    )
    .expect("absent configuration should use compiled defaults");
    assert_eq!(effective.archetype, builtin_standard());
    assert_eq!(effective.limits, compiled_defaults().limits);
    assert_eq!(effective.providers, compiled_defaults().providers);
    assert_eq!(effective.supervision, compiled_defaults().supervision);
    assert!(effective.roles.values().all(|role| role.enabled));
}

#[test]
fn operator_can_restore_project_capacity_within_the_trusted_baseline() {
    let project = ProjectConfig {
        roles: BTreeMap::from([(
            "worker".into(),
            RoleRestriction {
                max_instances: Some(1),
                ..RoleRestriction::default()
            },
        )]),
        ..ProjectConfig::default()
    };
    let mut overrides = OperatorOverrides {
        roles: BTreeMap::from([(
            "worker".into(),
            RoleRestriction {
                max_instances: Some(3),
                ..RoleRestriction::default()
            },
        )]),
        ..OperatorOverrides::default()
    };
    let resolved =
        resolve(&GlobalConfig::default(), &project, &overrides).unwrap();
    assert_eq!(resolved.roles["worker"].max_instances, Some(3));
    overrides.roles.get_mut("worker").unwrap().max_instances = Some(4);
    assert!(resolve(&GlobalConfig::default(), &project, &overrides).is_err());
}

#[test]
fn configuration_locations_use_absolute_xdg_then_home_without_creating_files() {
    let root = Path::new("/project");
    let locations = ConfigLocations::from_environment_values(
        root,
        Some("/xdg".into()),
        Some("/home/operator".into()),
    );
    assert_eq!(locations.global, Some("/xdg/coterie/config.toml".into()));
    assert_eq!(locations.project, Path::new("/project/coterie.toml"));
    for xdg in [None, Some("relative".into()), Some("".into())] {
        let locations = ConfigLocations::from_environment_values(
            root,
            xdg,
            Some("/home/operator".into()),
        );
        assert_eq!(
            locations.global,
            Some("/home/operator/.config/coterie/config.toml".into())
        );
    }
    assert_eq!(
        ConfigLocations::from_environment_values(
            root,
            None,
            Some("relative".into())
        )
        .global,
        None
    );
    assert_eq!(
        ConfigLocations::from_environment_values(root, None, None).global,
        None
    );
}

#[test]
fn missing_files_are_optional_but_existing_unreadable_files_are_errors() {
    let fixture = Fixture::new();
    assert_eq!(fixture.load().unwrap().archetype, builtin_standard());
    assert_eq!(fs::read_dir(&fixture.0).unwrap().count(), 0);
    fs::create_dir(fixture.0.join("config.toml")).unwrap();
    assert!(matches!(fixture.load(), Err(ConfigError::Io { .. })));
    fs::remove_dir(fixture.0.join("config.toml")).unwrap();
    symlink("missing", fixture.0.join("coterie.toml")).unwrap();
    assert!(matches!(fixture.load(), Err(ConfigError::Io { .. })));
}

#[test]
fn project_discovery_reads_only_the_real_project_root_config() {
    let fixture = Fixture::new();
    git2::Repository::init(&fixture.0).unwrap();
    fixture.write("coterie.toml", "[roles.worker]\nmax_instances = 2");
    fixture.write("nested/coterie.toml", "unknown = true");
    let project =
        crate::project::DiscoveredProject::discover(fixture.0.join("nested"))
            .unwrap();
    let locations = ConfigLocations::from_environment_values(
        &project.canonical_path,
        None,
        None,
    );
    assert_eq!(
        load(&locations, &OperatorOverrides::default())
            .unwrap()
            .roles["worker"]
            .max_instances,
        Some(2)
    );
}

#[test]
fn includes_merge_tables_in_order_and_main_file_replaces_arrays_and_scalars() {
    let fixture = Fixture::new();
    fixture.write("parts/first.toml", &format!("{CUSTOM}\n[providers.codex]\ncommand = ['first', '--old']\n[limits]\nmax_agents_per_run = 30\nmax_concurrent_agents = 4"));
    fixture.write("parts/second.toml", "[providers.codex]\ncommand = ['second']\n[archetypes.'global:custom@1'.roles.builder]\nmax_instances = 4\ninstructions = 'From include'\n[limits]\nmax_concurrent_agents = 6");
    fixture.write("config.toml", "includes = ['parts/first.toml', 'parts/second.toml']\n[providers.codex]\ncommand = ['main', 'literal $(touch sentinel)', '`literal`']\n[archetypes.'global:custom@1'.roles.builder]\ncapabilities = []\nmax_instances = 3\n[limits]\nmax_spawns_per_minute = 9");
    let effective = fixture.load().unwrap();
    assert_eq!(
        effective.providers["codex"].command,
        ["main", "literal $(touch sentinel)", "`literal`"]
    );
    assert_eq!(
        effective.limits,
        RunLimits {
            max_concurrent_agents: 6,
            max_agents_per_run: 30,
            max_spawns_per_minute: 9
        }
    );
    let builder = effective.archetype.role("builder").unwrap();
    assert_eq!(builder.instructions.as_deref(), Some("From include"));
    assert!(builder.capabilities.is_empty());
    assert_eq!(builder.max_instances, Some(3));
    assert!(!fixture.0.join("sentinel").exists());
}

#[test]
fn every_archetype_selection_layer_has_the_documented_precedence() {
    let global: GlobalConfig = toml::from_str(CUSTOM).unwrap();
    assert_eq!(
        resolve(
            &global,
            &ProjectConfig::default(),
            &OperatorOverrides::default()
        )
        .unwrap()
        .archetype
        .lead,
        "coordinator"
    );
    let project = ProjectConfig {
        archetype: Some("builtin:standard@1".into()),
        ..ProjectConfig::default()
    };
    assert_eq!(
        resolve(&global, &project, &OperatorOverrides::default())
            .unwrap()
            .archetype,
        builtin_standard()
    );
    let operator = OperatorOverrides {
        archetype: Some("global:custom@1".into()),
        ..OperatorOverrides::default()
    };
    assert_eq!(
        resolve(&global, &project, &operator)
            .unwrap()
            .archetype
            .lead,
        "coordinator"
    );
}

#[test]
fn final_selection_determines_which_roles_can_be_restricted() {
    let global: GlobalConfig = toml::from_str(CUSTOM).unwrap();
    let project: ProjectConfig =
        toml::from_str("[roles.worker]\nmax_instances = 2").unwrap();
    assert!(resolve(&global, &project, &OperatorOverrides::default()).is_err());
    let operator = OperatorOverrides {
        archetype: Some("builtin:standard@1".into()),
        ..OperatorOverrides::default()
    };
    assert_eq!(
        resolve(&global, &project, &operator).unwrap().roles["worker"]
            .max_instances,
        Some(2)
    );
}

#[test]
fn global_profiles_cannot_shadow_sealed_builtin_profiles() {
    let fixture = Fixture::new();
    fixture.write("config.toml", "[permission_profiles.worker]\nfilesystem = 'project-write'\nnetwork = 'provider-default'\napprovals = 'interactive'");
    assert_eq!(fixture.load().unwrap().archetype, builtin_standard());
    fixture.write(
        "coterie.toml",
        "[roles.worker]\npermission_profile = 'worker'",
    );
    assert!(fixture.load().is_err());
}

#[test]
fn project_restrictions_and_operator_restoration_preserve_the_baseline() {
    let fixture = Fixture::new();
    fixture.write("config.toml", "[permission_profiles.strict]\nfilesystem = 'read-only'\nnetwork = 'deny'\napprovals = 'never'\n[permission_profiles.original]\nfilesystem = 'workspace-write'\nnetwork = 'deny'\napprovals = 'never'");
    fixture.write("coterie.toml", "[roles.worker]\nenabled = false\nmax_instances = 1\npermission_profile = 'strict'\n[limits]\nmax_concurrent_agents = 2");
    let restricted = fixture.load().unwrap();
    assert!(!restricted.roles["worker"].enabled);
    assert_eq!(
        restricted.roles["worker"].permission_profile.filesystem,
        FilesystemPolicy::ReadOnly
    );
    assert_eq!(restricted.limits.max_concurrent_agents, 2);
    let operator = OperatorOverrides {
        roles: BTreeMap::from([(
            "worker".into(),
            RoleRestriction {
                enabled: Some(true),
                max_instances: Some(3),
                permission_profile: Some("original".into()),
            },
        )]),
        limits: LimitOverrides {
            max_concurrent_agents: Some(8),
            ..LimitOverrides::default()
        },
        ..OperatorOverrides::default()
    };
    let restored = load(&fixture.locations(), &operator).unwrap();
    assert!(restored.roles["worker"].enabled);
    assert_eq!(
        restored.roles["worker"].permission_profile,
        builtin_standard().permission_profiles["worker"]
    );
    assert_eq!(restored.limits, compiled_defaults().limits);
    assert_eq!(restored.archetype, builtin_standard());
}

#[test]
fn invalid_project_requests_cannot_be_hidden_by_operator_overrides() {
    let global = GlobalConfig::default();
    let operator = OperatorOverrides {
        archetype: Some("builtin:standard@1".into()),
        roles: BTreeMap::from([(
            "worker".into(),
            RoleRestriction {
                max_instances: Some(2),
                ..RoleRestriction::default()
            },
        )]),
        ..OperatorOverrides::default()
    };
    for text in [
        "archetype = 'builtin:missing@1'",
        "[roles.worker]\nmax_instances = 4",
        "[roles.lead]\nenabled = false",
        "[limits]\nmax_agents_per_run = 17",
    ] {
        let project: ProjectConfig = toml::from_str(text).unwrap();
        assert!(
            matches!(
                resolve(&global, &project, &operator),
                Err(ConfigError::Invalid {
                    layer: ConfigLayer::Project,
                    ..
                })
            ),
            "{text}"
        );
    }
}

#[test]
fn parsing_rejects_unknown_or_prohibited_fields_before_merging() {
    let fixture = Fixture::new();
    for text in [
        "unknown = true",
        "schema_version = 2",
        "schema_version = -1",
        "schema_version = '1'",
        "[limits]\nmax_agents_per_run = 65536",
        "[limits]\nmax_agents_per_run = -1",
        "[roles.worker]\nmax_instances = 'two'",
        "[roles.worker]\nprovider = 'evil'",
        "[roles.worker]\ninstructions = 'do evil'",
        "[roles.worker]\ncapabilities = ['task:*']",
        "[roles.worker]\nworkspace = 'project'",
        "includes = ['global.toml']",
        "[providers.evil]\ncommand = ['evil']",
        "[permission_profiles.evil]\nfilesystem = 'project-write'",
        "hooks = []",
        "paths = ['/']",
        "environment = ['SECRET']",
        "[supervision]\nmax_launch_attempts = 99",
    ] {
        fixture.write("coterie.toml", text);
        assert!(
            matches!(fixture.load(), Err(ConfigError::Parse { path, .. }) if path == fixture.0.join("coterie.toml")),
            "{text}"
        );
    }
    fs::remove_file(fixture.0.join("coterie.toml")).unwrap();
    fixture.write(
        "config.toml",
        "includes = ['part.toml']\n[providers.codex]\ncommand = ['codex']",
    );
    for text in [
        "schema_version = 2",
        "unknown = true",
        "[providers.codex]\ncommand = 'ignored?'",
        "[providers.codex]\nunknown = 1",
    ] {
        fixture.write("part.toml", text);
        assert!(
            matches!(fixture.load(), Err(ConfigError::Parse { path, .. }) if path == fixture.0.join("part.toml")),
            "{text}"
        );
    }
}

#[test]
fn includes_reject_missing_nested_repeated_and_cyclic_files() {
    let fixture = Fixture::new();
    fixture.write("config.toml", "includes = ['missing.toml']");
    assert!(matches!(fixture.load(), Err(ConfigError::Io { .. })));
    for includes in ["['config.toml']", "['alias.toml']"] {
        fixture.write("config.toml", &format!("includes = {includes}"));
        if !fixture.0.join("alias.toml").exists() {
            symlink("config.toml", fixture.0.join("alias.toml")).unwrap();
        }
        assert!(fixture.load().unwrap_err().to_string().contains("cyclic"));
    }
    fixture.write("config.toml", "includes = ['part.toml']");
    for nested in [
        "includes = []",
        "includes = ['config.toml']",
        "includes = ['other.toml']",
    ] {
        fixture.write("part.toml", nested);
        assert!(fixture.load().unwrap_err().to_string().contains("nested"));
    }
    fixture.write("part.toml", "");
    fixture.write("config.toml", "includes = ['part.toml', 'part.toml']");
    assert!(
        fixture
            .load()
            .unwrap_err()
            .to_string()
            .contains("duplicate")
    );
}

#[test]
fn version_one_is_optional_in_every_file() {
    let fixture = Fixture::new();
    fixture.write(
        "config.toml",
        "schema_version = 1\nincludes = ['part.toml']",
    );
    fixture.write("part.toml", "schema_version = 1");
    fixture.write("coterie.toml", "schema_version = 1");
    assert_eq!(fixture.load().unwrap().archetype, builtin_standard());
}

#[test]
fn trusted_definitions_are_validated_even_when_not_selected() {
    let fixture = Fixture::new();
    fixture.write("coterie.toml", "archetype = 'builtin:standard@1'");
    for text in [
        "[archetypes.'builtin:standard@1']",
        "[archetypes.'global:bad@0']",
        "[archetypes.'global:bad@01']",
        "[archetypes.'global:bad@1']\nlead = 'missing'",
        "[providers.other]",
        "[providers.codex]\ncommand = []",
        "[providers.codex]\ncommand = ['']",
        "[permission_profiles.missing]\nfilesystem = 'read-only'",
    ] {
        fixture.write("config.toml", text);
        assert!(fixture.load().is_err(), "{text}");
    }
    for (from, to) in [
        ("provider = \"codex\"", "provider = \"absent\""),
        (
            "permission_profile = \"safe\"",
            "permission_profile = \"absent\"",
        ),
        ("mode = \"interactive\"", "mode = \"job\""),
        ("spawn:builder", "spawn:b*"),
        ("spawn:builder", "unknown:builder"),
        ("spawn:builder", "spawn:builder:extra"),
    ] {
        fixture.write("config.toml", &CUSTOM.replace(from, to));
        assert!(fixture.load().is_err(), "{from} -> {to}");
    }
}

#[test]
fn errors_retain_the_file_that_supplied_the_invalid_field() {
    let fixture = Fixture::new();
    fixture.write("parts/first.toml", "[limits]\nmax_agents_per_run = 0");
    fixture.write(
        "config.toml",
        "includes = ['parts/first.toml']\n[limits]\nmax_concurrent_agents = 2",
    );
    assert!(
        matches!(fixture.load(), Err(ConfigError::Invalid { path: Some(path), field, .. }) if path == fixture.0.join("parts/first.toml") && field == "limits.max_agents_per_run")
    );
}

#[test]
fn every_file_rejects_unknown_fields_and_unsupported_versions() {
    let fixture = Fixture::new();
    let invalid_global_inputs = [
        "schema_version = 0",
        "schema_version = 2",
        "unknown = true",
        "[providers.codex]\nunknown = true",
        "[limits]\nunknown = true",
        "[supervision]\nunknown = true",
        "[permission_profiles.safe]\nunknown = true",
        "[archetypes.'global:custom@1']\nunknown = true",
        "[archetypes.'global:custom@1'.roles.builder]\nunknown = true",
    ];
    for name in ["config.toml", "part.toml"] {
        for invalid in invalid_global_inputs {
            fixture.write(
                "config.toml",
                &format!("includes = ['part.toml']\n{CUSTOM}"),
            );
            fixture.write("part.toml", "");
            let expected = fixture.write(name, invalid);
            let error = fixture.load().unwrap_err();
            assert!(
                matches!(error, ConfigError::Parse { path, .. } if path == expected),
                "{name}: {invalid}"
            );
        }
    }
    fixture.write("config.toml", "");
    for invalid in [
        "schema_version = 0",
        "schema_version = 2",
        "unknown = true",
        "[limits]\nunknown = true",
        "[roles.worker]\nunknown = true",
    ] {
        let expected = fixture.write("coterie.toml", invalid);
        assert!(
            matches!(fixture.load(), Err(ConfigError::Parse { path, .. }) if path == expected),
            "{invalid}"
        );
    }
}

#[test]
fn includes_reject_canonical_alias_duplicates_and_cycles() {
    let fixture = Fixture::new();
    fixture.write("parts/part.toml", "");
    symlink("parts/part.toml", fixture.0.join("alias.toml")).unwrap();
    for includes in [
        "['parts/part.toml', 'alias.toml']",
        "['parts/part.toml', 'parts/../parts/part.toml']",
    ] {
        fixture.write("config.toml", &format!("includes = {includes}"));
        assert!(
            matches!(fixture.load(), Err(ConfigError::Parse { message, .. }) if message.contains("duplicate or cyclic"))
        );
    }
    fixture.write("config.toml", "includes = ['parts/part.toml']");
    fixture.write("parts/part.toml", "includes = ['second.toml']");
    fixture.write("parts/second.toml", "includes = ['part.toml']");
    assert!(
        matches!(fixture.load(), Err(ConfigError::Parse { path, message }) if path == fixture.0.join("parts/part.toml") && message.contains("nested includes"))
    );
}

#[test]
fn missing_trusted_definitions_and_builtin_shadowing_identify_the_source() {
    let fixture = Fixture::new();
    fixture.write("coterie.toml", "archetype = 'builtin:standard@1'");
    let operator = OperatorOverrides {
        archetype: Some("builtin:standard@1".into()),
        ..OperatorOverrides::default()
    };
    fixture.write("config.toml", "includes = ['parts/definitions.toml']");
    for (text, field, reason) in [
        (
            "[archetypes.'builtin:standard@1']".into(),
            "archetypes.builtin:standard@1",
            "reserved",
        ),
        (
            "[archetypes.'builtin:other@1']".into(),
            "archetypes.builtin:other@1",
            "reserved",
        ),
        (
            "archetype = 'global:missing@1'".into(),
            "archetype",
            "unknown archetype",
        ),
        (
            CUSTOM.replace("provider = \"codex\"", "provider = \"absent\""),
            "archetypes.global:custom@1.roles.builder.provider",
            "missing provider",
        ),
        (
            CUSTOM.replace(
                "permission_profile = \"safe\"",
                "permission_profile = \"absent\"",
            ),
            "archetypes.global:custom@1.roles.builder.permission_profile",
            "missing trusted permission profile",
        ),
        (
            CUSTOM.replace("lead = \"coordinator\"", "lead = \"absent\""),
            "archetypes.global:custom@1.lead",
            "must exist",
        ),
    ] {
        let expected = fixture.write("parts/definitions.toml", &text);
        let error = load(&fixture.locations(), &operator).unwrap_err();
        assert!(
            matches!(error, ConfigError::Invalid { layer: ConfigLayer::Global, path: Some(path), field: actual, reason: message } if path == expected && actual == field && message.contains(reason)),
            "{text}"
        );
    }
    fixture.write("parts/definitions.toml", CUSTOM);
    for (text, field) in [
        ("archetype = 'global:missing@1'", "archetype"),
        ("[roles.missing]", "roles.missing"),
        (
            "[roles.worker]\npermission_profile = 'missing'",
            "roles.worker.permission_profile",
        ),
        (
            "[roles.worker]\npermission_profile = 'worker'",
            "roles.worker.permission_profile",
        ),
    ] {
        let expected = fixture.write("coterie.toml", text);
        assert!(
            matches!(load(&fixture.locations(), &operator), Err(ConfigError::Invalid { layer: ConfigLayer::Project, path: Some(path), field: actual, .. }) if path == expected && actual == field),
            "{text}"
        );
    }
    fixture.write("coterie.toml", "archetype = 'builtin:standard@1'");
    for (operator, expected) in [
        (
            OperatorOverrides {
                archetype: Some("global:missing@1".into()),
                ..OperatorOverrides::default()
            },
            "archetype",
        ),
        (
            OperatorOverrides {
                roles: BTreeMap::from([(
                    "missing".into(),
                    RoleRestriction::default(),
                )]),
                ..OperatorOverrides::default()
            },
            "roles.missing",
        ),
        (
            OperatorOverrides {
                roles: BTreeMap::from([(
                    "worker".into(),
                    RoleRestriction {
                        permission_profile: Some("missing".into()),
                        ..RoleRestriction::default()
                    },
                )]),
                ..OperatorOverrides::default()
            },
            "roles.worker.permission_profile",
        ),
    ] {
        assert!(
            matches!(load(&fixture.locations(), &operator), Err(ConfigError::Invalid { layer: ConfigLayer::Operator, path: None, field, .. }) if field == expected),
            "{expected}"
        );
    }
}

#[test]
fn numeric_limits_are_checked_at_boundaries_for_each_layer() {
    for field in [
        "max_concurrent_agents",
        "max_agents_per_run",
        "max_spawns_per_minute",
    ] {
        for ceiling in [1, 2, 8, 16, u16::MAX] {
            let global: GlobalConfig =
                toml::from_str(&format!("[limits]\n{field} = {ceiling}"))
                    .unwrap();
            for value in [0, 1, ceiling, ceiling.saturating_add(1), u16::MAX] {
                let project: ProjectConfig =
                    toml::from_str(&format!("[limits]\n{field} = {value}"))
                        .unwrap();
                let expected = value > 0 && value <= ceiling;
                assert_eq!(
                    resolve(&global, &project, &OperatorOverrides::default())
                        .is_ok(),
                    expected,
                    "{field}: {value} <= {ceiling}"
                );
                let operator = OperatorOverrides {
                    limits: project.limits,
                    ..OperatorOverrides::default()
                };
                assert_eq!(
                    resolve(&global, &ProjectConfig::default(), &operator)
                        .is_ok(),
                    expected
                );
            }
        }
    }
    for capacity in 0..=4 {
        let project: ProjectConfig = toml::from_str(&format!(
            "[roles.worker]\nmax_instances = {capacity}"
        ))
        .unwrap();
        assert_eq!(
            resolve(
                &GlobalConfig::default(),
                &project,
                &OperatorOverrides::default()
            )
            .is_ok(),
            capacity <= 3
        );
    }
}

#[test]
fn all_permission_combinations_obey_the_partial_order() {
    let mut profiles = Vec::new();
    for filesystem in [
        FilesystemPolicy::ReadOnly,
        FilesystemPolicy::WorkspaceWrite,
        FilesystemPolicy::ProjectWrite,
    ] {
        for network in [NetworkPolicy::Deny, NetworkPolicy::ProviderDefault] {
            for approvals in
                [ApprovalPolicy::Never, ApprovalPolicy::Interactive]
            {
                profiles.push(PermissionProfile {
                    filesystem,
                    network,
                    approvals,
                });
            }
        }
    }
    for a in &profiles {
        assert!(a.no_more_permissive_than(*a));
        for b in &profiles {
            if a.no_more_permissive_than(*b) && b.no_more_permissive_than(*a) {
                assert_eq!(a, b);
            }
            for c in &profiles {
                if a.no_more_permissive_than(*b)
                    && b.no_more_permissive_than(*c)
                {
                    assert!(a.no_more_permissive_than(*c));
                }
            }
            let expected = matches!(
                (a.filesystem, b.filesystem),
                (FilesystemPolicy::ReadOnly, _)
                    | (
                        FilesystemPolicy::ProjectWrite,
                        FilesystemPolicy::ProjectWrite,
                    )
                    | (
                        FilesystemPolicy::WorkspaceWrite,
                        FilesystemPolicy::WorkspaceWrite,
                    )
            ) && !(a.network == NetworkPolicy::ProviderDefault
                && b.network == NetworkPolicy::Deny)
                && !(a.approvals == ApprovalPolicy::Interactive
                    && b.approvals == ApprovalPolicy::Never);
            assert_eq!(a.no_more_permissive_than(*b), expected);
            let mut global: GlobalConfig = toml::from_str(CUSTOM).unwrap();
            for (name, value) in [("safe", b), ("candidate", a)] {
                global.permission_profiles.insert(
                    name.into(),
                    ProfileInput {
                        filesystem: Some(value.filesystem),
                        network: Some(value.network),
                        approvals: Some(value.approvals),
                    },
                );
            }
            let project: ProjectConfig = toml::from_str(
                "[roles.builder]\npermission_profile = 'candidate'",
            )
            .unwrap();
            let result =
                resolve(&global, &project, &OperatorOverrides::default());
            assert_eq!(result.is_ok(), expected, "{a:?} <= {b:?}");
            if let Ok(effective) = result {
                assert_eq!(effective.roles["builder"].permission_profile, *a);
                assert_eq!(effective.archetype.permission_profiles["safe"], *b);
            }
        }
    }
}

#[test]
fn supervision_is_trusted_only_and_requires_consistent_positive_bounds() {
    let fixture = Fixture::new();
    fixture.write(
        "config.toml",
        "[supervision]\njob_timeout_seconds = 120\nmax_launch_attempts = 2",
    );
    let policy = fixture.load().unwrap().supervision;
    assert_eq!(policy.job_timeout_seconds, 120);
    assert_eq!(policy.max_launch_attempts, 2);
    assert_eq!(
        policy.startup_timeout_seconds,
        compiled_defaults().supervision.startup_timeout_seconds
    );
    for field in [
        "restart_window_seconds",
        "max_launch_attempts",
        "restart_backoff_seconds",
        "startup_timeout_seconds",
        "job_timeout_seconds",
        "interrupt_grace_ms",
        "shutdown_timeout_ms",
    ] {
        for value in [0, -1, i64::MAX] {
            fixture.write(
                "config.toml",
                &format!("[supervision]\n{field} = {value}"),
            );
            assert!(fixture.load().is_err(), "{field} = {value}");
        }
    }
    fixture.write("config.toml", "[supervision]\ninterrupt_grace_ms = 5000");
    assert!(fixture.load().is_err());
}

fn generated_schemas() -> [(PathBuf, String); 2] {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("schemas");
    [
        (
            root.join("config-global-v1.schema.json"),
            schemars::schema_for!(GlobalConfig),
        ),
        (
            root.join("config-project-v1.schema.json"),
            schemars::schema_for!(ProjectConfig),
        ),
    ]
    .map(|(path, schema)| {
        (
            path,
            format!("{}\n", serde_json::to_string_pretty(&schema).unwrap()),
        )
    })
}

#[test]
fn generated_configuration_schemas_match_golden_contracts() {
    for (path, generated) in generated_schemas() {
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            generated,
            "schema mismatch: {}",
            path.display()
        );
    }
}

#[test]
fn documented_examples_resolve_together() {
    let fixture = Fixture::new();
    fixture.write(
        "config.toml",
        include_str!("../../examples/config/global.toml"),
    );
    fixture.write(
        "coterie.toml",
        include_str!("../../examples/config/project.toml"),
    );
    let effective = fixture.load().unwrap();
    assert_eq!(effective.archetype.reference, "global:pair@1");
    assert_eq!(effective.archetype.lead, "coordinator");
    assert_eq!(effective.roles["builder"].max_instances, Some(2));
    assert_eq!(effective.limits.max_concurrent_agents, 3);
    assert_eq!(effective.supervision.job_timeout_seconds, 1800);
}

#[test]
fn pure_resolution_rejects_unexpanded_include_directives() {
    let global = GlobalConfig {
        includes: Some(vec!["policy.toml".into()]),
        ..GlobalConfig::default()
    };
    assert!(
        resolve(
            &global,
            &ProjectConfig::default(),
            &OperatorOverrides::default()
        )
        .is_err()
    );
}

#[test]
fn syntax_errors_identify_fields_without_echoing_document_lines() {
    let fixture = Fixture::new();
    fixture.write(
        "coterie.toml",
        "[roles.worker]\nmax_instances = 'invalid' # private-comment-marker\n",
    );
    let error = fixture.load().unwrap_err().to_string();
    assert!(error.contains("roles.worker.max_instances"), "{error}");
    assert!(!error.contains("private-comment-marker"));
}

#[test]
fn symlinked_global_files_keep_includes_relative_to_the_opened_path() {
    let fixture = Fixture::new();
    fixture.write("store/actual.toml", "includes = ['part.toml']");
    symlink("store/actual.toml", fixture.0.join("config.toml")).unwrap();
    fixture.write("part.toml", "[limits]\nmax_concurrent_agents = 4");
    assert_eq!(fixture.load().unwrap().limits.max_concurrent_agents, 4);
    let absolute =
        fixture.write("absolute.toml", "[limits]\nmax_concurrent_agents = 5");
    fixture.write(
        "store/actual.toml",
        &format!("includes = ['{}']", absolute.display()),
    );
    assert_eq!(fixture.load().unwrap().limits.max_concurrent_agents, 5);
}

#[test]
#[ignore = "explicit developer command to regenerate configuration schemas"]
fn regenerate_configuration_schemas() {
    for (path, generated) in generated_schemas() {
        fs::write(path, generated).unwrap();
    }
}

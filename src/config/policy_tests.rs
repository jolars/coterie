use super::resolution_tests::{CUSTOM, Fixture};
use super::resolver::EffectiveRole;
use super::*;

fn profiles() -> Vec<PermissionProfile> {
    let mut profiles = Vec::new();
    for filesystem in [
        FilesystemPolicy::ReadOnly,
        FilesystemPolicy::ProjectWrite,
        FilesystemPolicy::WorkspaceWrite,
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
    profiles
}

// Independent authority sets catch accidental total ordering of write scopes.
fn authority(profile: PermissionProfile) -> u8 {
    let filesystem = match profile.filesystem {
        FilesystemPolicy::ReadOnly => 0,
        FilesystemPolicy::ProjectWrite => 1,
        FilesystemPolicy::WorkspaceWrite => 2,
    };
    filesystem
        | if profile.network == NetworkPolicy::ProviderDefault {
            4
        } else {
            0
        }
        | if profile.approvals == ApprovalPolicy::Interactive {
            8
        } else {
            0
        }
}

fn capacity(value: Option<u16>) -> u32 {
    value.map_or(u32::MAX, u32::from)
}

#[test]
fn permission_intersection_is_the_greatest_common_restriction() {
    let profiles = profiles();
    for &a in &profiles {
        assert_eq!(a.intersect(a), a);
        for &b in &profiles {
            let common = a.intersect(b);
            assert_eq!(authority(common), authority(a) & authority(b));
            assert_eq!(common, b.intersect(a));
            for &c in &profiles {
                assert_eq!(
                    a.intersect(b).intersect(c),
                    a.intersect(b.intersect(c))
                );
                if authority(c) & !authority(a) == 0
                    && authority(c) & !authority(b) == 0
                {
                    assert_eq!(authority(c) & !authority(common), 0);
                }
            }
        }
    }
}

#[test]
fn role_intersection_preserves_disabled_roles_and_unbounded_capacity() {
    let mut roles = Vec::new();
    for enabled in [false, true] {
        for max_instances in [None, Some(0), Some(1), Some(u16::MAX)] {
            for permission_profile in profiles() {
                roles.push(EffectiveRole {
                    enabled,
                    max_instances,
                    permission_profile,
                });
            }
        }
    }
    for a in &roles {
        assert_eq!(a.intersect(a), *a);
        for b in &roles {
            let common = a.intersect(b);
            assert_eq!(common, b.intersect(a));
            assert_eq!(common.enabled, a.enabled && b.enabled);
            assert_eq!(
                capacity(common.max_instances),
                capacity(a.max_instances).min(capacity(b.max_instances))
            );
            assert_eq!(
                authority(common.permission_profile),
                authority(a.permission_profile)
                    & authority(b.permission_profile)
            );
            for c in &roles {
                assert_eq!(common.intersect(c), a.intersect(&b.intersect(c)));
            }
        }
    }
}

#[test]
fn run_limit_intersection_obeys_lattice_laws_at_numeric_boundaries() {
    let mut limits = Vec::new();
    for max_concurrent_agents in [1, 8, u16::MAX] {
        for max_agents_per_run in [1, 16, u16::MAX] {
            for max_spawns_per_minute in [1, 8, u16::MAX] {
                limits.push(RunLimits {
                    max_concurrent_agents,
                    max_agents_per_run,
                    max_spawns_per_minute,
                });
            }
        }
    }
    for &a in &limits {
        assert_eq!(a.intersect(a), a);
        for &b in &limits {
            let common = a.intersect(b);
            assert_eq!(common, b.intersect(a));
            assert_eq!(
                common.max_concurrent_agents,
                a.max_concurrent_agents.min(b.max_concurrent_agents)
            );
            assert_eq!(
                common.max_agents_per_run,
                a.max_agents_per_run.min(b.max_agents_per_run)
            );
            assert_eq!(
                common.max_spawns_per_minute,
                a.max_spawns_per_minute.min(b.max_spawns_per_minute)
            );
            for &c in &limits {
                assert_eq!(common.intersect(c), a.intersect(b.intersect(c)));
            }
        }
    }
}

fn with_profiles() -> GlobalConfig {
    let mut global: GlobalConfig = toml::from_str(CUSTOM).unwrap();
    for (index, profile) in profiles().into_iter().enumerate() {
        global.permission_profiles.insert(
            format!("profile-{index}"),
            ProfileInput {
                filesystem: Some(profile.filesystem),
                network: Some(profile.network),
                approvals: Some(profile.approvals),
            },
        );
    }
    global
}

fn assert_bounded(effective: &EffectiveConfig, trusted: &EffectiveConfig) {
    assert_eq!(effective.archetype, trusted.archetype);
    assert_eq!(effective.providers, trusted.providers);
    assert_eq!(effective.supervision, trusted.supervision);
    assert_eq!(
        effective.roles.keys().collect::<Vec<_>>(),
        trusted.roles.keys().collect::<Vec<_>>()
    );
    assert!(
        effective.limits.max_concurrent_agents
            <= trusted.limits.max_concurrent_agents
    );
    assert!(
        effective.limits.max_agents_per_run
            <= trusted.limits.max_agents_per_run
    );
    assert!(
        effective.limits.max_spawns_per_minute
            <= trusted.limits.max_spawns_per_minute
    );
    for (name, role) in &effective.roles {
        let ceiling = &trusted.roles[name];
        assert!(!role.enabled || ceiling.enabled);
        assert!(
            capacity(role.max_instances) <= capacity(ceiling.max_instances)
        );
        assert_eq!(
            authority(role.permission_profile)
                & !authority(ceiling.permission_profile),
            0
        );
    }
}

#[test]
fn combined_project_restrictions_never_increase_selected_trusted_authority() {
    let mut global = with_profiles();
    for trusted_profile in 0..profiles().len() {
        for trusted_capacity in [None, Some(0), Some(3), Some(u16::MAX)] {
            let builder = global
                .archetypes
                .get_mut("global:custom@1")
                .unwrap()
                .roles
                .get_mut("builder")
                .unwrap();
            builder.permission_profile =
                Some(format!("profile-{trusted_profile}"));
            builder.max_instances = trusted_capacity;
            let trusted = resolve(
                &global,
                &ProjectConfig::default(),
                &OperatorOverrides::default(),
            )
            .unwrap();
            for enabled in [None, Some(false), Some(true)] {
                for max_instances in
                    [None, Some(0), Some(1), Some(3), Some(4), Some(u16::MAX)]
                {
                    for (index, profile) in profiles().into_iter().enumerate() {
                        let project = ProjectConfig {
                            roles: BTreeMap::from([(
                                "builder".into(),
                                RoleRestriction {
                                    enabled,
                                    max_instances,
                                    permission_profile: Some(format!(
                                        "profile-{index}"
                                    )),
                                },
                            )]),
                            limits: LimitOverrides {
                                max_concurrent_agents: Some(1),
                                max_agents_per_run: Some(2),
                                max_spawns_per_minute: Some(3),
                            },
                            ..ProjectConfig::default()
                        };
                        let result = resolve(
                            &global,
                            &project,
                            &OperatorOverrides::default(),
                        );
                        let expected = max_instances.is_none_or(|value| {
                            u32::from(value) <= capacity(trusted_capacity)
                        }) && authority(profile)
                            & !authority(
                                trusted.roles["builder"].permission_profile,
                            )
                            == 0;
                        assert_eq!(
                            result.is_ok(),
                            expected,
                            "{project:?}, {trusted:?}"
                        );
                        if let Ok(effective) = result {
                            assert_bounded(&effective, &trusted);
                            assert_eq!(
                                effective.roles["builder"],
                                EffectiveRole {
                                    enabled: enabled.unwrap_or(true),
                                    max_instances: max_instances
                                        .or(trusted_capacity),
                                    permission_profile: profile,
                                }
                            );
                            assert_eq!(
                                effective.limits,
                                RunLimits {
                                    max_concurrent_agents: 1,
                                    max_agents_per_run: 2,
                                    max_spawns_per_minute: 3,
                                }
                            );
                        } else {
                            assert!(matches!(
                                result,
                                Err(ConfigError::Invalid {
                                    layer: ConfigLayer::Project,
                                    ..
                                })
                            ));
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn project_selection_preserves_trusted_definitions() {
    let global = with_profiles();
    for reference in ["builtin:standard@1", "global:custom@1"] {
        let selection = ProjectConfig {
            archetype: Some(reference.into()),
            ..ProjectConfig::default()
        };
        let trusted =
            resolve(&global, &selection, &OperatorOverrides::default())
                .unwrap();
        for (name, declaration) in &trusted.archetype.roles {
            for enabled in [false, true] {
                let mut project = selection.clone();
                project.roles.insert(
                    name.clone(),
                    RoleRestriction {
                        enabled: Some(enabled),
                        max_instances: Some(0),
                        permission_profile: Some("profile-0".into()),
                    },
                );
                let result =
                    resolve(&global, &project, &OperatorOverrides::default());
                if !enabled && *name == trusted.archetype.lead {
                    assert!(
                        matches!(result, Err(ConfigError::Invalid { layer: ConfigLayer::Project, field, .. }) if field == format!("roles.{name}.enabled"))
                    );
                } else {
                    let effective = result.unwrap();
                    assert_bounded(&effective, &trusted);
                    assert_eq!(effective.archetype.roles[name], *declaration);
                    assert_eq!(effective.roles[name].enabled, enabled);
                    assert_eq!(effective.roles[name].max_instances, Some(0));
                }
            }
        }
    }
}

#[test]
fn project_cannot_inject_authority_through_any_restriction_table() {
    let fixture = Fixture::new();
    fixture.write("config.toml", CUSTOM);
    for field in [
        "command = ['malicious']",
        "instructions = 'malicious'",
        "hooks = ['malicious']",
        "paths = ['/']",
        "allowed_project_roots = ['/']",
        "environment = ['SECRET']",
        "capabilities = ['task:*']",
        "providers.evil.command = ['malicious']",
        "permission_profiles.evil.filesystem = 'project-write'",
        "includes = ['malicious.toml']",
    ] {
        for prefix in [
            "",
            "[limits]\n",
            "[roles.builder]\n",
            "[roles.builder]\nenabled = false\n",
            "[roles.coordinator]\n",
        ] {
            let path =
                fixture.write("coterie.toml", &format!("{prefix}{field}"));
            assert!(
                matches!(fixture.load(), Err(ConfigError::Parse { path: actual, .. }) if actual == path),
                "{prefix}{field}"
            );
        }
    }
    for profile in ["/tmp/profile", "${PROFILE}", "$(profile)", "missing"] {
        fixture.write(
            "coterie.toml",
            &format!("[roles.builder]\npermission_profile = '{profile}'"),
        );
        assert!(
            matches!(fixture.load(), Err(ConfigError::Invalid { layer: ConfigLayer::Project, field, .. }) if field == "roles.builder.permission_profile")
        );
    }
}

#[test]
fn combined_run_limits_are_bounded_and_invalid_projects_cannot_be_masked() {
    for ceiling in [1, 8, u16::MAX] {
        let global = GlobalConfig {
            limits: LimitOverrides {
                max_concurrent_agents: Some(ceiling),
                max_agents_per_run: Some(ceiling),
                max_spawns_per_minute: Some(ceiling),
            },
            ..GlobalConfig::default()
        };
        let trusted = resolve(
            &global,
            &ProjectConfig::default(),
            &OperatorOverrides::default(),
        )
        .unwrap();
        let operator = OperatorOverrides {
            limits: global.limits,
            ..OperatorOverrides::default()
        };
        let values = [
            None,
            Some(0),
            Some(1),
            Some(ceiling),
            Some(ceiling.saturating_add(1)),
            Some(u16::MAX),
        ];
        for max_concurrent_agents in values {
            for max_agents_per_run in values {
                for max_spawns_per_minute in values {
                    let project = ProjectConfig {
                        limits: LimitOverrides {
                            max_concurrent_agents,
                            max_agents_per_run,
                            max_spawns_per_minute,
                        },
                        ..ProjectConfig::default()
                    };
                    let expected = [
                        max_concurrent_agents,
                        max_agents_per_run,
                        max_spawns_per_minute,
                    ]
                    .into_iter()
                    .flatten()
                    .all(|value| value > 0 && value <= ceiling);
                    let result = resolve(
                        &global,
                        &project,
                        &OperatorOverrides::default(),
                    );
                    assert_eq!(result.is_ok(), expected);
                    let restored = resolve(&global, &project, &operator);
                    if let Ok(effective) = result {
                        assert_bounded(&effective, &trusted);
                        assert_eq!(
                            effective.limits,
                            RunLimits {
                                max_concurrent_agents: max_concurrent_agents
                                    .unwrap_or(ceiling),
                                max_agents_per_run: max_agents_per_run
                                    .unwrap_or(ceiling),
                                max_spawns_per_minute: max_spawns_per_minute
                                    .unwrap_or(ceiling),
                            }
                        );
                        assert_eq!(restored.unwrap().limits, trusted.limits);
                    } else {
                        assert!(matches!(
                            restored,
                            Err(ConfigError::Invalid {
                                layer: ConfigLayer::Project,
                                ..
                            })
                        ));
                    }
                }
            }
        }
    }
}

#[test]
fn operator_profiles_are_bounded_and_omitted_role_fields_stay_restricted() {
    let mut global = with_profiles();
    let project: ProjectConfig = toml::from_str("[roles.builder]\nenabled = false\nmax_instances = 0\npermission_profile = 'profile-0'").unwrap();
    for (trusted_index, trusted_profile) in profiles().into_iter().enumerate() {
        global
            .archetypes
            .get_mut("global:custom@1")
            .unwrap()
            .roles
            .get_mut("builder")
            .unwrap()
            .permission_profile = Some(format!("profile-{trusted_index}"));
        let trusted = resolve(
            &global,
            &ProjectConfig::default(),
            &OperatorOverrides::default(),
        )
        .unwrap();
        for (index, profile) in profiles().into_iter().enumerate() {
            let operator = OperatorOverrides {
                roles: BTreeMap::from([(
                    "builder".into(),
                    RoleRestriction {
                        permission_profile: Some(format!("profile-{index}")),
                        ..RoleRestriction::default()
                    },
                )]),
                ..OperatorOverrides::default()
            };
            let result = resolve(&global, &project, &operator);
            let expected =
                authority(profile) & !authority(trusted_profile) == 0;
            assert_eq!(result.is_ok(), expected);
            if let Ok(effective) = result {
                assert_bounded(&effective, &trusted);
                assert_eq!(
                    effective.roles["builder"].permission_profile,
                    profile
                );
                assert!(!effective.roles["builder"].enabled);
                assert_eq!(effective.roles["builder"].max_instances, Some(0));
            } else {
                assert!(
                    matches!(result, Err(ConfigError::Invalid { layer: ConfigLayer::Operator, field, .. }) if field == "roles.builder.permission_profile")
                );
            }
            let mut invalid_project = project.clone();
            invalid_project
                .roles
                .get_mut("builder")
                .unwrap()
                .permission_profile = Some(format!("profile-{index}"));
            let restore = OperatorOverrides {
                roles: project.roles.clone(),
                ..OperatorOverrides::default()
            };
            assert_eq!(
                resolve(&global, &invalid_project, &restore).is_ok(),
                expected
            );
        }
    }
}

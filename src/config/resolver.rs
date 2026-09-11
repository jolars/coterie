//! Pure resolution keeps untrusted restrictions separate from trusted definitions.

use std::collections::BTreeMap;
use std::path::PathBuf;

use thiserror::Error;

use super::*;

#[derive(
    Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize,
)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum ConfigLayer {
    Compiled,
    Builtin,
    Global,
    Project,
    Operator,
}

impl std::fmt::Display for ConfigLayer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Compiled => "compiled",
            Self::Builtin => "builtin",
            Self::Global => "global",
            Self::Project => "project",
            Self::Operator => "operator",
        })
    }
}

fn source_suffix(path: &Option<PathBuf>) -> String {
    path.as_ref()
        .map_or_else(String::new, |path| format!(" in {}", path.display()))
}

#[derive(Debug, Error)]
pub(crate) enum ConfigError {
    #[error("could not read configuration at {path:?}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid configuration at {path:?}: {message}")]
    Parse { path: PathBuf, message: String },
    #[error(
        "{layer} configuration field `{field}`{location}: {reason}",
        location = source_suffix(.path)
    )]
    Invalid {
        layer: ConfigLayer,
        field: String,
        reason: &'static str,
        path: Option<PathBuf>,
    },
}

impl ConfigError {
    pub(super) fn invalid(
        layer: ConfigLayer,
        field: impl Into<String>,
        reason: &'static str,
    ) -> Self {
        Self::Invalid {
            layer,
            field: field.into(),
            reason,
            path: None,
        }
    }
}

/// Resolved values are distinct from the unmodified, versioned archetype.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub(crate) struct EffectiveConfig {
    pub(crate) archetype: ArchetypeDefinition,
    pub(crate) providers: BTreeMap<String, ProviderBinding>,
    #[serde(default)]
    pub(crate) allowed_project_roots: Vec<PathBuf>,
    pub(crate) limits: RunLimits,
    pub(crate) supervision: SupervisionPolicy,
    pub(crate) roles: BTreeMap<String, EffectiveRole>,
    /// Source files are metadata, separate from the resolved policy values.
    #[serde(skip)]
    pub(crate) provenance: Provenance,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub(crate) struct EffectiveRole {
    pub(crate) enabled: bool,
    pub(crate) max_instances: Option<u16>,
    pub(crate) permission_profile: PermissionProfile,
}

/// Resolves all authority before any runtime or provider side effect is possible.
pub(crate) fn resolve(
    global: &GlobalConfig,
    project: &ProjectConfig,
    operator: &OperatorOverrides,
) -> Result<EffectiveConfig, ConfigError> {
    if global.includes.is_some() {
        return Err(invalid_global(
            "includes",
            "includes must be expanded by the file loader before resolution",
        ));
    }
    let defaults = compiled_defaults();
    let mut provenance = Provenance::new();
    provenance::record_tree(
        &mut provenance,
        &defaults.providers,
        "providers",
        "providers",
        ConfigLayer::Compiled,
    );
    provenance::record_tree(
        &mut provenance,
        &defaults.limits,
        "limits",
        "limits",
        ConfigLayer::Compiled,
    );
    provenance::record_tree(
        &mut provenance,
        &defaults.supervision,
        "supervision",
        "supervision",
        ConfigLayer::Compiled,
    );
    let allowed_project_roots =
        global.allowed_project_roots.clone().unwrap_or_default();
    if allowed_project_roots.iter().any(|path| !path.is_absolute()) {
        return Err(invalid_global(
            "allowed_project_roots",
            "project roots must be absolute paths",
        ));
    }
    provenance::record(
        &mut provenance,
        "allowed_project_roots",
        if global.allowed_project_roots.is_some() {
            ConfigLayer::Global
        } else {
            ConfigLayer::Compiled
        },
    );
    let mut providers = defaults.providers;
    for (name, input) in &global.providers {
        check_name(name, &format!("providers.{name}"))?;
        let command = input
            .command
            .clone()
            .or_else(|| {
                providers.get(name).map(|binding| binding.command.clone())
            })
            .ok_or_else(|| {
                invalid_global(
                    format!("providers.{name}.command"),
                    "provider requires a command argument array",
                )
            })?;
        if command.first().is_none_or(|program| program.is_empty())
            || command.iter().any(|argument| argument.contains('\0'))
        {
            return Err(invalid_global(
                format!("providers.{name}.command"),
                "command requires a nonempty executable and arguments without NUL bytes",
            ));
        }
        providers.insert(name.clone(), ProviderBinding { command });
        if input.command.is_some() {
            provenance::record(
                &mut provenance,
                format!("providers.{name}.command"),
                ConfigLayer::Global,
            );
        }
    }
    let mut trusted_limits = defaults.limits;
    apply_limits(
        &mut trusted_limits,
        &global.limits,
        None,
        ConfigLayer::Global,
        &mut provenance,
    )?;
    let supervision = resolve_supervision(
        defaults.supervision,
        global.supervision,
        &mut provenance,
    )?;
    let profiles = resolve_profiles(&global.permission_profiles)?;
    let builtin = builtin_standard();
    let mut archetypes = BTreeMap::from([(builtin.reference.clone(), builtin)]);
    for (reference, input) in &global.archetypes {
        if !valid_reference(reference, "global") {
            return Err(invalid_global(
                format!("archetypes.{reference}"),
                "global definitions require a versioned global:name@N reference; builtin: is reserved",
            ));
        }
        let archetype =
            resolve_archetype(reference, input, &profiles, &providers)?;
        archetypes.insert(reference.clone(), archetype);
    }
    // Invalid lower-layer selectors must not disappear behind an operator choice.
    for (selection, layer) in [
        (&global.archetype, ConfigLayer::Global),
        (&project.archetype, ConfigLayer::Project),
        (&operator.archetype, ConfigLayer::Operator),
    ] {
        if selection
            .as_ref()
            .is_some_and(|reference| !archetypes.contains_key(reference))
        {
            return Err(ConfigError::invalid(
                layer,
                "archetype",
                "unknown archetype; select an available versioned builtin: or global: definition",
            ));
        }
    }
    let reference = operator
        .archetype
        .as_ref()
        .or(project.archetype.as_ref())
        .or(global.archetype.as_ref())
        .unwrap_or(&defaults.archetype);
    let archetype = archetypes
        .remove(reference)
        .expect("all selectors and the compiled default were validated");
    let selection_layer = if operator.archetype.is_some() {
        ConfigLayer::Operator
    } else if project.archetype.is_some() {
        ConfigLayer::Project
    } else if global.archetype.is_some() {
        ConfigLayer::Global
    } else {
        ConfigLayer::Compiled
    };
    provenance::record_archetype(
        &mut provenance,
        &archetype,
        global.archetypes.get(reference),
        ConfigSource::new(selection_layer, "archetype"),
    );
    let roles = archetype
        .roles
        .iter()
        .map(|(name, role)| {
            (
                name.clone(),
                EffectiveRole {
                    enabled: true,
                    max_instances: role.max_instances,
                    permission_profile: archetype.permission_profiles
                        [&role.permission_profile],
                },
            )
        })
        .collect();
    let mut effective = EffectiveConfig {
        archetype,
        providers,
        allowed_project_roots,
        limits: trusted_limits,
        supervision,
        roles,
        provenance,
    };
    apply_limits(
        &mut effective.limits,
        &project.limits,
        Some(trusted_limits),
        ConfigLayer::Project,
        &mut effective.provenance,
    )?;
    apply_roles(
        &mut effective,
        &project.roles,
        &profiles,
        ConfigLayer::Project,
    )?;
    apply_limits(
        &mut effective.limits,
        &operator.limits,
        Some(trusted_limits),
        ConfigLayer::Operator,
        &mut effective.provenance,
    )?;
    apply_roles(
        &mut effective,
        &operator.roles,
        &profiles,
        ConfigLayer::Operator,
    )?;
    for (name, role) in &effective.archetype.roles {
        if effective.roles[name].enabled
            && role.workspace == WorkspacePolicy::ReadOnly
            && effective.roles[name].permission_profile.filesystem
                != FilesystemPolicy::ReadOnly
        {
            return Err(invalid_global(
                format!(
                    "archetypes.{}.roles.{name}.workspace",
                    effective.archetype.reference
                ),
                "a read-only workspace requires an effective read-only filesystem permission profile",
            ));
        }
    }
    Ok(effective)
}

fn invalid_global(
    field: impl Into<String>,
    reason: &'static str,
) -> ConfigError {
    ConfigError::invalid(ConfigLayer::Global, field, reason)
}

fn check_name(name: &str, field: &str) -> Result<(), ConfigError> {
    if valid_name(name) {
        Ok(())
    } else {
        Err(invalid_global(
            field,
            "names must contain only ASCII letters, digits, underscores, or hyphens",
        ))
    }
}

fn valid_reference(reference: &str, namespace: &str) -> bool {
    let Some((prefix, rest)) = reference.split_once(':') else {
        return false;
    };
    let Some((name, version)) = rest.split_once('@') else {
        return false;
    };
    prefix == namespace
        && valid_name(name)
        && version
            .parse::<u32>()
            .is_ok_and(|number| number > 0 && number.to_string() == version)
}

fn required<T: Clone>(
    value: &Option<T>,
    field: String,
) -> Result<T, ConfigError> {
    value.clone().ok_or_else(|| {
        invalid_global(
            field,
            "required field is missing after merging trusted configuration",
        )
    })
}

fn resolve_profiles(
    inputs: &BTreeMap<String, ProfileInput>,
) -> Result<BTreeMap<String, PermissionProfile>, ConfigError> {
    inputs
        .iter()
        .map(|(name, input)| {
            let prefix = format!("permission_profiles.{name}");
            check_name(name, &prefix)?;
            Ok((
                name.clone(),
                PermissionProfile {
                    filesystem: required(
                        &input.filesystem,
                        format!("{prefix}.filesystem"),
                    )?,
                    network: required(
                        &input.network,
                        format!("{prefix}.network"),
                    )?,
                    approvals: required(
                        &input.approvals,
                        format!("{prefix}.approvals"),
                    )?,
                },
            ))
        })
        .collect()
}

fn resolve_archetype(
    reference: &str,
    input: &ArchetypeInput,
    profiles: &BTreeMap<String, PermissionProfile>,
    providers: &BTreeMap<String, ProviderBinding>,
) -> Result<ArchetypeDefinition, ConfigError> {
    let prefix = format!("archetypes.{reference}");
    let lead = required(&input.lead, format!("{prefix}.lead"))?;
    let mut roles = BTreeMap::new();
    let mut used_profiles = BTreeMap::new();
    for (name, input) in &input.roles {
        let prefix = format!("{prefix}.roles.{name}");
        check_name(name, &prefix)?;
        let provider = required(&input.provider, format!("{prefix}.provider"))?;
        if !providers.contains_key(&provider) {
            return Err(invalid_global(
                format!("{prefix}.provider"),
                "role references a missing provider binding",
            ));
        }
        let profile = required(
            &input.permission_profile,
            format!("{prefix}.permission_profile"),
        )?;
        let value = profiles.get(&profile).ok_or_else(|| {
            invalid_global(
                format!("{prefix}.permission_profile"),
                "role references a missing trusted permission profile",
            )
        })?;
        used_profiles.insert(profile.clone(), *value);
        let capabilities = required(&input.capabilities, format!("{prefix}.capabilities"))?.iter().map(|grant| {
            CapabilityGrant::parse(grant).ok_or_else(|| invalid_global(format!("{prefix}.capabilities"), "capability requires a supported namespace and an action name or *"))
        }).collect::<Result<_, _>>()?;
        roles.insert(
            name.clone(),
            RoleDefinition {
                provider,
                mode: required(&input.mode, format!("{prefix}.mode"))?,
                max_instances: input.max_instances,
                workspace: required(
                    &input.workspace,
                    format!("{prefix}.workspace"),
                )?,
                permission_profile: profile,
                instructions: input.instructions.clone(),
                capabilities,
            },
        );
    }
    if roles
        .get(&lead)
        .is_none_or(|role| role.mode != RoleMode::Interactive)
    {
        return Err(invalid_global(
            format!("{prefix}.lead"),
            "designated foreground role must exist and use interactive mode",
        ));
    }
    if roles[&lead].workspace == WorkspacePolicy::Worktree {
        return Err(invalid_global(
            format!("{prefix}.roles.{lead}.workspace"),
            "the initial foreground agent has no task assignment; use project or read-only, and assign worktree roles to background tasks",
        ));
    }
    Ok(ArchetypeDefinition {
        reference: reference.into(),
        lead,
        permission_profiles: used_profiles,
        roles,
    })
}

fn apply_limits(
    effective: &mut RunLimits,
    input: &LimitOverrides,
    ceiling: Option<RunLimits>,
    layer: ConfigLayer,
    provenance: &mut Provenance,
) -> Result<(), ConfigError> {
    let requested = RunLimits {
        max_concurrent_agents: input
            .max_concurrent_agents
            .unwrap_or(effective.max_concurrent_agents),
        max_agents_per_run: input
            .max_agents_per_run
            .unwrap_or(effective.max_agents_per_run),
        max_spawns_per_minute: input
            .max_spawns_per_minute
            .unwrap_or(effective.max_spawns_per_minute),
    };
    let bounded =
        ceiling.map_or(requested, |ceiling| requested.intersect(ceiling));
    macro_rules! apply {
        ($field:ident) => {
            if let Some(value) = input.$field {
                if value == 0 || value != bounded.$field {
                    return Err(ConfigError::invalid(layer, concat!("limits.", stringify!($field)), "limit must be positive and cannot exceed trusted operator policy"));
                }
                provenance::record(provenance, concat!("limits.", stringify!($field)), layer);
            }
        };
    }
    apply!(max_concurrent_agents);
    apply!(max_agents_per_run);
    apply!(max_spawns_per_minute);
    *effective = bounded;
    Ok(())
}

fn resolve_supervision(
    mut policy: SupervisionPolicy,
    input: SupervisionOverrides,
    provenance: &mut Provenance,
) -> Result<SupervisionPolicy, ConfigError> {
    macro_rules! apply {
        ($field:ident) => {
            if let Some(value) = input.$field {
                if value <= 0 || value > i64::MAX / 1_000 {
                    return Err(invalid_global(concat!("supervision.", stringify!($field)), "supervision bounds must be positive and fit millisecond arithmetic"));
                }
                policy.$field = value;
                provenance::record(provenance, concat!("supervision.", stringify!($field)), ConfigLayer::Global);
            }
        };
    }
    apply!(restart_window_seconds);
    apply!(max_launch_attempts);
    apply!(restart_backoff_seconds);
    apply!(startup_timeout_seconds);
    apply!(job_timeout_seconds);
    apply!(interrupt_grace_ms);
    apply!(shutdown_timeout_ms);
    if let Some(value) = input.idle_timeout_seconds {
        if !(0..=i64::MAX / 1_000).contains(&value) {
            return Err(invalid_global(
                "supervision.idle_timeout_seconds",
                "idle timeout must be nonnegative and fit millisecond arithmetic",
            ));
        }
        policy.idle_timeout_seconds = value;
        provenance::record(
            provenance,
            "supervision.idle_timeout_seconds",
            ConfigLayer::Global,
        );
    }
    if policy.interrupt_grace_ms >= policy.shutdown_timeout_ms {
        return Err(invalid_global(
            "supervision.interrupt_grace_ms",
            "interrupt grace must be shorter than the overall shutdown timeout",
        ));
    }
    Ok(policy)
}

fn apply_roles(
    effective: &mut EffectiveConfig,
    restrictions: &BTreeMap<String, RoleRestriction>,
    profiles: &BTreeMap<String, PermissionProfile>,
    layer: ConfigLayer,
) -> Result<(), ConfigError> {
    for (name, restriction) in restrictions {
        let prefix = format!("roles.{name}");
        let baseline = effective.archetype.role(name).ok_or_else(|| {
            ConfigError::invalid(
                layer,
                &prefix,
                "restriction references an undeclared role",
            )
        })?;
        let role = effective
            .roles
            .get_mut(name)
            .expect("effective roles match their archetype");
        let mut requested = role.clone();
        if let Some(enabled) = restriction.enabled {
            if !enabled && *name == effective.archetype.lead {
                return Err(ConfigError::invalid(
                    layer,
                    format!("{prefix}.enabled"),
                    "the designated foreground role cannot be disabled",
                ));
            }
            requested.enabled = enabled;
        }
        if let Some(capacity) = restriction.max_instances {
            requested.max_instances = Some(capacity);
        }
        if let Some(profile) = &restriction.permission_profile {
            let value = profiles.get(profile).ok_or_else(|| ConfigError::invalid(layer, format!("{prefix}.permission_profile"), "restriction must select an existing trusted global permission profile"))?;
            requested.permission_profile = *value;
        }
        // Operator overrides may restore project reductions, but only to the
        // selected trusted archetype. Omitted fields retain project restrictions.
        let ceiling = EffectiveRole {
            enabled: true,
            max_instances: baseline.max_instances,
            permission_profile: effective.archetype.permission_profiles
                [&baseline.permission_profile],
        };
        let bounded = requested.intersect(&ceiling);
        if requested.max_instances != bounded.max_instances {
            return Err(ConfigError::invalid(
                layer,
                format!("{prefix}.max_instances"),
                "capacity exceeds the trusted archetype",
            ));
        }
        if requested.permission_profile != bounded.permission_profile {
            return Err(ConfigError::invalid(
                layer,
                format!("{prefix}.permission_profile"),
                "permission profile exceeds or is incomparable with the trusted archetype profile",
            ));
        }
        *role = bounded;
        for (field, supplied) in [
            ("enabled", restriction.enabled.is_some()),
            ("max_instances", restriction.max_instances.is_some()),
        ] {
            if supplied {
                provenance::record(
                    &mut effective.provenance,
                    format!("{prefix}.{field}"),
                    layer,
                );
            }
        }
        if let Some(profile) = &restriction.permission_profile {
            for field in ["filesystem", "network", "approvals"] {
                effective.provenance.insert(
                    format!("{prefix}.permission_profile.{field}"),
                    ValueProvenance {
                        source: ConfigSource::new(
                            ConfigLayer::Global,
                            format!("permission_profiles.{profile}.{field}"),
                        ),
                        selected_by: Some(ConfigSource::new(
                            layer,
                            format!("{prefix}.permission_profile"),
                        )),
                    },
                );
            }
        }
    }
    Ok(())
}

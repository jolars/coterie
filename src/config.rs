//! Configuration loading, provenance, policy intersection, locking, and validation.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

mod input;
mod loader;
mod lock;
mod policy;
mod provenance;
mod resolver;

pub(crate) use input::*;
pub(crate) use loader::{ConfigLocations, load};
pub(crate) use lock::{ConfigLock, LockError, LockStatus};
pub(crate) use provenance::Provenance;
use provenance::{ConfigSource, ValueProvenance};
pub(crate) use resolver::{ConfigError, ConfigLayer, EffectiveConfig, resolve};

#[cfg(test)]
mod policy_tests;
#[cfg(test)]
mod provenance_tests;
#[cfg(test)]
mod resolution_tests;

const STANDARD_LEAD_INSTRUCTIONS: &str = "Coordinate work through Coterie. Delegate independent implementation and\n\
review tasks when useful, and report consolidated outcomes to the user.";

/// The compiled operator policy used when no trusted global configuration exists.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CompiledDefaults {
    pub(crate) archetype: String,
    pub(crate) providers: BTreeMap<String, ProviderBinding>,
    pub(crate) limits: RunLimits,
    pub(crate) supervision: SupervisionPolicy,
}

/// Trusted bounds on provider quota and process control, independent of roles.
#[derive(Clone, Copy, Debug, Eq, JsonSchema, PartialEq, Serialize)]
pub(crate) struct SupervisionPolicy {
    pub(crate) restart_window_seconds: i64,
    pub(crate) max_launch_attempts: i64,
    pub(crate) restart_backoff_seconds: i64,
    pub(crate) startup_timeout_seconds: i64,
    pub(crate) job_timeout_seconds: i64,
    pub(crate) interrupt_grace_ms: i64,
    pub(crate) shutdown_timeout_ms: i64,
}

/// A trusted command binding for an out-of-process provider.
#[derive(Clone, Debug, Eq, JsonSchema, PartialEq, Serialize)]
pub(crate) struct ProviderBinding {
    pub(crate) command: Vec<String>,
}

/// Operator ceilings that apply across archetypes.
#[derive(Clone, Copy, Debug, Eq, JsonSchema, PartialEq, Serialize)]
pub(crate) struct RunLimits {
    pub(crate) max_concurrent_agents: u16,
    pub(crate) max_agents_per_run: u16,
    pub(crate) max_spawns_per_minute: u16,
}

/// A sealed, versioned declaration of roles and provider policy.
#[derive(Clone, Debug, Eq, JsonSchema, PartialEq, Serialize)]
pub(crate) struct ArchetypeDefinition {
    pub(crate) reference: String,
    pub(crate) lead: String,
    pub(crate) permission_profiles: BTreeMap<String, PermissionProfile>,
    pub(crate) roles: BTreeMap<String, RoleDefinition>,
}

impl ArchetypeDefinition {
    /// Looks up a role without assigning semantics to its name.
    #[must_use]
    pub(crate) fn role(&self, name: &str) -> Option<&RoleDefinition> {
        self.roles.get(name)
    }

    /// Resolves one role capability to an explicit allow-or-deny decision.
    #[must_use]
    pub(crate) fn authorize(
        &self,
        role: &str,
        capability: Capability<'_>,
    ) -> AuthorizationDecision {
        self.role(role)
            .map_or(AuthorizationDecision::Denied, |role| {
                role.authorize(capability)
            })
    }
}

/// A configured type of agent with no runtime-defined role semantics.
#[derive(Clone, Debug, Eq, JsonSchema, PartialEq, Serialize)]
pub(crate) struct RoleDefinition {
    pub(crate) provider: String,
    pub(crate) mode: RoleMode,
    pub(crate) max_instances: Option<u16>,
    pub(crate) workspace: WorkspacePolicy,
    pub(crate) permission_profile: String,
    pub(crate) instructions: Option<String>,
    capabilities: Vec<CapabilityGrant>,
}

impl RoleDefinition {
    fn authorize(&self, capability: Capability<'_>) -> AuthorizationDecision {
        if self
            .capabilities
            .iter()
            .any(|grant| grant.allows(capability))
        {
            AuthorizationDecision::Allowed
        } else {
            AuthorizationDecision::Denied
        }
    }
}

/// The provider interaction style required by a role.
#[derive(
    Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize,
)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum RoleMode {
    Interactive,
    Job,
}

/// The kind of target workspace assigned to a role.
#[derive(
    Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize,
)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum WorkspacePolicy {
    Project,
    Worktree,
    ReadOnly,
}

/// Provider sandbox policy referenced by a role.
#[derive(
    Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize,
)]
#[serde(deny_unknown_fields)]
pub(crate) struct PermissionProfile {
    pub(crate) filesystem: FilesystemPolicy,
    pub(crate) network: NetworkPolicy,
    pub(crate) approvals: ApprovalPolicy,
}

/// Filesystem authority granted to a provider process.
#[derive(
    Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize,
)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum FilesystemPolicy {
    ProjectWrite,
    WorkspaceWrite,
    ReadOnly,
}

/// Network authority granted to a provider process.
#[derive(
    Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize,
)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum NetworkPolicy {
    ProviderDefault,
    Deny,
}

/// How a provider process may request operator approval.
#[derive(
    Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize,
)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum ApprovalPolicy {
    Interactive,
    Never,
}

/// One concrete supervisor action to authorize.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Capability<'name> {
    namespace: &'name str,
    action: &'name str,
}

impl<'name> Capability<'name> {
    /// Names an action within a capability namespace.
    #[must_use]
    pub(crate) const fn new(namespace: &'name str, action: &'name str) -> Self {
        Self { namespace, action }
    }
}

/// The only two outcomes of role capability authorization.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AuthorizationDecision {
    Allowed,
    Denied,
}

#[derive(Clone, Debug, Eq, JsonSchema, PartialEq, Serialize)]
struct CapabilityGrant {
    namespace: String,
    action: String,
}

impl CapabilityGrant {
    fn parse(value: &str) -> Option<Self> {
        let (namespace, action) = value.split_once(':')?;
        if !matches!(
            namespace,
            "spawn" | "send" | "task" | "logs" | "project" | "workspace"
        ) || !(action == "*" || valid_name(action))
        {
            return None;
        }
        Some(Self {
            namespace: namespace.into(),
            action: action.into(),
        })
    }

    fn allows(&self, capability: Capability<'_>) -> bool {
        self.namespace == capability.namespace
            && (self.action == "*" || self.action == capability.action)
    }
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')
        })
}

const LEAD_CAPABILITIES: &[&str] = &[
    "spawn:worker",
    "spawn:reviewer",
    "send:*",
    "task:*",
    "logs:*",
    "project:attach",
    "workspace:integrate",
];
const WORKER_CAPABILITIES: &[&str] = &[
    "send:lead",
    "send:peer",
    "task:read",
    "task:claim",
    "task:comment",
];
const REVIEWER_CAPABILITIES: &[&str] =
    &["send:lead", "task:read", "task:comment"];

fn compiled_capabilities(values: &[&str]) -> Vec<CapabilityGrant> {
    values
        .iter()
        .map(|value| {
            CapabilityGrant::parse(value)
                .expect("compiled capabilities are valid")
        })
        .collect()
}

/// Returns the operator defaults compiled into this Coterie version.
#[must_use]
pub(crate) fn compiled_defaults() -> CompiledDefaults {
    CompiledDefaults {
        archetype: "builtin:standard@1".into(),
        providers: BTreeMap::from([(
            "codex".into(),
            ProviderBinding {
                command: vec!["codex".into()],
            },
        )]),
        limits: RunLimits {
            max_concurrent_agents: 8,
            max_agents_per_run: 16,
            max_spawns_per_minute: 8,
        },
        supervision: SupervisionPolicy {
            restart_window_seconds: 60,
            max_launch_attempts: 3,
            restart_backoff_seconds: 1,
            startup_timeout_seconds: 30,
            job_timeout_seconds: 3_600,
            interrupt_grace_ms: 250,
            shutdown_timeout_ms: 5_000,
        },
    }
}

/// Returns the sealed `builtin:standard@1` archetype.
#[must_use]
pub(crate) fn builtin_standard() -> ArchetypeDefinition {
    ArchetypeDefinition {
        reference: "builtin:standard@1".into(),
        lead: "lead".into(),
        permission_profiles: BTreeMap::from([
            (
                "interactive".into(),
                PermissionProfile {
                    filesystem: FilesystemPolicy::ProjectWrite,
                    network: NetworkPolicy::ProviderDefault,
                    approvals: ApprovalPolicy::Interactive,
                },
            ),
            (
                "worker".into(),
                PermissionProfile {
                    filesystem: FilesystemPolicy::WorkspaceWrite,
                    network: NetworkPolicy::Deny,
                    approvals: ApprovalPolicy::Never,
                },
            ),
            (
                "review".into(),
                PermissionProfile {
                    filesystem: FilesystemPolicy::ReadOnly,
                    network: NetworkPolicy::Deny,
                    approvals: ApprovalPolicy::Never,
                },
            ),
        ]),
        roles: BTreeMap::from([
            (
                "lead".into(),
                RoleDefinition {
                    provider: "codex".into(),
                    mode: RoleMode::Interactive,
                    max_instances: None,
                    workspace: WorkspacePolicy::Project,
                    permission_profile: "interactive".into(),
                    instructions: Some(STANDARD_LEAD_INSTRUCTIONS.into()),
                    capabilities: compiled_capabilities(LEAD_CAPABILITIES),
                },
            ),
            (
                "worker".into(),
                RoleDefinition {
                    provider: "codex".into(),
                    mode: RoleMode::Job,
                    max_instances: Some(3),
                    workspace: WorkspacePolicy::Worktree,
                    permission_profile: "worker".into(),
                    instructions: None,
                    capabilities: compiled_capabilities(WORKER_CAPABILITIES),
                },
            ),
            (
                "reviewer".into(),
                RoleDefinition {
                    provider: "codex".into(),
                    mode: RoleMode::Job,
                    max_instances: Some(1),
                    workspace: WorkspacePolicy::ReadOnly,
                    permission_profile: "review".into(),
                    instructions: None,
                    capabilities: compiled_capabilities(REVIEWER_CAPABILITIES),
                },
            ),
        ]),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ApprovalPolicy, AuthorizationDecision, Capability, FilesystemPolicy,
        NetworkPolicy, PermissionProfile, RoleMode, RunLimits, WorkspacePolicy,
        builtin_standard, compiled_defaults,
    };

    #[test]
    fn compiled_defaults_match_the_operator_policy() {
        let defaults = compiled_defaults();

        assert_eq!(defaults.archetype, "builtin:standard@1");
        assert_eq!(defaults.providers.len(), 1);
        assert_eq!(
            defaults
                .providers
                .get("codex")
                .expect("the default provider should exist")
                .command,
            ["codex"]
        );
        assert_eq!(
            defaults.limits,
            RunLimits {
                max_concurrent_agents: 8,
                max_agents_per_run: 16,
                max_spawns_per_minute: 8,
            }
        );
    }

    #[test]
    fn builtin_standard_is_the_sealed_versioned_archetype() {
        let archetype = builtin_standard();

        assert_eq!(archetype.reference, "builtin:standard@1");
        assert_eq!(archetype.lead, "lead");
        assert_eq!(archetype.permission_profiles.len(), 3);
        assert_eq!(
            archetype.permission_profiles.get("interactive"),
            Some(&PermissionProfile {
                filesystem: FilesystemPolicy::ProjectWrite,
                network: NetworkPolicy::ProviderDefault,
                approvals: ApprovalPolicy::Interactive,
            })
        );
        assert_eq!(
            archetype.permission_profiles.get("worker"),
            Some(&PermissionProfile {
                filesystem: FilesystemPolicy::WorkspaceWrite,
                network: NetworkPolicy::Deny,
                approvals: ApprovalPolicy::Never,
            })
        );
        assert_eq!(
            archetype.permission_profiles.get("review"),
            Some(&PermissionProfile {
                filesystem: FilesystemPolicy::ReadOnly,
                network: NetworkPolicy::Deny,
                approvals: ApprovalPolicy::Never,
            })
        );

        let lead = archetype.role("lead").expect("the lead role should exist");
        assert_eq!(lead.provider, "codex");
        assert_eq!(lead.mode, RoleMode::Interactive);
        assert_eq!(lead.max_instances, None);
        assert_eq!(lead.workspace, WorkspacePolicy::Project);
        assert_eq!(lead.permission_profile, "interactive");
        assert_eq!(lead.capabilities.len(), 7);
        assert_eq!(
            lead.instructions.as_deref(),
            Some(
                "Coordinate work through Coterie. Delegate independent implementation and\n\
                 review tasks when useful, and report consolidated outcomes to the user."
            )
        );

        let worker = archetype
            .role("worker")
            .expect("the worker role should exist");
        assert_eq!(worker.provider, "codex");
        assert_eq!(worker.mode, RoleMode::Job);
        assert_eq!(worker.max_instances, Some(3));
        assert_eq!(worker.workspace, WorkspacePolicy::Worktree);
        assert_eq!(worker.permission_profile, "worker");
        assert_eq!(worker.instructions, None);
        assert_eq!(worker.capabilities.len(), 5);

        let reviewer = archetype
            .role("reviewer")
            .expect("the reviewer role should exist");
        assert_eq!(reviewer.provider, "codex");
        assert_eq!(reviewer.mode, RoleMode::Job);
        assert_eq!(reviewer.max_instances, Some(1));
        assert_eq!(reviewer.workspace, WorkspacePolicy::ReadOnly);
        assert_eq!(reviewer.permission_profile, "review");
        assert_eq!(reviewer.instructions, None);
        assert_eq!(reviewer.capabilities.len(), 3);
        assert_eq!(archetype.roles.len(), 3);
    }

    #[test]
    fn builtin_standard_authorizes_only_its_declared_capabilities() {
        let archetype = builtin_standard();
        let cases = [
            ("lead", Capability::new("spawn", "worker"), true),
            ("lead", Capability::new("spawn", "reviewer"), true),
            ("lead", Capability::new("spawn", "lead"), false),
            ("lead", Capability::new("send", "worker"), true),
            ("lead", Capability::new("send", "unknown-role"), true),
            ("lead", Capability::new("task", "create"), true),
            ("lead", Capability::new("task", "close"), true),
            ("lead", Capability::new("logs", "worker"), true),
            ("lead", Capability::new("project", "attach"), true),
            ("lead", Capability::new("project", "detach"), false),
            ("lead", Capability::new("workspace", "integrate"), true),
            ("lead", Capability::new("workspace", "delete"), false),
            ("worker", Capability::new("send", "lead"), true),
            ("worker", Capability::new("send", "peer"), true),
            ("worker", Capability::new("send", "reviewer"), false),
            ("worker", Capability::new("task", "read"), true),
            ("worker", Capability::new("task", "claim"), true),
            ("worker", Capability::new("task", "comment"), true),
            ("worker", Capability::new("task", "close"), false),
            ("worker", Capability::new("spawn", "worker"), false),
            ("reviewer", Capability::new("send", "lead"), true),
            ("reviewer", Capability::new("send", "peer"), false),
            ("reviewer", Capability::new("task", "read"), true),
            ("reviewer", Capability::new("task", "comment"), true),
            ("reviewer", Capability::new("task", "claim"), false),
            ("unknown", Capability::new("task", "read"), false),
        ];

        for (role, capability, expected) in cases {
            assert_eq!(
                archetype.authorize(role, capability),
                if expected {
                    AuthorizationDecision::Allowed
                } else {
                    AuthorizationDecision::Denied
                },
                "unexpected authorization decision for {role} and {capability:?}",
            );
        }
    }
}

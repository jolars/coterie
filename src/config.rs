//! Configuration loading, provenance, policy intersection, locking, and validation.

use std::collections::BTreeMap;

const STANDARD_LEAD_INSTRUCTIONS: &str = "Coordinate work through Coterie. Delegate independent implementation and\n\
review tasks when useful, and report consolidated outcomes to the user.";

/// The compiled operator policy used when no trusted global configuration exists.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CompiledDefaults {
    pub(crate) archetype: &'static str,
    pub(crate) providers: BTreeMap<&'static str, ProviderBinding>,
    pub(crate) limits: RunLimits,
}

/// A trusted command binding for an out-of-process provider.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ProviderBinding {
    pub(crate) command: &'static [&'static str],
}

/// Operator ceilings that apply across archetypes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RunLimits {
    pub(crate) max_concurrent_agents: u16,
    pub(crate) max_agents_per_run: u16,
    pub(crate) max_spawns_per_minute: u16,
}

/// A sealed, versioned declaration of roles and provider policy.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ArchetypeDefinition {
    pub(crate) reference: &'static str,
    pub(crate) lead: &'static str,
    pub(crate) permission_profiles: BTreeMap<&'static str, PermissionProfile>,
    pub(crate) roles: BTreeMap<&'static str, RoleDefinition>,
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
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RoleDefinition {
    pub(crate) provider: &'static str,
    pub(crate) mode: RoleMode,
    pub(crate) max_instances: Option<u16>,
    pub(crate) workspace: WorkspacePolicy,
    pub(crate) permission_profile: &'static str,
    pub(crate) instructions: Option<&'static str>,
    capabilities: &'static [CapabilityGrant],
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
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RoleMode {
    Interactive,
    Job,
}

/// The kind of target workspace assigned to a role.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WorkspacePolicy {
    Project,
    Worktree,
    ReadOnly,
}

/// Provider sandbox policy referenced by a role.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PermissionProfile {
    pub(crate) filesystem: FilesystemPolicy,
    pub(crate) network: NetworkPolicy,
    pub(crate) approvals: ApprovalPolicy,
}

/// Filesystem authority granted to a provider process.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FilesystemPolicy {
    ProjectWrite,
    WorkspaceWrite,
    ReadOnly,
}

/// Network authority granted to a provider process.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum NetworkPolicy {
    ProviderDefault,
    Deny,
}

/// How a provider process may request operator approval.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CapabilityGrant {
    Exact {
        namespace: &'static str,
        action: &'static str,
    },
    Namespace(&'static str),
}

impl CapabilityGrant {
    const fn exact(namespace: &'static str, action: &'static str) -> Self {
        Self::Exact { namespace, action }
    }

    const fn namespace(namespace: &'static str) -> Self {
        Self::Namespace(namespace)
    }

    fn allows(self, capability: Capability<'_>) -> bool {
        match self {
            Self::Exact { namespace, action } => {
                capability.namespace == namespace && capability.action == action
            }
            Self::Namespace(namespace) => capability.namespace == namespace,
        }
    }
}

const LEAD_CAPABILITIES: &[CapabilityGrant] = &[
    CapabilityGrant::exact("spawn", "worker"),
    CapabilityGrant::exact("spawn", "reviewer"),
    CapabilityGrant::namespace("send"),
    CapabilityGrant::namespace("task"),
    CapabilityGrant::exact("project", "attach"),
    CapabilityGrant::exact("workspace", "integrate"),
];

const WORKER_CAPABILITIES: &[CapabilityGrant] = &[
    CapabilityGrant::exact("send", "lead"),
    CapabilityGrant::exact("send", "peer"),
    CapabilityGrant::exact("task", "read"),
    CapabilityGrant::exact("task", "claim"),
    CapabilityGrant::exact("task", "comment"),
];

const REVIEWER_CAPABILITIES: &[CapabilityGrant] = &[
    CapabilityGrant::exact("send", "lead"),
    CapabilityGrant::exact("task", "read"),
    CapabilityGrant::exact("task", "comment"),
];

/// Returns the operator defaults compiled into this Coterie version.
#[must_use]
pub(crate) fn compiled_defaults() -> CompiledDefaults {
    CompiledDefaults {
        archetype: "builtin:standard@1",
        providers: BTreeMap::from([(
            "codex",
            ProviderBinding {
                command: &["codex"],
            },
        )]),
        limits: RunLimits {
            max_concurrent_agents: 8,
            max_agents_per_run: 16,
            max_spawns_per_minute: 8,
        },
    }
}

/// Returns the sealed `builtin:standard@1` archetype.
#[must_use]
pub(crate) fn builtin_standard() -> ArchetypeDefinition {
    ArchetypeDefinition {
        reference: "builtin:standard@1",
        lead: "lead",
        permission_profiles: BTreeMap::from([
            (
                "interactive",
                PermissionProfile {
                    filesystem: FilesystemPolicy::ProjectWrite,
                    network: NetworkPolicy::ProviderDefault,
                    approvals: ApprovalPolicy::Interactive,
                },
            ),
            (
                "worker",
                PermissionProfile {
                    filesystem: FilesystemPolicy::WorkspaceWrite,
                    network: NetworkPolicy::Deny,
                    approvals: ApprovalPolicy::Never,
                },
            ),
            (
                "review",
                PermissionProfile {
                    filesystem: FilesystemPolicy::ReadOnly,
                    network: NetworkPolicy::Deny,
                    approvals: ApprovalPolicy::Never,
                },
            ),
        ]),
        roles: BTreeMap::from([
            (
                "lead",
                RoleDefinition {
                    provider: "codex",
                    mode: RoleMode::Interactive,
                    max_instances: None,
                    workspace: WorkspacePolicy::Project,
                    permission_profile: "interactive",
                    instructions: Some(STANDARD_LEAD_INSTRUCTIONS),
                    capabilities: LEAD_CAPABILITIES,
                },
            ),
            (
                "worker",
                RoleDefinition {
                    provider: "codex",
                    mode: RoleMode::Job,
                    max_instances: Some(3),
                    workspace: WorkspacePolicy::Worktree,
                    permission_profile: "worker",
                    instructions: None,
                    capabilities: WORKER_CAPABILITIES,
                },
            ),
            (
                "reviewer",
                RoleDefinition {
                    provider: "codex",
                    mode: RoleMode::Job,
                    max_instances: Some(1),
                    workspace: WorkspacePolicy::ReadOnly,
                    permission_profile: "review",
                    instructions: None,
                    capabilities: REVIEWER_CAPABILITIES,
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
        assert_eq!(lead.capabilities.len(), 6);
        assert_eq!(
            lead.instructions,
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

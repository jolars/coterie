//! Policy intersection retains only authority and capacity shared by both inputs.

use super::resolver::EffectiveRole;
use super::{
    ApprovalPolicy, FilesystemPolicy, NetworkPolicy, PermissionProfile,
    RunLimits,
};

impl PermissionProfile {
    pub(super) fn intersect(self, other: Self) -> Self {
        Self {
            // The two writable scopes may refer to different roots.
            filesystem: if self.filesystem == other.filesystem {
                self.filesystem
            } else {
                FilesystemPolicy::ReadOnly
            },
            network: if self.network == other.network {
                self.network
            } else {
                NetworkPolicy::Deny
            },
            approvals: if self.approvals == other.approvals {
                self.approvals
            } else {
                ApprovalPolicy::Never
            },
        }
    }

    pub(crate) fn no_more_permissive_than(self, ceiling: Self) -> bool {
        self.intersect(ceiling) == self
    }
}

impl RunLimits {
    pub(super) fn intersect(self, other: Self) -> Self {
        Self {
            max_concurrent_agents: self
                .max_concurrent_agents
                .min(other.max_concurrent_agents),
            max_agents_per_run: self
                .max_agents_per_run
                .min(other.max_agents_per_run),
            max_spawns_per_minute: self
                .max_spawns_per_minute
                .min(other.max_spawns_per_minute),
        }
    }
}

impl EffectiveRole {
    pub(super) fn intersect(&self, other: &Self) -> Self {
        Self {
            enabled: self.enabled && other.enabled,
            // An omitted role ceiling still remains subject to run-wide limits.
            max_instances: match (self.max_instances, other.max_instances) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            },
            permission_profile: self
                .permission_profile
                .intersect(other.permission_profile),
        }
    }
}

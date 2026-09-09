//! File inputs are partial so trusted includes can contribute individual fields.

use std::collections::BTreeMap;
use std::path::PathBuf;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Serialize};

use super::{
    ApprovalPolicy, FilesystemPolicy, NetworkPolicy, RoleMode, WorkspacePolicy,
};

/// The file format version, independent of the archetype's semantic version.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
#[serde(try_from = "u16", into = "u16")]
pub(crate) struct ConfigSchemaVersion;

impl TryFrom<u16> for ConfigSchemaVersion {
    type Error = &'static str;

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        if value == 1 {
            Ok(Self)
        } else {
            Err("unsupported configuration schema_version; expected 1")
        }
    }
}

impl From<ConfigSchemaVersion> for u16 {
    fn from(_: ConfigSchemaVersion) -> Self {
        1
    }
}

impl JsonSchema for ConfigSchemaVersion {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "ConfigSchemaVersion".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({"type": "integer", "const": 1})
    }
}

/// Trusted operator definitions. Included files use the same partial format.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct GlobalConfig {
    pub(crate) schema_version: ConfigSchemaVersion,
    pub(crate) archetype: Option<String>,
    pub(crate) includes: Option<Vec<PathBuf>>,
    pub(crate) providers: BTreeMap<String, ProviderInput>,
    pub(crate) limits: LimitOverrides,
    pub(crate) supervision: SupervisionOverrides,
    pub(crate) permission_profiles: BTreeMap<String, ProfileInput>,
    pub(crate) archetypes: BTreeMap<String, ArchetypeInput>,
}

/// Untrusted repository requests can only select definitions and restrict policy.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct ProjectConfig {
    pub(crate) schema_version: ConfigSchemaVersion,
    pub(crate) archetype: Option<String>,
    pub(crate) limits: LimitOverrides,
    pub(crate) roles: BTreeMap<String, RoleRestriction>,
}

/// Explicit operator input, supplied by a future CLI boundary rather than a file.
#[derive(Clone, Debug, Default)]
pub(crate) struct OperatorOverrides {
    pub(crate) archetype: Option<String>,
    pub(crate) limits: LimitOverrides,
    pub(crate) roles: BTreeMap<String, RoleRestriction>,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct ProviderInput {
    pub(crate) command: Option<Vec<String>>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, JsonSchema, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct LimitOverrides {
    pub(crate) max_concurrent_agents: Option<u16>,
    pub(crate) max_agents_per_run: Option<u16>,
    pub(crate) max_spawns_per_minute: Option<u16>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, JsonSchema, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct SupervisionOverrides {
    pub(crate) restart_window_seconds: Option<i64>,
    pub(crate) max_launch_attempts: Option<i64>,
    pub(crate) restart_backoff_seconds: Option<i64>,
    pub(crate) startup_timeout_seconds: Option<i64>,
    pub(crate) job_timeout_seconds: Option<i64>,
    pub(crate) interrupt_grace_ms: Option<i64>,
    pub(crate) shutdown_timeout_ms: Option<i64>,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct ArchetypeInput {
    pub(crate) lead: Option<String>,
    pub(crate) roles: BTreeMap<String, RoleInput>,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct RoleInput {
    pub(crate) provider: Option<String>,
    pub(crate) mode: Option<RoleMode>,
    pub(crate) max_instances: Option<u16>,
    pub(crate) workspace: Option<WorkspacePolicy>,
    pub(crate) permission_profile: Option<String>,
    pub(crate) instructions: Option<String>,
    pub(crate) capabilities: Option<Vec<String>>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, JsonSchema, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct ProfileInput {
    pub(crate) filesystem: Option<FilesystemPolicy>,
    pub(crate) network: Option<NetworkPolicy>,
    pub(crate) approvals: Option<ApprovalPolicy>,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct RoleRestriction {
    pub(crate) enabled: Option<bool>,
    pub(crate) max_instances: Option<u16>,
    pub(crate) permission_profile: Option<String>,
}

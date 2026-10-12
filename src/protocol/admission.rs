//! Bounded saved-policy observations available to callers with spawn authority.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::context::TextPreview;
use crate::config::ConfigLayer;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub(crate) struct AdmissionView {
    pub(crate) observed_at: i64,
    pub(crate) concurrent_agents: AdmissionBudget,
    pub(crate) agents_per_run: AdmissionBudget,
    pub(crate) spawns_per_minute: AdmissionBudget,
    #[schemars(length(max = 16))]
    pub(crate) roles: Vec<RoleAdmission>,
    pub(crate) omitted_roles: usize,
    pub(crate) warning: Option<String>,
    pub(crate) guidance: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub(crate) struct AdmissionBudget {
    pub(crate) used: u64,
    pub(crate) limit: Option<u16>,
    pub(crate) remaining: Option<u64>,
    pub(crate) exhausted: bool,
    pub(crate) released_by: CapacityRelease,
    pub(crate) source: Option<PolicySource>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub(crate) struct RoleAdmission {
    pub(crate) role: TextPreview,
    pub(crate) enabled: bool,
    pub(crate) enabled_source: Option<PolicySource>,
    pub(crate) active_instances: AdmissionBudget,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub(crate) struct PolicySource {
    pub(crate) layer: ConfigLayer,
    pub(crate) field: TextPreview,
}

#[derive(
    Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CapacityRelease {
    AgentTerminal,
    RollingMinute,
    Never,
}

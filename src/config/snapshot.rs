//! Run snapshots retain host bindings and provenance independently of portable locks.

use serde::{Deserialize, Serialize};

use super::{ConfigLock, ConfigSchemaVersion, EffectiveConfig, Provenance};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RunConfiguration {
    schema_version: ConfigSchemaVersion,
    pub(crate) effective: EffectiveConfig,
    provenance: Provenance,
}

impl RunConfiguration {
    pub(crate) fn new(effective: EffectiveConfig) -> Self {
        Self {
            schema_version: ConfigSchemaVersion,
            provenance: effective.provenance.clone(),
            effective,
        }
    }

    pub(crate) fn fingerprint(&self) -> String {
        ConfigLock::for_config(&self.effective).fingerprint
    }

    /// Source relocation and equal assignments do not change runtime behavior.
    pub(crate) fn differences(&self, current: &EffectiveConfig) -> Vec<String> {
        let saved =
            serde_json::to_value(&self.effective).expect("typed configuration");
        let current =
            serde_json::to_value(current).expect("typed configuration");
        saved
            .as_object()
            .expect("configuration object")
            .iter()
            .filter(|(key, value)| current.get(*key) != Some(*value))
            .map(|(key, _)| key.clone())
            .collect()
    }

    pub(crate) fn restore(mut self) -> EffectiveConfig {
        self.effective.provenance = self.provenance;
        self.effective
    }

    pub(crate) fn valid(&self, fingerprint: &str) -> bool {
        let config = &self.effective;
        config.archetype.roles.keys().eq(config.roles.keys())
            && config
                .roles
                .get(&config.archetype.lead)
                .is_some_and(|role| role.enabled)
            && config.archetype.roles.values().all(|role| {
                config.providers.get(&role.provider).is_some_and(|binding| {
                    binding
                        .command
                        .first()
                        .is_some_and(|program| !program.is_empty())
                })
            })
            && self.fingerprint() == fingerprint
    }
}

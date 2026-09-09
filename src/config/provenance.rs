//! Origins follow assignments, including explicit values equal to their defaults.

use std::collections::BTreeMap;
use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{ArchetypeDefinition, ArchetypeInput, ConfigLayer};

/// Field paths use dots; definition names cannot contain dots.
pub(crate) type Provenance = BTreeMap<String, ValueProvenance>;

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub(crate) struct ConfigSource {
    pub(crate) layer: ConfigLayer,
    pub(crate) file: Option<PathBuf>,
    pub(crate) field: String,
}

impl ConfigSource {
    pub(super) fn new(layer: ConfigLayer, field: impl Into<String>) -> Self {
        Self {
            layer,
            file: None,
            field: field.into(),
        }
    }
}

/// A reference selects a value without becoming the source of its definition.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub(crate) struct ValueProvenance {
    pub(crate) source: ConfigSource,
    pub(crate) selected_by: Option<ConfigSource>,
}

impl ValueProvenance {
    pub(super) fn direct(layer: ConfigLayer, field: impl Into<String>) -> Self {
        Self {
            source: ConfigSource::new(layer, field),
            selected_by: None,
        }
    }
}

pub(super) fn record(
    provenance: &mut Provenance,
    field: impl Into<String>,
    layer: ConfigLayer,
) {
    let field = field.into();
    provenance.insert(field.clone(), ValueProvenance::direct(layer, field));
}

/// Arrays are atomic assignments, while every scalar and optional default has an origin.
pub(super) fn record_tree(
    provenance: &mut Provenance,
    value: &impl Serialize,
    output: &str,
    source: &str,
    layer: ConfigLayer,
) {
    fn visit(
        provenance: &mut Provenance,
        value: &serde_json::Value,
        output: &str,
        source: &str,
        layer: ConfigLayer,
    ) {
        if let serde_json::Value::Object(fields) = value {
            for (field, value) in fields {
                visit(
                    provenance,
                    value,
                    &format!("{output}.{field}"),
                    &format!("{source}.{field}"),
                    layer,
                );
            }
        } else {
            provenance
                .insert(output.into(), ValueProvenance::direct(layer, source));
        }
    }
    // These internal policy types contain only JSON-compatible data.
    let value =
        serde_json::to_value(value).expect("configuration values serialize");
    visit(provenance, &value, output, source, layer);
}

pub(super) fn record_archetype(
    provenance: &mut Provenance,
    archetype: &ArchetypeDefinition,
    input: Option<&ArchetypeInput>,
    selector: ConfigSource,
) {
    let prefix = format!("archetypes.{}", archetype.reference);
    let layer = if input.is_some() {
        ConfigLayer::Global
    } else {
        ConfigLayer::Builtin
    };
    record_tree(provenance, archetype, "archetype", &prefix, layer);
    provenance.insert(
        "archetype.reference".into(),
        ValueProvenance {
            source: ConfigSource::new(layer, &prefix),
            selected_by: Some(selector),
        },
    );
    if let Some(input) = input {
        for (name, profile) in &archetype.permission_profiles {
            record_tree(
                provenance,
                profile,
                &format!("archetype.permission_profiles.{name}"),
                &format!("permission_profiles.{name}"),
                layer,
            );
        }
        for (name, role) in &input.roles {
            for (field, absent) in [
                ("max_instances", role.max_instances.is_none()),
                ("instructions", role.instructions.is_none()),
            ] {
                if absent {
                    provenance.insert(
                        format!("archetype.roles.{name}.{field}"),
                        ValueProvenance::direct(
                            ConfigLayer::Compiled,
                            format!("{prefix}.roles.{name}.{field}"),
                        ),
                    );
                }
            }
        }
    }
    for (name, role) in &archetype.roles {
        record(
            provenance,
            format!("roles.{name}.enabled"),
            ConfigLayer::Compiled,
        );
        let capacity = provenance
            [&format!("archetype.roles.{name}.max_instances")]
            .clone();
        provenance.insert(format!("roles.{name}.max_instances"), capacity);
        let selector = provenance
            [&format!("archetype.roles.{name}.permission_profile")]
            .source
            .clone();
        for field in ["filesystem", "network", "approvals"] {
            let mut origin = provenance[&format!(
                "archetype.permission_profiles.{}.{field}",
                role.permission_profile
            )]
                .clone();
            origin.selected_by = Some(selector.clone());
            provenance.insert(
                format!("roles.{name}.permission_profile.{field}"),
                origin,
            );
        }
    }
}

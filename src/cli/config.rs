//! Local configuration inspection has no supervisor or provider side effects.

use std::io::{self, Write};

use clap::{Args, Subcommand, ValueEnum};
use schemars::JsonSchema;
use serde::Serialize;

use crate::config::{
    ConfigLocations, ConfigLock, EffectiveConfig, GlobalConfig, LockStatus,
    OperatorOverrides, ProjectConfig, load,
};
use crate::project::DiscoveredProject;
use crate::supervisor::SupervisorError;

use super::{ExitCategory, RenderError};

#[derive(Debug, Args)]
pub(crate) struct ConfigArguments {
    #[command(subcommand)]
    command: ConfigCommand,
}

#[derive(Debug, Subcommand)]
enum ConfigCommand {
    /// Validate configuration and verify coterie.lock when present.
    Check,
    /// Show resolved configuration and verify coterie.lock when present.
    Show {
        /// Show effective policy (the default and currently supported view).
        #[arg(long)]
        effective: bool,
        /// Include the source layer, file, and selectors for every effective value.
        #[arg(long)]
        provenance: bool,
    },
    /// Generate JSON Schema from the typed contract without loading configuration.
    Schema {
        #[arg(long, value_enum, default_value = "project")]
        target: SchemaTarget,
    },
    /// Explicitly create or replace coterie.lock with the resolved portable policy.
    Lock,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub(crate) enum SchemaTarget {
    Global,
    Project,
    Lock,
    Effective,
}

impl SchemaTarget {
    pub(crate) fn generate(self) -> schemars::Schema {
        match self {
            Self::Global => schemars::schema_for!(GlobalConfig),
            Self::Project => schemars::schema_for!(ProjectConfig),
            Self::Lock => schemars::schema_for!(ConfigLock),
            Self::Effective => {
                schemars::generate::SchemaSettings::draft2020_12()
                    .with(|settings| {
                        settings.contract =
                            schemars::generate::Contract::Serialize
                    })
                    .into_generator()
                    .into_root_schema_for::<EffectiveReport<'static>>()
            }
        }
    }
}

/// Command data inside the version 1 CLI success envelope.
#[derive(Serialize, JsonSchema)]
pub(crate) struct EffectiveReport<'a> {
    effective: &'a EffectiveConfig,
    fingerprint: &'a str,
    lock: LockStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    provenance: Option<&'a crate::config::Provenance>,
}

pub(crate) fn run(
    arguments: ConfigArguments,
    json: bool,
) -> Result<ExitCategory, SupervisorError> {
    if let ConfigCommand::Schema { target } = arguments.command {
        return render(json, &target.generate());
    }
    let cwd =
        std::env::current_dir().map_err(SupervisorError::CurrentDirectory)?;
    let project = DiscoveredProject::discover(&cwd)?;
    let config = load(
        &ConfigLocations::from_environment(&project),
        &OperatorOverrides::default(),
    )?;
    let lock = ConfigLock::for_config(&config);
    let path = project.canonical_path.join("coterie.lock");
    if matches!(arguments.command, ConfigCommand::Lock) {
        lock.write(&path)?;
        if json {
            return render(true, &lock);
        }
        return line("Wrote coterie.lock.");
    }
    let status = lock.verify(&path)?;
    match arguments.command {
        ConfigCommand::Check => {
            if json {
                render(
                    true,
                    &serde_json::json!({
                        "archetype": lock.archetype,
                        "fingerprint": lock.fingerprint,
                        "lock": status,
                    }),
                )
            } else {
                line(match status {
                    LockStatus::Absent => {
                        "Configuration is valid; coterie.lock is absent."
                    }
                    LockStatus::Verified => {
                        "Configuration is valid; coterie.lock is verified."
                    }
                })
            }
        }
        ConfigCommand::Show { provenance, .. } => {
            let report = EffectiveReport {
                effective: &config,
                fingerprint: &lock.fingerprint,
                lock: status,
                provenance: provenance.then_some(&config.provenance),
            };
            // Redact values before encoding JSON so quotes and escapes in credentials cannot bypass filtering.
            let mut value = serde_json::to_value(report)?;
            redact(&mut value);
            render(json, &value)
        }
        ConfigCommand::Schema { .. } | ConfigCommand::Lock => {
            unreachable!("handled before verification")
        }
    }
}

fn redact(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::String(text) => *text = crate::redaction::text(text),
        serde_json::Value::Array(values) => values.iter_mut().for_each(redact),
        serde_json::Value::Object(fields) => {
            *fields = std::mem::take(fields)
                .into_iter()
                .map(|(key, mut value)| {
                    redact(&mut value);
                    (crate::redaction::text(&key), value)
                })
                .collect();
        }
        _ => {}
    }
}

fn render(
    json: bool,
    data: &impl Serialize,
) -> Result<ExitCategory, SupervisorError> {
    let mut stdout = io::stdout().lock();
    let mut stderr = io::stderr().lock();
    if json {
        super::render_json_success(&mut stdout, &mut stderr, data)
    } else {
        super::render_human_success(&mut stdout, &mut stderr, data)
    }
    .map_err(Into::into)
}

fn line(text: &str) -> Result<ExitCategory, SupervisorError> {
    writeln!(io::stdout().lock(), "{text}").map_err(RenderError::from)?;
    Ok(ExitCategory::Success)
}

//! File discovery and loading never start a run or execute a provider.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;

use super::{
    ConfigError, ConfigLayer, EffectiveConfig, GlobalConfig, OperatorOverrides,
    ProjectConfig, resolve,
};
use crate::project::DiscoveredProject;

/// Explicit locations make loading independent of process-wide test environment.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ConfigLocations {
    pub(crate) global: Option<PathBuf>,
    pub(crate) project: PathBuf,
}

impl ConfigLocations {
    pub(crate) fn from_environment(project: &DiscoveredProject) -> Self {
        Self::from_environment_values(
            &project.canonical_path,
            std::env::var_os("XDG_CONFIG_HOME"),
            std::env::var_os("HOME"),
        )
    }

    pub(crate) fn from_environment_values(
        project_root: &Path,
        xdg: Option<OsString>,
        home: Option<OsString>,
    ) -> Self {
        let global = xdg
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .or_else(|| {
                home.map(PathBuf::from)
                    .filter(|path| path.is_absolute())
                    .map(|path| path.join(".config"))
            })
            .map(|path| path.join("coterie/config.toml"));
        Self {
            global,
            project: project_root.join("coterie.toml"),
        }
    }
}

/// Reads optional files, validates every input layer, and resolves effective policy.
pub(crate) fn load(
    locations: &ConfigLocations,
    overrides: &OperatorOverrides,
) -> Result<EffectiveConfig, ConfigError> {
    let mut sources = BTreeMap::new();
    let global = match &locations.global {
        Some(path) => load_global(path, &mut sources)?,
        None => GlobalConfig::default(),
    };
    let project = read(&locations.project, true)?.map_or_else(
        || Ok(ProjectConfig::default()),
        |text| parse(&text, &locations.project),
    )?;
    resolve(&global, &project, overrides).map_err(|mut error| {
        if let ConfigError::Invalid {
            layer, field, path, ..
        } = &mut error
        {
            *path = match layer {
                ConfigLayer::Global => {
                    let mut key = field.as_str();
                    loop {
                        if let Some(source) = sources.get(key) {
                            break Some(source.clone());
                        }
                        match key.rsplit_once('.') {
                            Some((parent, _)) => key = parent,
                            None => break locations.global.clone(),
                        }
                    }
                }
                ConfigLayer::Project => Some(locations.project.clone()),
                ConfigLayer::Operator => None,
            };
        }
        error
    })
}

fn read(path: &Path, optional: bool) -> Result<Option<String>, ConfigError> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(source)
            if optional
                && source.kind() == std::io::ErrorKind::NotFound
                && fs::symlink_metadata(path).is_err_and(|error| {
                    error.kind() == std::io::ErrorKind::NotFound
                }) =>
        {
            Ok(None)
        }
        Err(source) => Err(ConfigError::Io {
            path: path.into(),
            source,
        }),
    }
}

fn parse<T: DeserializeOwned>(
    text: &str,
    path: &Path,
) -> Result<T, ConfigError> {
    toml::from_str(text).map_err(|error| parse_error(error, path))
}

fn parse_error(mut error: toml::de::Error, path: &Path) -> ConfigError {
    // Retain the field path without echoing source lines that may contain secrets.
    error.set_input(None);
    ConfigError::Parse {
        path: path.into(),
        message: error.to_string().trim().into(),
    }
}

fn canonicalize(path: &Path) -> Result<PathBuf, ConfigError> {
    fs::canonicalize(path).map_err(|source| ConfigError::Io {
        path: path.into(),
        source,
    })
}

fn load_global(
    path: &Path,
    sources: &mut BTreeMap<String, PathBuf>,
) -> Result<GlobalConfig, ConfigError> {
    let Some(text) = read(path, true)? else {
        return Ok(GlobalConfig::default());
    };
    let main: GlobalConfig = parse(&text, path)?;
    let mut visited = BTreeSet::from([canonicalize(path)?]);
    let mut merged = toml::Value::Table(toml::Table::new());
    for include in main.includes.unwrap_or_default() {
        let include_path =
            path.parent().unwrap_or(Path::new(".")).join(include);
        if !visited.insert(canonicalize(&include_path)?) {
            return Err(ConfigError::Parse {
                path: include_path,
                message: "duplicate or cyclic global include".into(),
            });
        }
        let text = read(&include_path, false)?
            .expect("required reads cannot return absence");
        let included: GlobalConfig = parse(&text, &include_path)?;
        if included.includes.is_some() {
            return Err(ConfigError::Parse {
                path: include_path,
                message:
                    "nested includes are forbidden, including recursive cycles"
                        .into(),
            });
        }
        merge(
            &mut merged,
            parse(&text, &include_path)?,
            "",
            &include_path,
            sources,
        );
    }
    merge(&mut merged, parse(&text, path)?, "", path, sources);
    let mut global: GlobalConfig = merged
        .try_into()
        .map_err(|error| parse_error(error, path))?;
    // Include expansion belongs to this boundary; pure resolution receives data only.
    global.includes = None;
    Ok(global)
}

fn merge(
    target: &mut toml::Value,
    incoming: toml::Value,
    prefix: &str,
    source: &Path,
    sources: &mut BTreeMap<String, PathBuf>,
) {
    sources.insert(prefix.into(), source.into());
    match (target, incoming) {
        (toml::Value::Table(target), toml::Value::Table(incoming)) => {
            for (key, value) in incoming {
                let field = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                let entry = target
                    .entry(key)
                    .or_insert_with(|| toml::Value::Table(toml::Table::new()));
                merge(entry, value, &field, source, sources);
            }
        }
        (target, incoming) => *target = incoming,
    }
}

//! Locks bind portable policy, never host executable bindings or source locations.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::{
    ConfigSchemaVersion, EffectiveConfig, PermissionProfile, RoleMode,
};

/// Portable requirements describe policy demands without probing installed providers.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProviderRoleRequirement {
    mode: RoleMode,
    permission_profile: PermissionProfile,
}

type ProviderRequirements =
    BTreeMap<String, BTreeMap<String, ProviderRoleRequirement>>;

const MAX_LOCK_BYTES: usize = 1024 * 1024;

/// The versioned, committable JSON lock format.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ConfigLock {
    pub(crate) schema_version: ConfigSchemaVersion,
    pub(crate) archetype: String,
    pub(crate) coterie_version: String,
    pub(crate) providers: ProviderRequirements,
    /// SHA-256 of compact, recursively key-sorted portable JSON, encoded as lowercase hex.
    #[schemars(regex(pattern = "^[0-9a-f]{64}$"))]
    pub(crate) fingerprint: String,
}

#[derive(Clone, Copy, Debug, JsonSchema, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum LockStatus {
    Absent,
    Verified,
}

#[derive(Debug, Error)]
pub(crate) enum LockError {
    #[error("could not {action} configuration lock at {path:?}: {source}")]
    Io {
        action: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error(
        "invalid configuration lock at {path:?}: {reason}; restore the intended configuration or review it and run `coterie config lock`"
    )]
    Invalid { path: PathBuf, reason: &'static str },
    #[error(
        "configuration lock mismatch at {path:?}: {fields}; restore the locked configuration or review the configuration files and run `coterie config lock`; for coterie_version mismatches, use a compatible Coterie release"
    )]
    Mismatch { path: PathBuf, fields: String },
}

impl ConfigLock {
    pub(crate) fn for_config(config: &EffectiveConfig) -> Self {
        let mut providers = ProviderRequirements::new();
        for (name, role) in &config.archetype.roles {
            let effective = &config.roles[name];
            if effective.enabled {
                providers.entry(role.provider.clone()).or_default().insert(
                    name.clone(),
                    ProviderRoleRequirement {
                        mode: role.mode,
                        permission_profile: effective.permission_profile,
                    },
                );
            }
        }
        // An explicit projection prevents new host bindings from entering the portable contract.
        let mut portable = serde_json::json!({
            "schema_version": ConfigSchemaVersion,
            "archetype": config.archetype,
            "limits": config.limits,
            "supervision": config.supervision,
            "roles": config.roles,
            "providers": providers,
        });
        // Disabled idle shutdown is the historical policy, so old snapshots
        // and locks retain their fingerprint after migration.
        if config.supervision.idle_timeout_seconds == 0 {
            portable["supervision"]
                .as_object_mut()
                .expect("supervision object")
                .remove("idle_timeout_seconds");
        }
        let bytes = serde_json::to_vec(&portable)
            .expect("portable configuration serializes");
        let fingerprint = Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        Self {
            schema_version: ConfigSchemaVersion,
            archetype: config.archetype.reference.clone(),
            coterie_version: format!("^{}", env!("CARGO_PKG_VERSION")),
            providers,
            fingerprint,
        }
    }

    pub(crate) fn verify(&self, path: &Path) -> Result<LockStatus, LockError> {
        let Some(file) = open_existing(path)? else {
            return Ok(LockStatus::Absent);
        };
        // A lock is small metadata. Bound reads, including a concurrently growing file.
        let mut bytes = Vec::new();
        file.take(MAX_LOCK_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|source| io_error("read", path, source))?;
        if bytes.len() > MAX_LOCK_BYTES {
            return Err(invalid(path, "lock exceeds the 1 MiB limit"));
        }
        let locked: Self = serde_json::from_slice(&bytes)
            .map_err(|_| invalid(path, "expected a version 1 JSON lock with only the documented fields"))?;
        locked.compare(self, path, env!("CARGO_PKG_VERSION"))?;
        Ok(LockStatus::Verified)
    }

    fn compare(
        &self,
        expected: &Self,
        path: &Path,
        version: &str,
    ) -> Result<(), LockError> {
        let requirement = semver::VersionReq::parse(&self.coterie_version)
            .map_err(|_| {
                invalid(
                    path,
                    "coterie_version is not a semantic version requirement",
                )
            })?;
        let version =
            semver::Version::parse(version).expect("package version is valid");
        if self.fingerprint.len() != 64
            || !self
                .fingerprint
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(invalid(
                path,
                "fingerprint must contain 64 lowercase SHA-256 hex digits",
            ));
        }
        let mut fields = Vec::new();
        for (field, matches) in [
            ("archetype", self.archetype == expected.archetype),
            ("coterie_version", requirement.matches(&version)),
            ("providers", self.providers == expected.providers),
            ("fingerprint", self.fingerprint == expected.fingerprint),
        ] {
            if !matches {
                fields.push(field);
            }
        }
        if fields.is_empty() {
            Ok(())
        } else {
            Err(LockError::Mismatch {
                path: path.into(),
                fields: fields.join(", "),
            })
        }
    }

    /// Explicit replacement publishes a complete synced file without truncating the previous lock.
    pub(crate) fn write(&self, path: &Path) -> Result<(), LockError> {
        self.write_with(path, |_| Ok(()))
    }

    fn write_with(
        &self,
        path: &Path,
        boundary: impl Fn(&'static str) -> io::Result<()>,
    ) -> Result<(), LockError> {
        let mut bytes =
            serde_json::to_vec_pretty(self).expect("lock serializes");
        bytes.push(b'\n');
        if bytes.len() > MAX_LOCK_BYTES {
            return Err(invalid(path, "lock exceeds the 1 MiB limit"));
        }
        let original = open_existing(path)?
            .map(|file| file.metadata())
            .transpose()
            .map_err(|source| io_error("inspect", path, source))?;
        let parent = path.parent().expect("lock has a project directory");
        let temporary = parent
            .join(format!(".coterie.lock-{}.tmp", ulid::Ulid::generate()));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)
            .map_err(|source| {
                io_error("create temporary", &temporary, source)
            })?;
        let result = (|| {
            boundary("created")?;
            file.write_all(&bytes)?;
            boundary("written")?;
            file.sync_all()?;
            boundary("synced")?;
            let current = open_existing(path)
                .map_err(io::Error::other)?
                .map(|file| file.metadata())
                .transpose()?;
            if original.as_ref().map(identity) != current.as_ref().map(identity)
            {
                return Err(io::Error::other(
                    "lock changed during creation; inspect it and retry",
                ));
            }
            fs::rename(&temporary, path)?;
            boundary("published")?;
            File::open(parent)?.sync_all()?;
            boundary("directory-synced")
        })();
        // This attempt owns the temporary name. Never remove an existing lock during cleanup.
        let _ = fs::remove_file(&temporary);
        result.map_err(|source| io_error("write", path, source))
    }
}

fn identity(metadata: &fs::Metadata) -> (u64, u64, i64, i64, u64) {
    (
        metadata.dev(),
        metadata.ino(),
        metadata.ctime(),
        metadata.ctime_nsec(),
        metadata.len(),
    )
}

fn open_existing(path: &Path) -> Result<Option<File>, LockError> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(None);
        }
        Err(source) => return Err(io_error("inspect", path, source)),
        Ok(metadata) if !metadata.is_file() || metadata.nlink() != 1 => {
            return Err(invalid(
                path,
                "lock must be a regular file with one link",
            ));
        }
        Ok(_) => {}
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
        .open(path)
        .map_err(|source| io_error("open", path, source))?;
    let metadata = file
        .metadata()
        .map_err(|source| io_error("inspect", path, source))?;
    if !metadata.is_file() || metadata.nlink() != 1 {
        return Err(invalid(path, "lock must be a regular file with one link"));
    }
    Ok(Some(file))
}

fn invalid(path: &Path, reason: &'static str) -> LockError {
    LockError::Invalid {
        path: path.into(),
        reason,
    }
}

fn io_error(action: &'static str, path: &Path, source: io::Error) -> LockError {
    LockError::Io {
        action,
        path: path.into(),
        source,
    }
}

#[cfg(test)]
mod tests;

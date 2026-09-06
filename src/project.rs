//! Project discovery, identity, run-scoped attachment, and leases.

use std::ffi::OsString;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use git2::{ErrorCode, Repository};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::id::{ProjectId, RunId};

const ACTIVE_RUN_INDEX_VERSION: u16 = 1;

#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum ProjectIdentity {
    Git {
        #[serde(with = "path_bytes")]
        common_directory: PathBuf,
        #[serde(with = "path_bytes")]
        git_directory: PathBuf,
    },
    Directory {
        #[serde(with = "path_bytes")]
        canonical_directory: PathBuf,
    },
}

/// A stable, filesystem-safe digest of a resolved project identity.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct ProjectKey(String);

impl ProjectKey {
    #[must_use]
    pub(crate) fn for_identity(identity: &ProjectIdentity) -> Self {
        let mut digest = Sha256::new();
        match identity {
            ProjectIdentity::Git {
                common_directory,
                git_directory,
            } => {
                digest.update(b"git\0");
                hash_path(&mut digest, common_directory);
                hash_path(&mut digest, git_directory);
            }
            ProjectIdentity::Directory {
                canonical_directory,
            } => {
                digest.update(b"directory\0");
                hash_path(&mut digest, canonical_directory);
            }
        }
        let digest = digest.finalize();
        let mut encoded = String::with_capacity(digest.len() * 2);
        const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";
        for byte in digest {
            encoded.push(char::from(HEX_DIGITS[usize::from(byte >> 4)]));
            encoded.push(char::from(HEX_DIGITS[usize::from(byte & 0x0f)]));
        }
        Self(encoded)
    }

    pub(crate) fn from_hex(
        value: impl Into<String>,
    ) -> Result<Self, ProjectError> {
        let value = value.into();
        if value.len() == 64
            && value.bytes().all(|byte| {
                byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)
            })
        {
            Ok(Self(value))
        } else {
            Err(ProjectError::InvalidProjectKey { value })
        }
    }

    #[must_use]
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ProjectKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl Serialize for ProjectKey {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for ProjectKey {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::from_hex(String::deserialize(deserializer)?)
            .map_err(serde::de::Error::custom)
    }
}

fn hash_path(digest: &mut Sha256, path: &Path) {
    let bytes = path.as_os_str().as_bytes();
    let length = u64::try_from(bytes.len())
        .expect("a filesystem path cannot exceed u64::MAX bytes");
    digest.update(length.to_be_bytes());
    digest.update(bytes);
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DiscoveredProject {
    pub(crate) original_path: PathBuf,
    pub(crate) canonical_path: PathBuf,
    pub(crate) identity: ProjectIdentity,
}

impl DiscoveredProject {
    pub(crate) fn discover(
        path: impl AsRef<Path>,
    ) -> Result<Self, ProjectError> {
        let original_path = path.as_ref().to_owned();
        let canonical_input = canonicalize("project path", &original_path)?;
        if !canonical_input.is_dir() {
            return Err(ProjectError::NotDirectory {
                path: original_path,
            });
        }

        let repository = match Repository::discover(&canonical_input) {
            Ok(repository) => repository,
            Err(error) if error.code() == ErrorCode::NotFound => {
                return Ok(Self {
                    original_path,
                    canonical_path: canonical_input.clone(),
                    identity: ProjectIdentity::Directory {
                        canonical_directory: canonical_input,
                    },
                });
            }
            Err(source) => {
                return Err(ProjectError::GitDiscovery {
                    path: original_path,
                    source,
                });
            }
        };

        let worktree = repository.workdir().ok_or_else(|| {
            ProjectError::BareRepository {
                path: original_path.clone(),
            }
        })?;
        let canonical_path = canonicalize("Git worktree", worktree)?;
        let common_directory =
            canonicalize("Git common directory", repository.commondir())?;
        let git_directory =
            canonicalize("Git worktree directory", repository.path())?;

        Ok(Self {
            original_path,
            canonical_path,
            identity: ProjectIdentity::Git {
                common_directory,
                git_directory,
            },
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CoterieDirectories {
    pub(crate) runtime: PathBuf,
    pub(crate) leases: PathBuf,
    pub(crate) state: PathBuf,
    pub(crate) runs: PathBuf,
    pub(crate) projects: PathBuf,
    runtime_base: PathBuf,
}

impl CoterieDirectories {
    pub(crate) fn from_environment() -> Result<Self, ProjectError> {
        Self::from_environment_values(
            std::env::var_os("XDG_RUNTIME_DIR"),
            std::env::var_os("XDG_STATE_HOME"),
            std::env::var_os("HOME"),
        )
    }

    fn from_environment_values(
        runtime: Option<OsString>,
        state: Option<OsString>,
        home: Option<OsString>,
    ) -> Result<Self, ProjectError> {
        let runtime = runtime
            .map(PathBuf::from)
            .ok_or(ProjectError::RuntimeDirectoryUnavailable)?;
        require_absolute("XDG_RUNTIME_DIR", &runtime)?;

        let state = state
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .or_else(|| {
                home.map(PathBuf::from)
                    .filter(|path| path.is_absolute())
                    .map(|path| path.join(".local/state"))
            })
            .ok_or(ProjectError::StateDirectoryUnavailable)?;

        Self::from_base_directories(runtime, state)
    }

    fn from_base_directories(
        runtime: impl AsRef<Path>,
        state: impl AsRef<Path>,
    ) -> Result<Self, ProjectError> {
        let runtime_base = runtime.as_ref().to_owned();
        let state_base = state.as_ref().to_owned();
        require_absolute("XDG_RUNTIME_DIR", &runtime_base)?;
        require_absolute("XDG_STATE_HOME", &state_base)?;

        let runtime = runtime_base.join("coterie");
        let state = state_base.join("coterie");
        Ok(Self {
            runs: state.join("runs"),
            projects: state.join("projects"),
            leases: runtime.join("projects"),
            runtime,
            state,
            runtime_base,
        })
    }

    pub(crate) fn prepare_run(
        &self,
        run_id: RunId,
    ) -> Result<RunDirectories, ProjectError> {
        self.prepare()?;

        let state = self.runs.join(run_id.to_string());
        create_private_directory(&state)?;
        Ok(RunDirectories {
            runtime: self.runtime.clone(),
            state,
            projects: self.projects.clone(),
        })
    }

    /// Prepares the private application directories shared by all local runs.
    pub(crate) fn prepare(&self) -> Result<(), ProjectError> {
        validate_runtime_base(&self.runtime_base)?;
        for path in [
            &self.runtime,
            &self.leases,
            &self.state,
            &self.runs,
            &self.projects,
        ] {
            create_private_directory(path)?;
        }
        Ok(())
    }

    #[must_use]
    pub(crate) fn socket_path(&self, run_id: RunId) -> PathBuf {
        self.runtime.join(format!("{run_id}.sock"))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RunDirectories {
    pub(crate) runtime: PathBuf,
    pub(crate) state: PathBuf,
    pub(crate) projects: PathBuf,
}

/// The result of attempting an exclusive project lease without waiting.
pub(crate) enum LeaseAttempt {
    Acquired(ProjectLease),
    Held,
}

/// An exclusive lease retained for as long as its backing file remains open.
pub(crate) struct ProjectLease {
    _file: File,
}

impl ProjectLease {
    pub(crate) fn try_acquire(
        directories: &CoterieDirectories,
        identity: &ProjectIdentity,
        run_id: RunId,
    ) -> Result<LeaseAttempt, ProjectError> {
        let key = ProjectKey::for_identity(identity);
        let path = directories.leases.join(format!("{key}.lock"));
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(&path)
            .map_err(|source| ProjectError::LeaseIo {
                action: "open",
                path: path.clone(),
                source,
            })?;

        match file.try_lock() {
            Ok(()) => {}
            Err(fs::TryLockError::WouldBlock) => {
                return Ok(LeaseAttempt::Held);
            }
            Err(fs::TryLockError::Error(source)) => {
                return Err(ProjectError::LeaseIo {
                    action: "lock",
                    path,
                    source,
                });
            }
        }

        file.set_len(0)
            .and_then(|()| file.rewind())
            .and_then(|()| {
                writeln!(file, "{run_id}")?;
                file.sync_data()
            })
            .map_err(|source| ProjectError::LeaseIo {
                action: "record owner in",
                path: path.clone(),
                source,
            })?;

        Ok(LeaseAttempt::Acquired(Self { _file: file }))
    }
}

/// Disposable lookup metadata for the run that owns one project identity.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ActiveRunEntry {
    pub(crate) schema_version: u16,
    pub(crate) run_id: RunId,
    pub(crate) project_id: ProjectId,
    pub(crate) project_key: ProjectKey,
    pub(crate) project_identity: ProjectIdentity,
}

impl ActiveRunEntry {
    #[must_use]
    pub(crate) fn new(
        run_id: RunId,
        project_id: ProjectId,
        project_identity: ProjectIdentity,
    ) -> Self {
        let project_key = ProjectKey::for_identity(&project_identity);
        Self {
            schema_version: ACTIVE_RUN_INDEX_VERSION,
            run_id,
            project_id,
            project_key,
            project_identity,
        }
    }
}

/// Reads and atomically updates the disposable active-run project index.
pub(crate) struct ActiveRunIndex {
    directory: PathBuf,
}

impl ActiveRunIndex {
    #[must_use]
    pub(crate) fn new(directories: &CoterieDirectories) -> Self {
        Self {
            directory: directories.projects.clone(),
        }
    }

    pub(crate) fn lookup(
        &self,
        identity: &ProjectIdentity,
    ) -> Result<Option<ActiveRunEntry>, ProjectError> {
        let key = ProjectKey::for_identity(identity);
        let path = self.entry_path(&key);
        let mut file = match File::open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(None);
            }
            Err(source) => {
                return Err(ProjectError::IndexIo {
                    action: "open",
                    path,
                    source,
                });
            }
        };
        let mut encoded = Vec::new();
        file.read_to_end(&mut encoded).map_err(|source| {
            ProjectError::IndexIo {
                action: "read",
                path: path.clone(),
                source,
            }
        })?;
        let entry = serde_json::from_slice::<ActiveRunEntry>(&encoded)
            .map_err(|source| ProjectError::IndexJson {
                path: path.clone(),
                source,
            })?;
        if entry.schema_version != ACTIVE_RUN_INDEX_VERSION {
            return Err(ProjectError::UnsupportedIndexVersion {
                path,
                found: entry.schema_version,
                supported: ACTIVE_RUN_INDEX_VERSION,
            });
        }
        if entry.project_key != key || entry.project_identity != *identity {
            return Err(ProjectError::IndexIdentityMismatch { path });
        }
        Ok(Some(entry))
    }

    pub(crate) fn publish(
        &self,
        entry: &ActiveRunEntry,
    ) -> Result<(), ProjectError> {
        let expected_key = ProjectKey::for_identity(&entry.project_identity);
        if entry.schema_version != ACTIVE_RUN_INDEX_VERSION
            || entry.project_key != expected_key
        {
            return Err(ProjectError::InvalidIndexEntry);
        }

        let path = self.entry_path(&entry.project_key);
        let temporary = self.directory.join(format!(
            ".{}.{}.{}.tmp",
            entry.project_key,
            entry.run_id,
            RunId::generate()
        ));
        let result = (|| {
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .mode(0o600)
                .open(&temporary)
                .map_err(|source| ProjectError::IndexIo {
                    action: "create temporary",
                    path: temporary.clone(),
                    source,
                })?;
            serde_json::to_writer(&mut file, entry).map_err(|source| {
                ProjectError::EncodeIndex {
                    path: temporary.clone(),
                    source,
                }
            })?;
            file.write_all(b"\n")
                .and_then(|()| file.sync_all())
                .map_err(|source| ProjectError::IndexIo {
                    action: "persist temporary",
                    path: temporary.clone(),
                    source,
                })?;
            fs::rename(&temporary, &path).map_err(|source| {
                ProjectError::IndexIo {
                    action: "publish",
                    path: path.clone(),
                    source,
                }
            })?;
            sync_directory(&self.directory)
        })();

        if result.is_err() {
            let _ignored = fs::remove_file(&temporary);
        }
        result
    }

    pub(crate) fn retire(
        &self,
        identity: &ProjectIdentity,
        run_id: RunId,
    ) -> Result<(), ProjectError> {
        let Some(entry) = self.lookup(identity)? else {
            return Ok(());
        };
        if entry.run_id != run_id {
            return Ok(());
        }
        let path = self.entry_path(&entry.project_key);
        match fs::remove_file(&path) {
            Ok(()) => sync_directory(&self.directory),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(ProjectError::IndexIo {
                action: "retire",
                path,
                source,
            }),
        }
    }

    fn entry_path(&self, key: &ProjectKey) -> PathBuf {
        self.directory.join(format!("{key}.json"))
    }
}

#[derive(Debug, Error)]
pub(crate) enum ProjectError {
    #[error("could not canonicalize {purpose} at {path:?}: {source}")]
    Canonicalize {
        purpose: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("project path is not a directory: {path:?}")]
    NotDirectory { path: PathBuf },
    #[error("could not discover a Git repository from {path:?}: {source}")]
    GitDiscovery {
        path: PathBuf,
        #[source]
        source: git2::Error,
    },
    #[error("bare Git repositories are not projects: {path:?}")]
    BareRepository { path: PathBuf },
    #[error("XDG_RUNTIME_DIR is unavailable")]
    RuntimeDirectoryUnavailable,
    #[error("neither an absolute XDG_STATE_HOME nor HOME is available")]
    StateDirectoryUnavailable,
    #[error("{variable} must be an absolute path, but was {path:?}")]
    BaseDirectoryNotAbsolute {
        variable: &'static str,
        path: PathBuf,
    },
    #[error("could not inspect XDG_RUNTIME_DIR at {path:?}: {source}")]
    RuntimeDirectoryMetadata {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("XDG_RUNTIME_DIR is not a directory: {path:?}")]
    RuntimePathNotDirectory { path: PathBuf },
    #[error(
        "XDG_RUNTIME_DIR at {path:?} must have mode 0700, but has mode {mode:04o}"
    )]
    InsecureRuntimeDirectory { path: PathBuf, mode: u32 },
    #[error(
        "refusing to use a symlink or non-directory for private data: {path:?}"
    )]
    UnsafeDirectory { path: PathBuf },
    #[error("could not {action} private directory at {path:?}: {source}")]
    DirectoryIo {
        action: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("invalid project identity key `{value}`")]
    InvalidProjectKey { value: String },
    #[error("could not {action} project lease at {path:?}: {source}")]
    LeaseIo {
        action: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("could not {action} active-run index at {path:?}: {source}")]
    IndexIo {
        action: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("could not encode active-run index at {path:?}: {source}")]
    EncodeIndex {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("could not decode active-run index at {path:?}: {source}")]
    IndexJson {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error(
        "active-run index at {path:?} has version {found}, but version {supported} is supported"
    )]
    UnsupportedIndexVersion {
        path: PathBuf,
        found: u16,
        supported: u16,
    },
    #[error("active-run index at {path:?} does not match its project identity")]
    IndexIdentityMismatch { path: PathBuf },
    #[error("active-run index entry is internally inconsistent")]
    InvalidIndexEntry,
}

fn sync_directory(path: &Path) -> Result<(), ProjectError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| ProjectError::IndexIo {
            action: "synchronize",
            path: path.to_owned(),
            source,
        })
}

fn canonicalize(
    purpose: &'static str,
    path: &Path,
) -> Result<PathBuf, ProjectError> {
    fs::canonicalize(path).map_err(|source| ProjectError::Canonicalize {
        purpose,
        path: path.to_owned(),
        source,
    })
}

fn require_absolute(
    variable: &'static str,
    path: &Path,
) -> Result<(), ProjectError> {
    if path.is_absolute() {
        Ok(())
    } else {
        Err(ProjectError::BaseDirectoryNotAbsolute {
            variable,
            path: path.to_owned(),
        })
    }
}

fn validate_runtime_base(path: &Path) -> Result<(), ProjectError> {
    let metadata = fs::metadata(path).map_err(|source| {
        ProjectError::RuntimeDirectoryMetadata {
            path: path.to_owned(),
            source,
        }
    })?;
    if !metadata.is_dir() {
        return Err(ProjectError::RuntimePathNotDirectory {
            path: path.to_owned(),
        });
    }
    let mode = metadata.permissions().mode() & 0o777;
    if mode != 0o700 {
        return Err(ProjectError::InsecureRuntimeDirectory {
            path: path.to_owned(),
            mode,
        });
    }
    Ok(())
}

fn create_private_directory(path: &Path) -> Result<(), ProjectError> {
    match fs::symlink_metadata(path) {
        Ok(metadata)
            if metadata.file_type().is_symlink() || !metadata.is_dir() =>
        {
            return Err(ProjectError::UnsafeDirectory {
                path: path.to_owned(),
            });
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(path)
                .map_err(|source| ProjectError::DirectoryIo {
                    action: "create",
                    path: path.to_owned(),
                    source,
                })?;
        }
        Err(source) => {
            return Err(ProjectError::DirectoryIo {
                action: "inspect",
                path: path.to_owned(),
                source,
            });
        }
    }

    let metadata = fs::symlink_metadata(path).map_err(|source| {
        ProjectError::DirectoryIo {
            action: "inspect",
            path: path.to_owned(),
            source,
        }
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(ProjectError::UnsafeDirectory {
            path: path.to_owned(),
        });
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(
        |source| ProjectError::DirectoryIo {
            action: "secure",
            path: path.to_owned(),
            source,
        },
    )
}

mod path_bytes {
    use std::ffi::OsString;
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    use std::path::{Path, PathBuf};

    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub(super) fn serialize<S>(
        path: &Path,
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        path.as_os_str().as_bytes().serialize(serializer)
    }

    pub(super) fn deserialize<'de, D>(
        deserializer: D,
    ) -> Result<PathBuf, D::Error>
    where
        D: Deserializer<'de>,
    {
        let bytes = Vec::<u8>::deserialize(deserializer)?;
        Ok(PathBuf::from(OsString::from_vec(bytes)))
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::fs;
    use std::os::unix::ffi::OsStringExt;
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt, symlink};
    use std::path::{Path, PathBuf};

    use git2::{Repository, Signature, WorktreeAddOptions};

    use super::{
        ActiveRunEntry, ActiveRunIndex, CoterieDirectories, DiscoveredProject,
        LeaseAttempt, ProjectError, ProjectIdentity, ProjectKey, ProjectLease,
    };
    use crate::id::{ProjectId, RunId};

    const RUN_ID: &str = "cr-01ARZ3NDEKTSV4RRFFQ69G5FAV";

    #[test]
    fn discovers_the_canonical_root_from_inside_a_git_worktree() {
        let fixture = TestDirectory::new();
        let root = fixture.join("repository");
        let repository = initialize_repository(&root);
        let nested = root.join("nested/directory");
        fs::create_dir_all(&nested).expect("the nested directory should exist");

        let project = DiscoveredProject::discover(&nested)
            .expect("the Git project should be discovered");

        assert_eq!(project.original_path, nested);
        assert_eq!(
            project.canonical_path,
            fs::canonicalize(&root).expect("the root should canonicalize")
        );
        assert_eq!(
            project.identity,
            ProjectIdentity::Git {
                common_directory: fs::canonicalize(repository.commondir())
                    .expect("the common directory should canonicalize"),
                git_directory: fs::canonicalize(repository.path())
                    .expect("the Git directory should canonicalize"),
            }
        );
    }

    #[test]
    fn symlinked_non_git_project_keeps_the_original_and_resolved_paths() {
        let fixture = TestDirectory::new();
        let target = fixture.join("actual project");
        let alias = fixture.join("project-link");
        fs::create_dir(&target).expect("the project directory should exist");
        symlink(&target, &alias)
            .expect("the project symlink should be created");

        let project = DiscoveredProject::discover(&alias)
            .expect("the directory project should be discovered");
        let canonical =
            fs::canonicalize(&target).expect("the target should canonicalize");

        assert_eq!(project.original_path, alias);
        assert_eq!(project.canonical_path, canonical);
        assert_eq!(
            project.identity,
            ProjectIdentity::Directory {
                canonical_directory: canonical,
            }
        );
    }

    #[test]
    fn linked_worktrees_have_distinct_identities_in_one_common_repository() {
        let fixture = TestDirectory::new();
        let primary_path = fixture.join("primary");
        let linked_path = fixture.join("linked");
        let repository = initialize_repository(&primary_path);
        create_initial_commit(&repository, &primary_path);
        let commit = repository
            .head()
            .and_then(|head| head.peel_to_commit())
            .expect("HEAD should resolve to the initial commit");
        repository
            .branch("linked", &commit, false)
            .expect("the linked-worktree branch should be created");
        let reference = repository
            .find_reference("refs/heads/linked")
            .expect("the linked-worktree branch should resolve");
        let mut options = WorktreeAddOptions::new();
        options.reference(Some(&reference));
        repository
            .worktree("linked", &linked_path, Some(&options))
            .expect("the linked worktree should be created");

        let primary = DiscoveredProject::discover(&primary_path)
            .expect("the primary worktree should be discovered");
        let linked = DiscoveredProject::discover(&linked_path)
            .expect("the linked worktree should be discovered");

        assert_eq!(
            primary.canonical_path,
            fs::canonicalize(&primary_path).unwrap()
        );
        assert_eq!(
            linked.canonical_path,
            fs::canonicalize(&linked_path).unwrap()
        );
        let (
            ProjectIdentity::Git {
                common_directory: primary_common,
                git_directory: primary_git,
            },
            ProjectIdentity::Git {
                common_directory: linked_common,
                git_directory: linked_git,
            },
        ) = (&primary.identity, &linked.identity)
        else {
            panic!("both projects should have Git identities");
        };
        assert_eq!(primary_common, linked_common);
        assert_ne!(primary_git, linked_git);
        assert_ne!(primary.identity, linked.identity);
    }

    #[test]
    fn rejects_a_bare_repository_as_a_project() {
        let fixture = TestDirectory::new();
        let bare = fixture.join("bare.git");
        Repository::init_bare(&bare)
            .expect("the bare repository should initialize");

        let error = DiscoveredProject::discover(&bare)
            .expect_err("a project must have a working directory");

        assert!(matches!(error, ProjectError::BareRepository { .. }));
    }

    #[test]
    fn rejects_missing_paths_and_regular_files() {
        let fixture = TestDirectory::new();
        let missing = fixture.join("missing");
        let file = fixture.join("file");
        fs::write(&file, "not a project")
            .expect("the fixture file should be written");

        assert!(matches!(
            DiscoveredProject::discover(&missing),
            Err(ProjectError::Canonicalize { .. })
        ));
        assert!(matches!(
            DiscoveredProject::discover(&file),
            Err(ProjectError::NotDirectory { .. })
        ));
    }

    #[test]
    fn identity_json_preserves_non_utf8_directory_paths() {
        let fixture = TestDirectory::new();
        let directory =
            fixture.join(OsString::from_vec(b"project-\xff".to_vec()));
        fs::create_dir(&directory)
            .expect("the non-UTF-8 directory should exist");
        let project = DiscoveredProject::discover(&directory)
            .expect("the directory project should be discovered");

        let encoded = serde_json::to_value(&project.identity)
            .expect("the identity should serialize losslessly");
        let decoded: ProjectIdentity = serde_json::from_value(encoded)
            .expect("the identity should deserialize losslessly");

        assert_eq!(decoded, project.identity);
    }

    #[test]
    fn project_identity_keys_have_a_stable_filesystem_contract() {
        let identity = ProjectIdentity::Directory {
            canonical_directory: PathBuf::from("/tmp/project"),
        };

        assert_eq!(
            ProjectKey::for_identity(&identity).as_str(),
            "15da66ddfb38f372717bfd9f46426f598dcc6baac5f1e895211ccbef64d2c58e"
        );
    }

    #[test]
    fn resolves_xdg_directories_and_ignores_relative_state_home() {
        let explicit = CoterieDirectories::from_environment_values(
            Some(OsString::from("/run/user/1000")),
            Some(OsString::from("/var/lib/user-state")),
            Some(OsString::from("/home/alice")),
        )
        .expect("absolute XDG directories should resolve");
        assert_eq!(explicit.runtime, Path::new("/run/user/1000/coterie"));
        assert_eq!(explicit.state, Path::new("/var/lib/user-state/coterie"));
        assert_eq!(
            explicit.runs,
            Path::new("/var/lib/user-state/coterie/runs")
        );
        assert_eq!(
            explicit.projects,
            Path::new("/var/lib/user-state/coterie/projects")
        );

        let fallback = CoterieDirectories::from_environment_values(
            Some(OsString::from("/run/user/1000")),
            Some(OsString::from("relative-state")),
            Some(OsString::from("/home/alice")),
        )
        .expect("a relative state home should fall back to HOME");
        assert_eq!(
            fallback.state,
            Path::new("/home/alice/.local/state/coterie")
        );
    }

    #[test]
    fn requires_absolute_runtime_and_resolvable_state_directories() {
        assert!(matches!(
            CoterieDirectories::from_environment_values(
                None,
                Some(OsString::from("/state")),
                None,
            ),
            Err(ProjectError::RuntimeDirectoryUnavailable)
        ));
        assert!(matches!(
            CoterieDirectories::from_environment_values(
                Some(OsString::from("runtime")),
                Some(OsString::from("/state")),
                None,
            ),
            Err(ProjectError::BaseDirectoryNotAbsolute {
                variable: "XDG_RUNTIME_DIR",
                ..
            })
        ));
        assert!(matches!(
            CoterieDirectories::from_environment_values(
                Some(OsString::from("/runtime")),
                None,
                None,
            ),
            Err(ProjectError::StateDirectoryUnavailable)
        ));
    }

    #[test]
    fn prepares_private_runtime_state_run_and_project_index_directories() {
        let fixture = TestDirectory::new();
        let runtime_base = fixture.join("runtime");
        let state_base = fixture.join("state");
        create_directory(&runtime_base, 0o700);
        let directories = CoterieDirectories::from_base_directories(
            &runtime_base,
            &state_base,
        )
        .expect("the base directories should resolve");
        let run_id = RUN_ID.parse::<RunId>().expect("the run ID should parse");

        let run = directories
            .prepare_run(run_id)
            .expect("the run directories should be prepared");

        assert_eq!(run.runtime, runtime_base.join("coterie"));
        assert_eq!(run.state, state_base.join("coterie/runs").join(RUN_ID));
        assert_eq!(run.projects, state_base.join("coterie/projects"));
        for path in [
            &directories.runtime,
            &directories.state,
            &directories.runs,
            &directories.projects,
            &run.state,
        ] {
            assert!(path.is_dir(), "{} should be a directory", path.display());
            assert_eq!(
                mode(path),
                0o700,
                "{} should be private",
                path.display()
            );
        }

        fs::set_permissions(
            &directories.state,
            fs::Permissions::from_mode(0o755),
        )
        .expect("the state permissions should be changed for the fixture");
        let repeated = directories
            .prepare_run(run_id)
            .expect("preparing an existing run should be idempotent");
        assert_eq!(repeated, run);
        assert_eq!(mode(&directories.state), 0o700);
    }

    #[test]
    fn rejects_an_insecure_xdg_runtime_directory() {
        let fixture = TestDirectory::new();
        let runtime_base = fixture.join("runtime");
        create_directory(&runtime_base, 0o755);
        let directories = CoterieDirectories::from_base_directories(
            &runtime_base,
            fixture.join("state"),
        )
        .expect("the paths should resolve before they are prepared");

        let error = directories
            .prepare_run(RUN_ID.parse().expect("the run ID should parse"))
            .expect_err("an insecure runtime base must be rejected");

        assert!(matches!(
            error,
            ProjectError::InsecureRuntimeDirectory { mode: 0o755, .. }
        ));
    }

    #[test]
    fn refuses_to_place_state_through_an_application_directory_symlink() {
        let fixture = TestDirectory::new();
        let runtime_base = fixture.join("runtime");
        let state_base = fixture.join("state");
        let redirected = fixture.join("redirected");
        create_directory(&runtime_base, 0o700);
        fs::create_dir(&state_base).expect("the state base should exist");
        fs::create_dir(&redirected).expect("the redirect target should exist");
        symlink(&redirected, state_base.join("coterie"))
            .expect("the application state symlink should exist");
        let directories = CoterieDirectories::from_base_directories(
            &runtime_base,
            &state_base,
        )
        .expect("the base directories should resolve");

        let error = directories
            .prepare_run(RUN_ID.parse().expect("the run ID should parse"))
            .expect_err("application state must not follow a symlink");

        assert!(matches!(error, ProjectError::UnsafeDirectory { .. }));
        assert!(
            fs::read_dir(&redirected)
                .expect("the redirect target should be readable")
                .next()
                .is_none(),
            "nothing should be placed through the symlink"
        );
    }

    #[test]
    fn project_leases_are_nonblocking_and_identity_scoped() {
        let fixture = TestDirectory::new();
        let runtime_base = fixture.join("runtime");
        create_directory(&runtime_base, 0o700);
        let directories = CoterieDirectories::from_base_directories(
            &runtime_base,
            fixture.join("state"),
        )
        .expect("the paths should resolve");
        directories
            .prepare()
            .expect("the private directories should be prepared");
        let first = ProjectIdentity::Directory {
            canonical_directory: fixture.join("first"),
        };
        let second = ProjectIdentity::Directory {
            canonical_directory: fixture.join("second"),
        };
        let run_id = RUN_ID.parse::<RunId>().expect("valid run ID");

        let first_lease =
            match ProjectLease::try_acquire(&directories, &first, run_id)
                .expect("the first lease attempt should complete")
            {
                LeaseAttempt::Acquired(lease) => lease,
                LeaseAttempt::Held => {
                    panic!("the first lease should be available")
                }
            };
        assert!(matches!(
            ProjectLease::try_acquire(&directories, &first, RunId::generate())
                .expect("contention should be reported without blocking"),
            LeaseAttempt::Held
        ));
        let lease_path = directories
            .leases
            .join(format!("{}.lock", ProjectKey::for_identity(&first)));
        assert_eq!(
            fs::read_to_string(lease_path)
                .expect("the owner record should remain readable"),
            format!("{run_id}\n"),
            "a losing contender must not truncate the current owner record"
        );
        assert!(matches!(
            ProjectLease::try_acquire(&directories, &second, run_id)
                .expect("a different identity should not contend"),
            LeaseAttempt::Acquired(_)
        ));

        drop(first_lease);
        assert!(matches!(
            ProjectLease::try_acquire(&directories, &first, run_id)
                .expect("a released lease should be acquirable"),
            LeaseAttempt::Acquired(_)
        ));
    }

    #[test]
    fn active_run_index_is_identity_checked_and_retired_only_by_its_owner() {
        let fixture = TestDirectory::new();
        let runtime_base = fixture.join("runtime");
        create_directory(&runtime_base, 0o700);
        let directories = CoterieDirectories::from_base_directories(
            &runtime_base,
            fixture.join("state"),
        )
        .expect("the paths should resolve");
        directories
            .prepare()
            .expect("the private directories should be prepared");
        let identity = ProjectIdentity::Directory {
            canonical_directory: fixture.join("project"),
        };
        let entry = ActiveRunEntry {
            schema_version: 1,
            run_id: RUN_ID.parse().expect("valid run ID"),
            project_id: "cp-01ARZ3NDEKTSV4RRFFQ69G5FAW"
                .parse::<ProjectId>()
                .expect("valid project ID"),
            project_key: ProjectKey::for_identity(&identity),
            project_identity: identity.clone(),
        };
        let index = ActiveRunIndex::new(&directories);

        index
            .publish(&entry)
            .expect("the entry should be published");
        assert_eq!(
            index
                .lookup(&identity)
                .expect("the entry should be readable"),
            Some(entry.clone())
        );

        index
            .retire(&identity, RunId::generate())
            .expect("a non-owner retirement should be harmless");
        assert_eq!(
            index.lookup(&identity).expect("the entry should remain"),
            Some(entry.clone())
        );

        index
            .retire(&identity, entry.run_id)
            .expect("the owner should retire its entry");
        assert_eq!(
            index.lookup(&identity).expect("the lookup should succeed"),
            None
        );
    }

    #[test]
    fn active_run_index_rejects_content_for_another_identity() {
        let fixture = TestDirectory::new();
        let runtime_base = fixture.join("runtime");
        create_directory(&runtime_base, 0o700);
        let directories = CoterieDirectories::from_base_directories(
            &runtime_base,
            fixture.join("state"),
        )
        .expect("the paths should resolve");
        directories
            .prepare()
            .expect("the private directories should be prepared");
        let expected = ProjectIdentity::Directory {
            canonical_directory: fixture.join("expected"),
        };
        let key = ProjectKey::for_identity(&expected);
        let inconsistent = ActiveRunEntry {
            schema_version: 1,
            run_id: RUN_ID.parse().expect("valid run ID"),
            project_id: "cp-01ARZ3NDEKTSV4RRFFQ69G5FAW"
                .parse()
                .expect("valid project ID"),
            project_key: key.clone(),
            project_identity: ProjectIdentity::Directory {
                canonical_directory: fixture.join("different"),
            },
        };
        let path = directories.projects.join(format!("{key}.json"));
        fs::write(
            &path,
            serde_json::to_vec(&inconsistent)
                .expect("the inconsistent fixture should encode"),
        )
        .expect("the fixture index should be written");

        assert!(matches!(
            ActiveRunIndex::new(&directories).lookup(&expected),
            Err(ProjectError::IndexIdentityMismatch { .. })
        ));
    }

    fn initialize_repository(path: &Path) -> Repository {
        fs::create_dir_all(path)
            .expect("the repository directory should exist");
        Repository::init(path).expect("the repository should initialize")
    }

    fn create_initial_commit(repository: &Repository, worktree: &Path) {
        fs::write(worktree.join("README.md"), "fixture\n")
            .expect("the fixture file should be written");
        let mut index = repository.index().expect("the index should open");
        index
            .add_path(Path::new("README.md"))
            .expect("the fixture should be added to the index");
        let tree_id = index.write_tree().expect("the tree should be written");
        let tree = repository
            .find_tree(tree_id)
            .expect("the written tree should resolve");
        let signature = Signature::now("Coterie Test", "test@example.invalid")
            .expect("the test signature should be valid");
        repository
            .commit(
                Some("HEAD"),
                &signature,
                &signature,
                "Initial commit",
                &tree,
                &[],
            )
            .expect("the initial commit should be created");
    }

    fn create_directory(path: &Path, mode: u32) {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(mode)
            .create(path)
            .expect("the directory should be created");
        fs::set_permissions(path, fs::Permissions::from_mode(mode))
            .expect("the directory permissions should be set");
    }

    fn mode(path: &Path) -> u32 {
        fs::metadata(path)
            .expect("the directory metadata should be available")
            .permissions()
            .mode()
            & 0o777
    }

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let path = std::env::temp_dir()
                .join(format!("coterie-project-test-{}", RunId::generate()));
            fs::DirBuilder::new()
                .mode(0o700)
                .create(&path)
                .expect("the test directory should be created");
            Self(path)
        }

        fn join(&self, path: impl AsRef<Path>) -> PathBuf {
            self.0.join(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            if self.0.exists() {
                fs::remove_dir_all(&self.0)
                    .expect("the test directory should be removable");
            }
        }
    }
}

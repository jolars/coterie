//! Read-only recovery observations behind the Git workspace boundary.

use std::collections::BTreeSet;

use super::*;
use crate::protocol::recovery::{RecoveryPath, RecoverySnapshot};

impl GitWorkspace {
    pub(super) fn inspect_recovery(
        &self,
        workspace: &WorkspaceRecord,
        project: &ProjectRecord,
    ) -> Result<RecoverySnapshot, WorkspaceBackendError> {
        Self::validate_identity(workspace, project)?;
        self.validate_path(workspace, project)?;
        if workspace.kind != WORKTREE_WORKSPACE {
            return Err(WorkspaceBackendError::UnsupportedKind {
                kind: workspace.kind.clone(),
            });
        }
        let repository = self.owned_worktree_repository(workspace, project)?;
        let git_error = |source| WorkspaceBackendError::Git {
            action: "inspect the preserved worktree for recovery",
            path: workspace.path.clone(),
            source,
        };
        let head_commit = repository
            .head()
            .and_then(|head| head.peel_to_commit())
            .map_err(git_error)?
            .id()
            .to_string();
        let index = repository.index().map_err(git_error)?;
        let hidden = index
            .iter()
            .filter(|entry| {
                entry.flags & git2::IndexEntryFlag::VALID.bits() != 0
                    || entry.flags_extended
                        & git2::IndexEntryExtendedFlag::SKIP_WORKTREE.bits()
                        != 0
            })
            .map(|entry| entry.path)
            .collect();
        let mut options = StatusOptions::new();
        options
            .include_untracked(true)
            .recurse_untracked_dirs(true)
            .include_unreadable(true)
            .include_ignored(false)
            // Recovery must preserve even index stat-cache bytes.
            .update_index(false);
        let statuses =
            repository.statuses(Some(&mut options)).map_err(git_error)?;
        let mut dirty = BTreeSet::new();
        let mut staged = BTreeSet::new();
        let mut unstaged = BTreeSet::new();
        let mut untracked = BTreeSet::new();
        let mut conflicted = BTreeSet::new();
        let mut unreadable = BTreeSet::new();
        for entry in statuses.iter() {
            let status = entry.status();
            let staged_paths =
                delta_paths(entry.head_to_index(), entry.path_bytes());
            let workdir_paths =
                delta_paths(entry.index_to_workdir(), entry.path_bytes());
            for (flags, destination, paths) in [
                (
                    Status::INDEX_NEW
                        | Status::INDEX_MODIFIED
                        | Status::INDEX_DELETED
                        | Status::INDEX_RENAMED
                        | Status::INDEX_TYPECHANGE,
                    &mut staged,
                    &staged_paths,
                ),
                (
                    Status::WT_MODIFIED
                        | Status::WT_DELETED
                        | Status::WT_RENAMED
                        | Status::WT_TYPECHANGE,
                    &mut unstaged,
                    &workdir_paths,
                ),
                (Status::WT_NEW, &mut untracked, &workdir_paths),
                (Status::CONFLICTED, &mut conflicted, &workdir_paths),
                (Status::WT_UNREADABLE, &mut unreadable, &workdir_paths),
            ] {
                if status.intersects(flags) {
                    destination.extend(paths.iter().cloned());
                    dirty.extend(paths.iter().cloned());
                }
            }
        }
        let hidden_index_paths = native_paths(hidden);
        Ok(RecoverySnapshot {
            head_commit,
            operation_in_progress: repository.state() != RepositoryState::Clean,
            complete: hidden_index_paths.is_empty() && unreadable.is_empty(),
            dirty_paths: native_paths(dirty),
            staged_paths: native_paths(staged),
            unstaged_paths: native_paths(unstaged),
            untracked_paths: native_paths(untracked),
            conflicted_paths: native_paths(conflicted),
            unreadable_paths: native_paths(unreadable),
            hidden_index_paths,
        })
    }
}

fn delta_paths(
    delta: Option<git2::DiffDelta<'_>>,
    fallback: &[u8],
) -> BTreeSet<Vec<u8>> {
    let mut paths: BTreeSet<_> = delta
        .into_iter()
        .flat_map(|delta| [delta.old_file(), delta.new_file()])
        .filter_map(|file| file.path_bytes().map(Vec::from))
        .collect();
    if paths.is_empty() {
        paths.insert(fallback.to_vec());
    }
    paths
}

fn native_paths(paths: BTreeSet<Vec<u8>>) -> Vec<RecoveryPath> {
    paths
        .into_iter()
        .map(|path_bytes| RecoveryPath {
            path: String::from_utf8_lossy(&path_bytes).into_owned(),
            path_bytes,
        })
        .collect()
}

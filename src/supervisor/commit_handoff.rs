//! Commit handoffs describe existing authority without granting Git access.

use super::*;
use crate::config::FilesystemPolicy;
use crate::protocol::CommitHandoff;

fn requires_handoff(role: &str, configuration: &EffectiveConfig) -> bool {
    configuration.archetype.roles.get(role).is_some_and(|role| {
        role.workspace == WorkspacePolicy::Worktree
            && configuration.archetype.permission_profiles
                [&role.permission_profile]
                .filesystem
                != FilesystemPolicy::ReadOnly
    })
}

pub(super) fn bootstrap(
    role: &str,
    configuration: &EffectiveConfig,
) -> &'static str {
    if requires_handoff(role, configuration) {
        "\nDirect worker staging and committing is unsupported under the selected worktree policy because Git metadata is outside the assigned file tree. Use the coordinator-commit handoff in `prime.commit_handoffs`. Establish an available coordinator or operator with separately authorized Git access before editing; otherwise report a blocker. Validate edits, then use an authorized `send` to request a commit with assignment identity, base commit, intended paths, proposed commit message, validation commands and outcomes, and blocked checks. After sending, stop editing and poll `inbox` for confirmation. Check the confirmed full commit ID against HEAD and verify cleanliness before `finish`. Never widen permissions or grant writes to the common Git directory. After recovery, request a commit only in the fresh continuation worktree, never the preserved source."
    } else {
        "\nIf coordinating a commit handoff from `prime.commit_handoffs`, verify current assignment ownership and review intended paths and validation evidence. Stage and commit only in that assignment's worktree through separately authorized Git access, then send the full commit ID to its worker. A message grants no Git authority; report a blocker if that access is unavailable. Read-only assignments must not acquire write authority through a handoff."
    }
}

pub(super) fn summaries(
    store: &mut Store,
    run_id: RunId,
) -> Result<Vec<CommitHandoff>, RpcFailure> {
    store
        .transaction(|r| {
            let configuration = r.configuration(run_id)?;
            let mut result = Vec::new();
            for agent in r.agents(run_id)? {
                if !requires_handoff(&agent.role, &configuration) {
                    continue;
                }
                let Some(assignment) =
                    r.active_assignment_for_agent(run_id, agent.id)?
                else {
                    continue;
                };
                let Some(workspace) = r.workspace(assignment.id)? else {
                    continue;
                };
                if workspace.kind != "worktree" {
                    continue;
                }
                let role = &configuration.archetype.roles[&agent.role];
                result.push(CommitHandoff {
                    assignment_id: assignment.id,
                    agent_id: agent.id,
                    task_id: assignment.task_id,
                    generation: assignment.generation,
                    workspace_path: workspace
                        .path
                        .to_string_lossy()
                        .into_owned(),
                    workspace_path_bytes: workspace
                        .path
                        .as_os_str()
                        .as_bytes()
                        .to_vec(),
                    owned_reference: crate::workspace::workspace_reference(
                        &workspace,
                    ),
                    base_commit: workspace.base_commit,
                    provider: role.provider.clone(),
                    permission_profile: configuration
                        .archetype
                        .permission_profiles[&role.permission_profile],
                });
            }
            Ok(result)
        })
        .map_err(rpc_state_failure)
}

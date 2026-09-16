//! Compact current context and full, revision-checked detail documents.

use sha2::{Digest, Sha256};

use super::*;
use crate::protocol::context::{
    AssignmentBrief, AssignmentDetail, CONTEXT_BUDGET, DetailPage, NextAction,
    RecoveryBrief, TaskBrief, TaskContext, TaskDetail, TextPreview,
    WorkspaceDetail,
};

pub(super) fn prime(
    store: &mut Store,
    run_id: RunId,
    caller: &AuthenticatedCaller,
    after_task: Option<TaskId>,
    limit: u16,
) -> Result<RpcResponse, RpcFailure> {
    if !(1..=50).contains(&limit) {
        return Err(invalid_argument("prime limit must be between 1 and 50"));
    }
    if let Some(id) = after_task {
        task_by_id(store, run_id, id)?;
    }
    let identity = caller_summary(store, run_id, caller)?;
    let (projects, agents, ids, current_assignment) = store
        .transaction(|r| {
            Ok((
                r.projects(run_id)?,
                r.agents(run_id)?,
                r.task_page_ids(run_id, after_task, limit + 1)?,
                caller
                    .agent_id()
                    .map(|id| r.latest_assignment_for_agent(run_id, id))
                    .transpose()?
                    .flatten(),
            ))
        })
        .map_err(rpc_state_failure)?;
    let peers = summarize_agents(
        &agents,
        &store
            .configuration(run_id)
            .map_err(rpc_state_failure)?
            .archetype
            .lead,
    )
    .into_iter()
    .filter(|agent| Some(agent.id) != caller.agent_id())
    .collect();
    let mut context = TaskContext::default();
    if let Some(assignment) = current_assignment {
        let (task, recovery) = brief(store, run_id, assignment.task_id)?;
        if assignment.completed_at.is_none() {
            context.active_task = Some(Box::new(task.clone()));
        }
        context.current_task = Some(Box::new(task));
        context.recoveries.extend(recovery);
    }
    context.has_more = ids.len() > usize::from(limit);
    for id in ids.into_iter().take(usize::from(limit)) {
        let (task, recovery) = brief(store, run_id, id)?;
        let mut candidate = context.clone();
        if task.ready {
            candidate.ready_tasks.push(task.id);
        }
        candidate.tasks.push(task);
        if let Some(recovery) = recovery
            && !candidate
                .recoveries
                .iter()
                .any(|r| r.assignment_id == recovery.assignment_id)
        {
            candidate.recoveries.push(recovery);
        }
        candidate.next_task = Some(id);
        if serde_json::to_vec(&candidate)
            .map_err(serialization_failure)?
            .len()
            > CONTEXT_BUDGET
        {
            context.has_more = true;
            break;
        }
        context = candidate;
    }
    Ok(RpcResponse::Prime {
        page: crate::protocol::context::PrimePage {
            session: match caller {
                AuthenticatedCaller::Agent(scope) => Some(Box::new(*scope)),
                AuthenticatedCaller::Operator => None,
            },
            notifications: notifications::availability(store, caller)?,
            identity,
            projects: projects.into_iter().map(project_summary).collect(),
            peers,
            context,
            commit_handoffs: commit_handoff::summaries(store, run_id)?,
            commands: available_commands(store, run_id, caller)?,
        },
    })
}

fn brief(
    store: &mut Store,
    run_id: RunId,
    id: TaskId,
) -> Result<(TaskBrief, Option<RecoveryBrief>), RpcFailure> {
    let task = task_by_id(store, run_id, id)?;
    let (assignment, workspace_kind, recovery) = store
        .transaction(|r| {
            let record = r.latest_assignment_for_task(run_id, id)?;
            let workspace = record
                .as_ref()
                .map(|a| r.workspace(a.id))
                .transpose()?
                .flatten();
            let assignment = record
                .map(|a| {
                    let session = a
                        .session_id
                        .map(|id| r.session(id))
                        .transpose()?
                        .flatten();
                    Ok::<_, StoreError>(AssignmentBrief {
                        id: a.id,
                        agent_id: a.agent_id,
                        session_id: a.session_id,
                        generation: a.generation,
                        state: a.state,
                        session_state: session.map(|s| s.state),
                        summary: a.summary.as_deref().map(TextPreview::new),
                        base_commit: workspace
                            .as_ref()
                            .and_then(|w| w.base_commit.clone()),
                        result_commit: workspace
                            .as_ref()
                            .and_then(|w| w.result_commit.clone()),
                        target_commit: workspace
                            .as_ref()
                            .and_then(|w| w.target_commit.clone()),
                    })
                })
                .transpose()?;
            Ok((
                assignment,
                workspace.map(|w| w.kind),
                r.latest_task_recovery(run_id, id)?,
            ))
        })
        .map_err(rpc_state_failure)?;
    let next_action = match task.status {
        TaskStatus::Closed | TaskStatus::Canceled => NextAction::None,
        TaskStatus::Open if !task.unresolved_dependencies.is_empty() => {
            NextAction::WaitForDependencies
        }
        TaskStatus::Open
            if recovery
                .as_ref()
                .is_some_and(|r| r.continuation_assignment_id.is_none()) =>
        {
            NextAction::SpawnContinuation
        }
        TaskStatus::Open if task.ready => NextAction::Ready,
        TaskStatus::InProgress
            if assignment.as_ref().is_some_and(|a| {
                a.state == "active"
                    && a.session_state == Some(LifecycleState::Running)
            }) =>
        {
            NextAction::ContinueAssignment
        }
        TaskStatus::Submitted
            if workspace_kind.as_deref() == Some("worktree")
                && assignment
                    .as_ref()
                    .is_some_and(|a| a.target_commit.is_none()) =>
        {
            NextAction::ReviewAndIntegrate
        }
        TaskStatus::Submitted => NextAction::ValidateAndClose,
        _ => NextAction::InspectProvider,
    };
    let result = task.result.as_ref().map(|result| {
        let summary = result
            .get("validation_summary")
            .or_else(|| result.get("summary"))
            .and_then(serde_json::Value::as_str);
        TextPreview::new(
            &summary
                .map(str::to_owned)
                .unwrap_or_else(|| result.to_string()),
        )
    });
    let omitted_dependencies =
        task.unresolved_dependencies.len().saturating_sub(8);
    Ok((
        TaskBrief {
            id: task.id,
            project_id: task.project_id,
            project: TextPreview::new(&task.project),
            title: TextPreview::new(&task.title),
            description: TextPreview::new(&task.description),
            status: task.status,
            ready: task.ready,
            unresolved_dependencies: task
                .unresolved_dependencies
                .into_iter()
                .take(8)
                .collect(),
            omitted_dependencies,
            result,
            assignment,
            next_action,
        },
        recovery.map(|r| RecoveryBrief {
            task_id: r.task_id,
            assignment_id: r.assignment_id,
            session_id: r.session_id,
            generation: r.generation,
            workspace_path: TextPreview::new(&r.workspace_path),
            base_commit: r.base_commit,
            reason: TextPreview::new(&r.reason),
            continuation_assignment_id: r.continuation_assignment_id,
            handoff: r.handoff,
        }),
    ))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn task_show(
    store: &mut Store,
    run_id: RunId,
    caller: &AuthenticatedCaller,
    task_id: TaskId,
    after: u64,
    limit: u32,
    revision: Option<&str>,
) -> Result<RpcResponse, RpcFailure> {
    require_capability(store, run_id, caller, "task", "read")?;
    let task = task_by_id(store, run_id, task_id)?;
    let assignments = store
        .transaction(|r| r.task_assignment_ids(run_id, task_id))
        .map_err(rpc_state_failure)?;
    document_page(&TaskDetail { task, assignments }, after, limit, revision)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn assignment_show(
    store: &mut Store,
    run_id: RunId,
    caller: &AuthenticatedCaller,
    assignment_id: AssignmentId,
    after: u64,
    limit: u32,
    revision: Option<&str>,
) -> Result<RpcResponse, RpcFailure> {
    require_capability(store, run_id, caller, "task", "read")?;
    let (assignment, workspace, recoveries) = store
        .transaction(|r| {
            Ok((
                r.assignment(assignment_id)?,
                r.workspace(assignment_id)?,
                r.task_recoveries(run_id)?,
            ))
        })
        .map_err(rpc_state_failure)?;
    let assignment =
        assignment.filter(|a| a.run_id == run_id).ok_or_else(|| {
            not_found(format!("assignment `{assignment_id}` does not exist"))
        })?;
    let workspace = workspace.map(|w| WorkspaceDetail {
        project_id: w.project_id,
        kind: w.kind,
        path: w.path.to_string_lossy().into_owned(),
        path_bytes: w.path.as_os_str().as_bytes().to_vec(),
        base_commit: w.base_commit,
        result_commit: w.result_commit,
        target_commit: w.target_commit,
    });
    let recoveries: Vec<_> = recoveries
        .into_iter()
        .filter(|r| {
            r.assignment_id == assignment_id
                || r.continuation_assignment_id == Some(assignment_id)
        })
        .collect();
    let recovery_handoffs = store
        .transaction(|r| {
            recoveries
                .iter()
                .filter_map(|source| {
                    r.recovery_handoff(run_id, source.assignment_id).transpose()
                })
                .collect::<Result<Vec<_>, _>>()
        })
        .map_err(rpc_state_failure)?;
    document_page(
        &AssignmentDetail {
            assignment,
            workspace,
            recoveries,
            recovery_handoffs,
        },
        after,
        limit,
        revision,
    )
}

fn serialization_failure(error: serde_json::Error) -> RpcFailure {
    RpcFailure::new(
        RpcFailureCode::Internal,
        format!("could not serialize context: {error}"),
    )
}

fn document_page(
    document: &impl Serialize,
    after: u64,
    limit: u32,
    expected_revision: Option<&str>,
) -> Result<RpcResponse, RpcFailure> {
    if !(1..=65536).contains(&limit) {
        return Err(invalid_argument(
            "detail limit must be between 1 and 65536",
        ));
    }
    let text =
        serde_json::to_string(document).map_err(serialization_failure)?;
    let revision: String = Sha256::digest(text.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    if after != 0 && expected_revision.is_none() {
        return Err(invalid_argument(
            "detail continuation requires the first page's revision",
        ));
    }
    if expected_revision.is_some_and(|expected| expected != revision) {
        return Err(RpcFailure::new(
            RpcFailureCode::Conflict,
            "detail document changed; restart at byte zero without a revision",
        ));
    }
    let start = usize::try_from(after).ok().filter(|&n| n <= text.len() && text.is_char_boundary(n)).ok_or_else(|| invalid_argument("detail cursor is outside the document or splits a UTF-8 character"))?;
    let mut end = text.len().min(start.saturating_add(limit as usize));
    while !text.is_char_boundary(end) {
        end += 1;
    }
    Ok(RpcResponse::Detail {
        page: DetailPage {
            text: text[start..end].to_owned(),
            revision,
            next_cursor: end as u64,
            total_bytes: text.len() as u64,
            eof: end == text.len(),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_detail_pages_preserve_unicode_and_refuse_mixed_revisions() {
        let document = json!({"description":"café 🦀\n\"quoted\"", "report":"long report".repeat(10)});
        let expected = serde_json::to_string(&document).unwrap();
        for limit in 1..=9 {
            let mut text = String::new();
            let mut after = 0;
            let mut revision = None;
            loop {
                let RpcResponse::Detail { page } =
                    document_page(&document, after, limit, revision.as_deref())
                        .unwrap()
                else {
                    panic!("expected detail page")
                };
                assert!(page.text.len() <= limit as usize + 3);
                assert_eq!(page.total_bytes as usize, expected.len());
                assert!(page.next_cursor > after);
                after = page.next_cursor;
                revision = Some(page.revision);
                text.push_str(&page.text);
                if page.eof {
                    break;
                }
            }
            assert_eq!(text, expected);
            assert!(
                document_page(
                    &json!({"changed":true}),
                    after,
                    limit,
                    revision.as_deref()
                )
                .is_err()
            );
        }
        for (after, limit) in [(1, 10), (0, 0), (0, 65537), (u64::MAX, 1)] {
            assert!(document_page(&document, after, limit, None).is_err());
        }
        let preview = TextPreview::new(&"🦀".repeat(300));
        assert_eq!(preview.text.len(), 512);
        assert!(preview.truncated);
        assert_eq!(preview.total_bytes, 1200);
    }

    #[test]
    fn full_documents_larger_than_transport_frames_remain_retrievable() {
        let document = json!({"description":"Long description. ".repeat(70_000), "report":"Long report. ".repeat(70_000)});
        let mut text = String::new();
        let mut after = 0;
        let mut revision = None;
        loop {
            let RpcResponse::Detail { page } =
                document_page(&document, after, 65536, revision.as_deref())
                    .unwrap()
            else {
                panic!("expected detail page")
            };
            assert!(serde_json::to_vec(&page).unwrap().len() < 400 * 1024);
            after = page.next_cursor;
            revision = Some(page.revision);
            text.push_str(&page.text);
            if page.eof {
                break;
            }
        }
        assert!(text.len() > 2 * 1024 * 1024);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&text).unwrap(),
            document
        );
    }
}

//! Scoped progress inspection without blocking the supervisor's mutation loop.

use super::*;
use crate::protocol::progress::ProgressPage;

#[cfg(test)]
mod tests;

pub(super) fn poll(
    store: &mut Store,
    run_id: RunId,
    caller: &AuthenticatedCaller,
    after: Option<&str>,
    limit: u16,
    wait_seconds: u8,
) -> Result<RpcResponse, RpcFailure> {
    require_current_caller(store, run_id, caller)?;
    require_capability(store, run_id, caller, "task", "read")?;
    validate_bounds(limit, wait_seconds)?;
    let prefix = cursor_prefix(run_id, caller);
    let after = parse_cursor(after, &prefix)?;
    let scan = store
        .transaction(|r| r.progress_after(run_id, after, limit))
        .map_err(rpc_state_failure)?;
    if after > scan.high_watermark {
        return Err(invalid_argument(
            "progress cursor is beyond the durable event sequence",
        ));
    }
    Ok(RpcResponse::Progress {
        page: ProgressPage {
            run_id,
            changes: scan.changes,
            next_cursor: format!("{prefix}{}", scan.next_sequence),
            has_more: scan.next_sequence < scan.high_watermark,
            timed_out: false,
        },
    })
}

fn validate_bounds(limit: u16, wait_seconds: u8) -> Result<(), RpcFailure> {
    if !(1..=100).contains(&limit) {
        return Err(invalid_argument(
            "progress limit must be between 1 and 100",
        ));
    }
    if wait_seconds > 5 {
        return Err(invalid_argument(
            "progress wait must be between 0 and 5 seconds",
        ));
    }
    Ok(())
}

fn cursor_prefix(run_id: RunId, caller: &AuthenticatedCaller) -> String {
    let reader = caller
        .agent_id()
        .map_or_else(|| "operator".to_owned(), |id| id.to_string());
    format!("p1:{run_id}:{reader}:")
}

fn parse_cursor(after: Option<&str>, prefix: &str) -> Result<i64, RpcFailure> {
    let Some(after) = after else { return Ok(0) };
    let invalid = || {
        invalid_argument(
            "invalid progress cursor for this run and caller; resume with a cursor returned to this caller or omit --after",
        )
    };
    let sequence = after.strip_prefix(prefix).ok_or_else(invalid)?;
    if sequence.is_empty()
        || sequence.len() > 19
        || !sequence.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(invalid());
    }
    sequence.parse().map_err(|_| invalid())
}

pub(super) async fn wait(
    commands: &mpsc::Sender<SupervisorCommand>,
    caller: AuthenticatedCaller,
    mut after: Option<String>,
    limit: u16,
    wait_seconds: u8,
) -> Result<RpcResult, SupervisorError> {
    if let Err(failure) = validate_bounds(limit, wait_seconds) {
        return Ok(RpcResult::Err(failure));
    }
    let deadline =
        Instant::now() + Duration::from_secs(u64::from(wait_seconds));
    loop {
        let mut result = request_dispatch(
            commands,
            caller,
            RpcRequest::Progress {
                after,
                limit,
                wait_seconds: 0,
            },
        )
        .await?;
        let RpcResult::Ok(response) = &mut result else {
            return Ok(result);
        };
        let RpcResponse::Progress { page } = response.as_mut() else {
            return Err(SupervisorError::UnexpectedMessage {
                expected: "progress response",
            });
        };
        if !page.changes.is_empty() || page.has_more || wait_seconds == 0 {
            return Ok(result);
        }
        if Instant::now() >= deadline {
            page.timed_out = true;
            return Ok(result);
        }
        after = Some(page.next_cursor.clone());
        // The store belongs to the supervisor loop, which remains free to apply
        // provider observations and mutations between these bounded reads.
        tokio::time::sleep_until(
            (Instant::now() + Duration::from_millis(100)).min(deadline),
        )
        .await;
    }
}

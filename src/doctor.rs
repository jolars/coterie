//! Read-only diagnostics shared by the supervisor and offline inspection.

use std::path::Path;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::id::RunId;
use crate::providers::terminal::{ForegroundIdentity, TerminalObservation};
use crate::state::{
    ExternalResourceState, SessionProcessOwner, Store, StoreError,
};
use crate::transcript::TranscriptStore;
use crate::workspace::{GitWorkspace, WorkspaceBackend};

#[derive(
    Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CheckStatus {
    Ok,
    Warning,
    Error,
    Unavailable,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub(crate) struct DoctorCheck {
    pub(crate) check: String,
    pub(crate) status: CheckStatus,
    pub(crate) subject: Option<String>,
    pub(crate) message: String,
}

#[derive(
    Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, JsonSchema,
)]
pub(crate) struct DoctorReport {
    pub(crate) run_id: Option<RunId>,
    pub(crate) checks: Vec<DoctorCheck>,
}

impl DoctorReport {
    pub(crate) fn add(
        &mut self,
        check: &str,
        status: CheckStatus,
        subject: Option<String>,
        message: impl Into<String>,
    ) {
        self.checks.push(DoctorCheck {
            check: check.to_owned(),
            status,
            subject,
            message: crate::redaction::text(&message.into()),
        });
    }
}

pub(crate) fn inspect_store(
    store: &mut Store,
    run_id: RunId,
    root: &Path,
) -> Result<DoctorReport, StoreError> {
    let mut report = store.diagnose(run_id)?;
    if report.checks.iter().any(|check| {
        check.check == "database_migrations" && check.status != CheckStatus::Ok
    }) {
        return Ok(report);
    }
    // Snapshot durable ownership before any filesystem observation.
    let (sessions, workspaces, projects) =
        store.transaction(|repositories| {
            Ok((
                repositories
                    .sessions(run_id)?
                    .into_iter()
                    .map(|session| {
                        let identity = repositories
                            .foreground_identity(run_id, session.id)?;
                        let current = repositories.session_scope_is_current(
                            crate::auth::SessionScope {
                                run_id,
                                agent_id: session.agent_id,
                                session_id: session.id,
                                generation: session.generation,
                            },
                        )?;
                        Ok((session, identity, current))
                    })
                    .collect::<Result<Vec<_>, StoreError>>()?,
                repositories.workspaces(run_id)?,
                repositories.projects(run_id)?,
            ))
        })?;
    let transcripts = TranscriptStore::new(root);
    for (session, identity, current) in sessions {
        if session.process_owner == SessionProcessOwner::Foreground
            && !(session.state == crate::providers::LifecycleState::Exited
                && session.ended_at.is_some())
        {
            inspect_foreground(
                &mut report,
                &session,
                identity.as_ref(),
                current,
            );
        }
        let subject = Some(session.id.to_string());
        if session.transcript_path != TranscriptStore::relative_path(session.id)
        {
            report.add("transcript", CheckStatus::Error, subject, "Transcript path does not match its session; preserve the file and inspect ownership.");
            continue;
        }
        let path = root.join(&session.transcript_path);
        if std::fs::symlink_metadata(&path)
            .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
            && session.process_owner == SessionProcessOwner::Foreground
        {
            report.add("transcript", CheckStatus::Ok, subject, "Foreground terminal streams are inherited; no managed transcript is expected.");
            continue;
        }
        match crate::private_fs::open(&path, false, false).and_then(|file| file.metadata()) {
            Ok(metadata) => match transcripts.read(session.id, metadata.len().saturating_sub(1), 1) {
                Ok(page) => report.add("transcript", if page.incomplete_tail { CheckStatus::Warning } else { CheckStatus::Ok }, subject,
                    if page.incomplete_tail { "Transcript has an incomplete final frame; logs preserve it without inferring success." } else { "Transcript is accessible with private permissions." }),
                Err(error) => report.add("transcript", CheckStatus::Error, subject, error.to_string()),
            },
            Err(error) => report.add("transcript", if error.kind() == std::io::ErrorKind::NotFound { CheckStatus::Warning } else { CheckStatus::Error }, subject, format!("Transcript is unavailable: {error}. An empty or interrupted launch may not have produced output.")),
        }
    }
    let backend = GitWorkspace::new(root);
    let recoveries = store
        .transaction(|repositories| repositories.task_recoveries(run_id))?;
    for workspace in workspaces {
        let subject = Some(workspace.assignment_id.to_string());
        let current = store.transaction(|repositories| {
            repositories.assignment_scope_is_current(workspace.scope())
        })?;
        if !current
            && !recoveries
                .iter()
                .any(|source| source.assignment_id == workspace.assignment_id)
        {
            report.add("workspace", CheckStatus::Warning, subject, "Workspace belongs to a retired or inconsistent generation; preserve its path and reference.");
            continue;
        }
        let Some(project) = projects
            .iter()
            .find(|project| project.id == workspace.project_id)
        else {
            report.add(
                "workspace",
                CheckStatus::Error,
                subject,
                "Workspace has no attached project.",
            );
            continue;
        };
        match backend.observe(&workspace, project) {
            Ok(ExternalResourceState::Observed) => report.add("workspace", CheckStatus::Ok, subject, format!("Ownership verified at {}. This does not authorize cleanup or prove integration.", workspace.path.display())),
            Ok(state) => report.add("workspace", CheckStatus::Warning, subject, format!("Workspace is {state} at {}; preserve recoverable work.", workspace.path.display())),
            Err(error) => report.add("workspace", CheckStatus::Error, subject, format!("Ownership cannot be verified: {error}. Preserve recoverable work.")),
        }
    }
    Ok(report)
}

fn inspect_foreground(
    report: &mut DoctorReport,
    session: &crate::state::SessionRecord,
    identity: Option<&ForegroundIdentity>,
    current: bool,
) {
    let recovery = "Run `coterie stop` to request bounded shutdown before relaunching. If shutdown cannot verify process exit, preserve the run and its work for operator inspection.";
    let (status, message) = if !current {
        (
            CheckStatus::Unavailable,
            "Foreground session ownership or generation is not current; terminal health is unverified.",
        )
    } else if let Some(identity) = identity {
        if session.provider_session_id.as_deref()
            != Some(&format!("process:{}", identity.process_id))
        {
            (
                CheckStatus::Unavailable,
                "Recorded process evidence does not match the foreground session; terminal health is unverified.",
            )
        } else {
            match identity.inspect() {
                TerminalObservation::LivePty => (
                    CheckStatus::Ok,
                    "Foreground process identity is verified and its Linux PTY remains linked. Hiding an editor terminal does not establish terminal loss.",
                ),
                TerminalObservation::ClosedPty => (
                    CheckStatus::Warning,
                    "Foreground process identity is verified, but its Linux PTY has closed. The surviving process is stranded despite its durable session state.",
                ),
                TerminalObservation::NonTerminal => (
                    CheckStatus::Unavailable,
                    "The foreground launch did not inherit a supported Linux PTY; terminal health is unverified.",
                ),
                TerminalObservation::MissingProcess => (
                    CheckStatus::Warning,
                    "The recorded foreground process is missing; durable session state does not prove liveness or a recorded exit.",
                ),
                TerminalObservation::ExitedProcess => (
                    CheckStatus::Warning,
                    "The verified foreground process has exited but its session lacks an exit observation.",
                ),
                TerminalObservation::IdentityMismatch => (
                    CheckStatus::Unavailable,
                    "Foreground process identity does not match the startup evidence; possible PID reuse or changed ownership. Terminal health is unverified.",
                ),
                TerminalObservation::InputChanged => (
                    CheckStatus::Unavailable,
                    "Foreground input no longer matches the recorded PTY; terminal health is unverified.",
                ),
                TerminalObservation::Unavailable => (
                    CheckStatus::Unavailable,
                    "Foreground process or terminal information is inaccessible or incomplete; terminal health is unverified.",
                ),
            }
        }
    } else {
        (
            CheckStatus::Unavailable,
            "No foreground process identity was recorded at startup. A stored PID or durable session state cannot verify terminal health.",
        )
    };
    report.add(
        "foreground_terminal",
        status,
        Some(session.id.to_string()),
        if status == CheckStatus::Ok {
            message.to_owned()
        } else {
            format!("{message} {recovery}")
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::terminal::InheritedInput;

    #[test]
    fn ambiguous_or_missing_foreground_processes_never_report_healthy() {
        let mut child = std::process::Command::new("sleep")
            .arg("60")
            .stdin(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let identity = ForegroundIdentity::capture_child(
            child.id(),
            InheritedInput::NonTerminal,
        )
        .unwrap();
        child.kill().unwrap();
        child.wait().unwrap();
        let session = crate::state::SessionRecord {
            id: "cs-01ARZ3NDEKTSV4RRFFQ69G5FAY".parse().unwrap(),
            run_id: "cr-01ARZ3NDEKTSV4RRFFQ69G5FAV".parse().unwrap(),
            agent_id: "cg-01ARZ3NDEKTSV4RRFFQ69G5FAX".parse().unwrap(),
            generation: 1,
            provider: "configured_provider".into(),
            provider_session_id: Some(format!(
                "process:{}",
                identity.process_id
            )),
            reconciliation_state: ExternalResourceState::Observed,
            state: crate::providers::LifecycleState::Running,
            transcript_path: "unused".into(),
            created_at: 1,
            ended_at: None,
            reconciled_at: Some(1),
            process_owner: SessionProcessOwner::Foreground,
        };
        for (evidence, current, status, message) in [
            (
                None,
                true,
                CheckStatus::Unavailable,
                "No foreground process identity",
            ),
            (
                Some(&identity),
                false,
                CheckStatus::Unavailable,
                "generation is not current",
            ),
            (
                Some(&identity),
                true,
                CheckStatus::Warning,
                "process is missing",
            ),
        ] {
            let mut report = DoctorReport::default();
            inspect_foreground(&mut report, &session, evidence, current);
            let check = &report.checks[0];
            assert_eq!(check.status, status);
            assert!(check.message.contains(message), "{check:?}");
            assert!(check.message.contains("coterie stop"));
            assert_eq!(
                serde_json::to_value(&report).unwrap()["checks"][0]["subject"],
                session.id.to_string()
            );
        }
        let mut mismatched = identity;
        mismatched.process_id = std::process::id();
        let mut report = DoctorReport::default();
        inspect_foreground(&mut report, &session, Some(&mismatched), true);
        assert_eq!(report.checks[0].status, CheckStatus::Unavailable);
        assert!(report.checks[0].message.contains("does not match"));
    }

    #[test]
    fn generated_doctor_schema_matches_the_documented_contract() {
        let mut schema = serde_json::to_string_pretty(&schemars::schema_for!(
            super::DoctorReport
        ))
        .unwrap();
        schema.push('\n');
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("schemas/doctor-report-v1.schema.json");
        if std::env::var_os("COTERIE_UPDATE_SCHEMAS").is_some() {
            std::fs::write(&path, &schema).unwrap();
        }
        assert_eq!(schema, std::fs::read_to_string(path).unwrap());
    }
}

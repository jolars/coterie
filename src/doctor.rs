//! Read-only diagnostics shared by the supervisor and offline inspection.

use std::path::Path;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::id::RunId;
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
                repositories.sessions(run_id)?,
                repositories.workspaces(run_id)?,
                repositories.projects(run_id)?,
            ))
        })?;
    let transcripts = TranscriptStore::new(root);
    for session in sessions {
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
    for workspace in workspaces {
        let subject = Some(workspace.assignment_id.to_string());
        let current = store.transaction(|repositories| {
            repositories.assignment_scope_is_current(workspace.scope())
        })?;
        if !current {
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

#[cfg(test)]
mod tests {
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

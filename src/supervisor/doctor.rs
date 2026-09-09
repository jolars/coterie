//! Local diagnostics remain usable when the run supervisor cannot answer.

use super::*;
use crate::doctor::{CheckStatus, DoctorReport};

pub(super) async fn run(
    json_output: bool,
) -> Result<crate::cli::ExitCategory, SupervisorError> {
    if [
        "COTERIE_AGENT_ID",
        "COTERIE_SESSION_ID",
        "COTERIE_TOKEN",
        "COTERIE_RUN_ID",
        "COTERIE_PROJECT_ID",
        "COTERIE_PRIMARY_PROJECT_ROOT",
        "COTERIE_SOCKET",
    ]
    .iter()
    .any(|name| env::var_os(name).is_some())
    {
        return Err(RpcFailure::new(
            RpcFailureCode::PermissionDenied,
            "doctor requires the local operator environment",
        )
        .into());
    }
    let project = discover_current_project()?;
    let directories = CoterieDirectories::from_environment()?;
    let mut report = DoctorReport::default();
    let mut private = true;
    for path in directories.inspection_directories() {
        let secure = check(
            &mut report,
            "runtime_permissions",
            path,
            crate::private_fs::check_directory(path),
        );
        if path.starts_with(&directories.state) {
            private &= secure;
        }
    }
    let global_config = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            env::var_os("HOME")
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .map(|path| path.join(".config"))
        })
        .map(|path| path.join("coterie/config.toml"));
    let paths = [
        project.canonical_path.join("coterie.toml"),
        project.canonical_path.join("coterie.lock"),
    ];
    for path in paths.into_iter().chain(global_config) {
        if fs::symlink_metadata(&path).is_ok() {
            report.add("configuration", CheckStatus::Warning, Some(path.display().to_string()), "External configuration and lock files are not loaded in this milestone; compatibility is unverified. The run uses compiled operator policy.");
        }
    }
    report.add("configuration", CheckStatus::Ok, None, "Current runtime policy is the compiled builtin:standard@1 definition; external configuration compatibility requires M5.");
    inspect_provider(&mut report);
    if private {
        match ActiveRunIndex::new(&directories).lookup(&project.identity) {
            Ok(Some(entry)) => {
                report.run_id = Some(entry.run_id);
                inspect_run(&mut report, &directories, &entry, &project).await;
            }
            Ok(None) => report.add("supervisor", CheckStatus::Unavailable, None, "No active run is indexed. Launch coterie to start one."),
            Err(error) => report.add("project_index", CheckStatus::Error, None, format!("{error}. Preserve the index and run state; ownership is ambiguous.")),
        }
    } else {
        report.add("supervisor", CheckStatus::Unavailable, None, "Runtime paths are absent or insecure; inspection did not open the run database or connect to a socket.");
    }
    render_public_response(json_output, None, &RpcResponse::Doctor { report })
}

fn inspect_provider(report: &mut DoctorReport) {
    let defaults = compiled_defaults();
    let binding = &defaults.providers["codex"];
    let provider = CodexProvider::new(binding.command.iter().copied());
    match provider.probe() {
        Ok(probe) => {
            let archetype = builtin_standard();
            let mut errors = Vec::new();
            for role in archetype.roles.values() {
                let profile =
                    archetype.permission_profiles[role.permission_profile];
                let mut required = vec![
                    ProviderCapability::StartupInstructions,
                    if role.mode == RoleMode::Interactive {
                        ProviderCapability::ForegroundInteractive
                    } else {
                        ProviderCapability::BackgroundJobs
                    },
                ];
                if role.mode != RoleMode::Interactive {
                    required.extend([
                        ProviderCapability::StructuredLifecycleEvents,
                        ProviderCapability::TranscriptStreaming,
                        ProviderCapability::Interrupt,
                        ProviderCapability::Termination,
                    ]);
                }
                if let Err(error) = validate_provider_capabilities(
                    &probe,
                    required
                        .into_iter()
                        .chain(required_permission_capabilities(profile)),
                ) {
                    errors.push(error.to_string());
                }
            }
            report.add("provider", if errors.is_empty() { CheckStatus::Ok } else { CheckStatus::Error }, Some(probe.name),
                if errors.is_empty() { format!("Installed version {} supports the required compiled role capabilities.", probe.version) } else { errors.join("; ") });
        }
        Err(error) => report.add(
            "provider",
            CheckStatus::Unavailable,
            None,
            error.to_string(),
        ),
    }
}

fn check(
    report: &mut DoctorReport,
    name: &str,
    path: &Path,
    result: io::Result<()>,
) -> bool {
    let ok = result.is_ok();
    report.add(
        name,
        if ok {
            CheckStatus::Ok
        } else {
            CheckStatus::Error
        },
        Some(path.display().to_string()),
        match result {
            Ok(()) => "Private ownership and permissions verified.".to_owned(),
            Err(error) => error.to_string(),
        },
    );
    ok
}

async fn inspect_run(
    report: &mut DoctorReport,
    directories: &CoterieDirectories,
    entry: &ActiveRunEntry,
    project: &DiscoveredProject,
) {
    let root = directories.runs.join(entry.run_id.to_string());
    if !check(
        report,
        "runtime_permissions",
        &root,
        crate::private_fs::check_directory(&root),
    ) {
        return;
    }
    let database = root.join(DATABASE_FILE);
    let mut private = check(
        report,
        "runtime_permissions",
        &database,
        crate::private_fs::open(&database, false, false).map(|_| ()),
    );
    for suffix in ["-wal", "-shm", "-journal"] {
        let path = root.join(format!("{DATABASE_FILE}{suffix}"));
        if fs::symlink_metadata(&path).is_ok() {
            private &= check(
                report,
                "runtime_permissions",
                &path,
                crate::private_fs::open(&path, false, false).map(|_| ()),
            );
        }
    }
    let lease_path = directories
        .leases
        .join(format!("{}.lock", entry.project_key));
    match crate::private_fs::open(&lease_path, false, false) {
        Ok(file) => match file.try_lock() {
            Ok(()) => report.add("project_lease", CheckStatus::Warning, None, "Project lease is free. A stale index alone does not prove supervisor liveness."),
            Err(fs::TryLockError::WouldBlock) => report.add("project_lease", CheckStatus::Ok, None, "Project lease is held; the socket handshake must independently prove run identity."),
            Err(error) => report.add("project_lease", CheckStatus::Error, None, error.to_string()),
        },
        Err(error) => report.add("project_lease", CheckStatus::Error, None, error.to_string()),
    }
    let socket = directories.socket_path(entry.run_id);
    match SupervisorClient::connect_operator_at(&socket, entry).await {
        Ok(mut client) => {
            report.add("supervisor", CheckStatus::Ok, None, "Supervisor handshake proves the indexed run and project identity.");
            if private {
                match client.request(RpcRequest::Doctor).await {
                    Ok(RpcResponse::Doctor { report: snapshot }) => report.checks.extend(snapshot.checks),
                    Ok(_) => report.add("database", CheckStatus::Error, None, "Supervisor returned an unexpected diagnostic response."),
                    Err(error) => report.add("database", CheckStatus::Unavailable, None, error.to_string()),
                }
            }
        }
        Err(error) => {
            report.add("supervisor", CheckStatus::Unavailable, None, format!("{error}. Launch coterie to attempt lease-protected recovery of this indexed run."));
            if private {
                let result = Store::open_read_only(&database).and_then(|mut store| {
                    let matching = store.transaction(|repositories| repositories.project(entry.project_id))?
                        .is_some_and(|stored| stored.run_id == entry.run_id && stored.identity == project.identity && stored.canonical_path == project.canonical_path);
                    if !matching { report.add("project_index", CheckStatus::Error, None, "Index does not match durable project ownership; automatic recovery must refuse it."); }
                    crate::doctor::inspect_store(&mut store, entry.run_id, &root)
                });
                match result {
                    Ok(snapshot) => report.checks.extend(snapshot.checks),
                    Err(error) => report.add(
                        "database",
                        CheckStatus::Error,
                        None,
                        error.to_string(),
                    ),
                }
            }
        }
    }
}

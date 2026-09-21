use super::*;

fn intent() -> run_recovery::Intent {
    run_recovery::Intent {
        operation_id: "co-01ARZ3NDEKTSV4RRFFQ69G5FC1".parse().unwrap(),
        reason: "Continue interrupted implementation.".to_owned(),
    }
}

pub(super) fn prepare(fixture: &mut Fixture) {
    let workspace = fixture.workspace();
    fs::write(workspace.path.join("README.md"), "staged work\n").unwrap();
    let repository = Repository::open(&workspace.path).unwrap();
    let mut index = repository.index().unwrap();
    index.add_path(Path::new("README.md")).unwrap();
    index.write().unwrap();
    fs::write(workspace.path.join("README.md"), "unstaged work\n").unwrap();
    fs::write(workspace.path.join("untracked.txt"), "retained work\n").unwrap();
    fs::write(
        fixture.root.join("original-index"),
        fs::read(repository.path().join("index")).unwrap(),
    )
    .unwrap();
    fixture.shutdown();
}

pub(super) fn exercise(fixture: &mut Fixture) {
    run_recovery::reactivate(
        &mut fixture.store,
        RUN.parse().unwrap(),
        &intent(),
        &mut fixture.sessions,
        &fixture.workspaces,
    )
    .unwrap();
}

pub(super) fn verify(fixture: &mut Fixture) {
    let workspace = fixture.workspace();
    let repository = Repository::open(&workspace.path).unwrap();
    assert_eq!(
        fs::read(repository.path().join("index")).unwrap(),
        fs::read(fixture.root.join("original-index")).unwrap()
    );
    assert_eq!(
        fs::read_to_string(workspace.path.join("README.md")).unwrap(),
        "unstaged work\n"
    );
    assert_eq!(
        fs::read_to_string(workspace.path.join("untracked.txt")).unwrap(),
        "retained work\n"
    );
    fixture
        .store
        .transaction(|r| {
            let run = RUN.parse().unwrap();
            assert_eq!(r.run(run)?.unwrap().status, "active");
            assert!(r.run_shutdown(run)?.is_none());
            assert!(
                r.session_controls(run)?
                    .iter()
                    .all(|control| control.phase == ControlPhase::Completed)
            );
            assert_eq!(
                r.assignment(workspace.assignment_id)?.unwrap().state,
                "draining"
            );
            assert_eq!(
                r.task(TASK.parse().unwrap())?.unwrap().status,
                TaskStatus::InProgress
            );
            assert_eq!(
                r.events_after(run, 0, 1000)?
                    .iter()
                    .filter(|event| event.event_type == "run.recovered")
                    .count(),
                1
            );
            for session in r.sessions(run)? {
                assert!(
                    r.session_credential(session.id)?
                        .unwrap()
                        .revoked_at
                        .is_some()
                );
            }
            Ok(())
        })
        .unwrap();
}

#[test]
fn stopped_run_recovery_refuses_uncertain_or_inconsistent_ownership_without_mutation()
 {
    for sql in [
        "UPDATE sessions SET reconciliation_state = 'unknown'",
        "UPDATE sessions SET state = 'lost', reconciliation_state = 'lost'",
        "UPDATE sessions SET ended_at = NULL",
        "UPDATE session_credentials SET revoked_at = NULL",
        "UPDATE agents SET generation = generation + 1",
        "UPDATE operations SET reconciliation_state = 'desired' WHERE kind = 'agent.spawn'",
        "UPDATE workspaces SET state = 'unknown'",
        "UPDATE session_controls SET generation = generation + 1",
        "UPDATE run_shutdowns SET phase = 'timed_out'",
        "UPDATE claims SET released_at = claimed_at",
    ] {
        let _clock = test_clock::FrozenClock::new(FIXTURE_TIME_MS);
        let directory = Directory::new();
        prepare_project(&directory.0);
        let mut fixture = Fixture::open(directory.0.clone());
        fixture.prepare("recover-run");
        rusqlite::Connection::open(fixture.run.join("state.sqlite3"))
            .unwrap()
            .execute_batch(sql)
            .unwrap();
        let before = snapshot(&directory.0);
        let failure = run_recovery::reactivate(
            &mut fixture.store,
            RUN.parse().unwrap(),
            &intent(),
            &mut fixture.sessions,
            &fixture.workspaces,
        )
        .unwrap_err();
        assert!(
            matches!(
                failure,
                SupervisorError::State(StoreError::RunRecoveryConflict { .. })
            ),
            "{sql}: {failure:?}"
        );
        let after = snapshot(&directory.0);
        assert_eq!(before.database, after.database, "{sql}");
        assert_eq!(before.files, after.files, "{sql}");
    }
}

#[test]
fn stopped_run_recovery_replays_archived_stop_without_stopping_the_continuation()
 {
    let _clock = test_clock::FrozenClock::new(FIXTURE_TIME_MS);
    let directory = Directory::new();
    prepare_project(&directory.0);
    let mut fixture = Fixture::open(directory.0.clone());
    fixture.prepare("recover-run");
    exercise(&mut fixture);
    let before = snapshot(&directory.0);
    fixture.shutdown();
    let after = snapshot(&directory.0);
    assert_eq!(before.database, after.database);
    assert_eq!(before.files, after.files);
    verify(&mut fixture);
}

#[test]
fn stopped_run_recovery_requires_the_secondary_lease_even_without_an_index() {
    let directory = Directory::new();
    prepare_project(&directory.0);
    let mut fixture = Fixture::open(directory.0.clone());
    let run_id = RUN.parse().unwrap();
    let primary =
        DiscoveredProject::discover(directory.0.join("project")).unwrap();
    let library_path = directory.0.join("library");
    fs::create_dir(&library_path).unwrap();
    let library = DiscoveredProject::discover(&library_path).unwrap();
    fixture
        .store
        .transaction(|r| {
            r.insert_project(&ProjectRecord {
                id: ProjectId::generate(),
                run_id,
                alias: "library".to_owned(),
                original_path: library.original_path.clone(),
                canonical_path: library.canonical_path.clone(),
                identity: library.identity.clone(),
                is_primary: false,
                attached_at: 1,
            })
        })
        .unwrap();
    let runtime = directory.0.join("rt");
    crate::private_fs::directory(&runtime).unwrap();
    let directories = CoterieDirectories::from_base_directories(
        &runtime,
        directory.0.join("state"),
    )
    .unwrap();
    directories.prepare().unwrap();
    let LeaseAttempt::Acquired(primary_lease) =
        ProjectLease::try_acquire(&directories, &primary.identity, run_id)
            .unwrap()
    else {
        panic!("primary lease")
    };
    let LeaseAttempt::Acquired(_secondary_lease) = ProjectLease::try_acquire(
        &directories,
        &library.identity,
        RunId::generate(),
    )
    .unwrap() else {
        panic!("secondary lease")
    };
    let active = ActiveRunEntry::new(
        run_id,
        PROJECT.parse().unwrap(),
        primary.identity.clone(),
    );
    let mut projects = projects::AttachedProjects::new(
        directories.clone(),
        active,
        primary_lease,
    );
    let error = projects
        .acquire_for_reactivation(&mut fixture.store, run_id)
        .unwrap_err();
    assert!(error.to_string().contains("exclusive lease"));
    let index = ActiveRunIndex::new(&directories);
    assert!(index.lookup(&primary.identity).unwrap().is_none());
    assert!(index.lookup(&library.identity).unwrap().is_none());
}

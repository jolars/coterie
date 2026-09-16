use super::*;
use crate::workspace::IntegrationStrategy;

fn integrate(
    fixture: &mut Fixture,
    operation: OperationId,
    strategy: IntegrationStrategy,
) -> Result<RpcResponse, RpcFailure> {
    let assignment = fixture.workspace().assignment_id;
    integrate_workspace(
        &mut fixture.store,
        &mut fixture.workspaces,
        RUN.parse().unwrap(),
        &AuthenticatedCaller::Operator,
        operation,
        assignment,
        Some(strategy),
    )
}

#[test]
fn git_write_failures_preserve_intent_and_recover_with_identical_retries() {
    for strategy in [IntegrationStrategy::Merge, IntegrationStrategy::Rebase] {
        for boundary in [
            "integration.tree.before",
            "integration.rebase.commit.before",
            "integration.commit.before",
            "integration.checkout.before",
            "integration.reference.before",
        ] {
            if strategy == IntegrationStrategy::Merge
                && boundary == "integration.rebase.commit.before"
            {
                continue;
            }
            let directory = Directory::new();
            prepare_project(&directory.0);
            let mut fixture = Fixture::open(directory.0.clone());
            fixture.prepare(if strategy == IntegrationStrategy::Merge {
                "merge"
            } else {
                "rebase"
            });
            let workspace = fixture.workspace();
            let project_path = directory.0.join("project");
            let repository = Repository::open(&project_path).unwrap();
            let original = repository.head().unwrap().target().unwrap();
            let objects = repository.commondir().join("objects");
            let saved_objects = repository.commondir().join("saved-objects");
            let obstruction = if boundary == "integration.checkout.before" {
                repository.path().join("index.lock")
            } else if boundary == "integration.reference.before" {
                repository.path().join(format!(
                    "{}.lock",
                    repository.head().unwrap().name().unwrap()
                ))
            } else {
                objects.clone()
            };
            let blocked = obstruction.clone();
            let saved = saved_objects.clone();
            let object_failure = obstruction == objects;
            let _action = injection::on_point(boundary, move || {
                if object_failure {
                    // Preserve every existing object while making writes fail for any UID.
                    fs::rename(&blocked, &saved).unwrap();
                }
                fs::write(blocked, "injected Git write failure").unwrap();
            });
            let operation = OperationId::generate();
            let failure = integrate(&mut fixture, operation, strategy);
            assert!(
                failure.is_err(),
                "{strategy:?} at {boundary}: {failure:?}"
            );
            fs::remove_file(&obstruction).unwrap();
            if object_failure {
                fs::rename(&saved_objects, &objects).unwrap();
            }
            let fresh = Repository::open(&project_path).unwrap();
            assert_eq!(fresh.head().unwrap().target(), Some(original));
            assert!(fresh.find_commit(original).is_ok());
            assert_eq!(fixture.workspace().target_commit, None);
            let intent = fixture
                .store
                .transaction(|r| {
                    let operation = r.operation(operation)?.unwrap();
                    assert_eq!(
                        operation.reconciliation_state,
                        Some(ExternalResourceState::Unknown)
                    );
                    assert!(operation.reconciliation_error.is_some());
                    assert_eq!(
                        r.events_after(RUN.parse().unwrap(), 0, 1000)?
                            .iter()
                            .filter(|event| event.event_type
                                == "workspace.integrated")
                            .count(),
                        0
                    );
                    Ok(operation)
                })
                .unwrap();
            let wrong_strategy = if strategy == IntegrationStrategy::Merge {
                IntegrationStrategy::Rebase
            } else {
                IntegrationStrategy::Merge
            };
            assert_eq!(
                integrate(&mut fixture, operation, wrong_strategy)
                    .unwrap_err()
                    .code,
                RpcFailureCode::Conflict
            );
            drop(fixture);

            let mut fixture = Fixture::open(directory.0.clone());
            // Restart reconciliation consumes the durable plan, including its timestamp.
            reconcile_integration_operation(
                &mut fixture.store,
                &mut fixture.workspaces,
                &intent,
                20,
            )
            .unwrap();
            if boundary == "integration.checkout.before" {
                // A failed index write can leave checked-out files. Recovery must
                // preserve them until the operator validates and stages that tree.
                assert_eq!(fixture.workspace().target_commit, None);
                let index = fs::read(repository.path().join("index")).unwrap();
                let result = fs::read(project_path.join("result.txt")).unwrap();
                let second = fs::read(project_path.join("second.txt")).unwrap();
                assert!(integrate(&mut fixture, operation, strategy).is_err());
                assert_eq!(
                    fs::read(repository.path().join("index")).unwrap(),
                    index
                );
                assert_eq!(
                    fs::read(project_path.join("result.txt")).unwrap(),
                    result
                );
                assert_eq!(
                    fs::read(project_path.join("second.txt")).unwrap(),
                    second
                );
                assert_eq!(result, b"recoverable worker result\n");
                assert_eq!(second, b"another recoverable change\n");
                let fresh = Repository::open(&project_path).unwrap();
                let mut index = fresh.index().unwrap();
                index.add_path(Path::new("result.txt")).unwrap();
                index.add_path(Path::new("second.txt")).unwrap();
                index.write().unwrap();
                reconcile_integration_operation(
                    &mut fixture.store,
                    &mut fixture.workspaces,
                    &intent,
                    21,
                )
                .unwrap();
            }
            let recovered =
                fixture.workspace().target_commit.unwrap_or_else(|| {
                    let operation = fixture
                        .store
                        .transaction(|r| r.operation(operation))
                        .unwrap();
                    panic!("{strategy:?} at {boundary}: {operation:?}");
                });
            let response =
                integrate(&mut fixture, operation, strategy).unwrap();
            let before = fs::read(repository.path().join("index")).unwrap();
            let reference = repository
                .path()
                .join(repository.head().unwrap().name().unwrap());
            let reference_bytes = fs::read(&reference).unwrap();
            assert_eq!(
                integrate(&mut fixture, operation, strategy).unwrap(),
                response
            );
            let observation = fixture
                .store
                .transaction(|r| r.operation(operation))
                .unwrap()
                .unwrap();
            reconcile_integration_operation(
                &mut fixture.store,
                &mut fixture.workspaces,
                &observation,
                21,
            )
            .unwrap();
            assert_eq!(
                fs::read(repository.path().join("index")).unwrap(),
                before
            );
            assert_eq!(fs::read(reference).unwrap(), reference_bytes);
            let fresh = Repository::open(&project_path).unwrap();
            let oid = git2::Oid::from_str(&recovered).unwrap();
            assert_eq!(fresh.head().unwrap().target(), Some(oid));
            assert!(fresh.find_commit(oid).unwrap().tree().is_ok());
            assert_eq!(
                fs::read_to_string(project_path.join("result.txt")).unwrap(),
                "recoverable worker result\n"
            );
            assert_eq!(
                fs::read_to_string(project_path.join("second.txt")).unwrap(),
                "another recoverable change\n"
            );
            assert_eq!(
                Repository::open(&workspace.path)
                    .unwrap()
                    .head()
                    .unwrap()
                    .target()
                    .unwrap()
                    .to_string(),
                workspace.result_commit.unwrap()
            );
            fixture
                .store
                .transaction(|r| {
                    let saved = r.operation(operation)?.unwrap();
                    assert_eq!(saved.result, intent.result);
                    assert_eq!(saved.request, intent.request);
                    assert_eq!(
                        saved.reconciliation_state,
                        Some(ExternalResourceState::Observed)
                    );
                    let events =
                        r.events_after(RUN.parse().unwrap(), 0, 1000)?;
                    for kind in [
                        "workspace.integration_desired",
                        "workspace.integrated",
                    ] {
                        assert_eq!(
                            events
                                .iter()
                                .filter(|event| event.event_type == kind)
                                .count(),
                            1,
                            "{kind}"
                        );
                    }
                    Ok(())
                })
                .unwrap();
        }
    }
}

use super::*;
use crate::fault::injection;
use crate::workspace::{IntegrationCandidate, IntegrationStrategy};
use git2::{ObjectType, Oid, Time};

fn loose_object(repository: &Repository, oid: Oid) -> PathBuf {
    let oid = oid.to_string();
    repository
        .commondir()
        .join("objects")
        .join(&oid[..2])
        .join(&oid[2..])
}

#[test]
fn libgit2_commit_id_does_not_prove_publication_after_a_write_failure() {
    let fixture = GitFixture::new();
    let (_, workspace) = fixture.materialized_workspace();
    let repository = Repository::open(&workspace.path).unwrap();
    let parent = repository.head().unwrap().peel_to_commit().unwrap();
    let tree = parent.tree().unwrap();
    let signature =
        Signature::new("Test", "test@example.invalid", &Time::new(42, 0))
            .unwrap();
    let (message, buffer, expected, obstruction) = (0..1000)
        .find_map(|attempt| {
            let message = format!("publication probe {attempt}");
            let buffer = repository
                .commit_create_buffer(
                    &signature,
                    &signature,
                    &message,
                    &tree,
                    &[&parent],
                )
                .unwrap();
            let oid = Oid::hash_object(ObjectType::Commit, &buffer).unwrap();
            let directory =
                loose_object(&repository, oid).parent().unwrap().to_owned();
            (!directory.exists()).then_some((message, buffer, oid, directory))
        })
        .unwrap();
    // A file in place of the fanout directory fails even when tests run as root.
    fs::write(&obstruction, "deny object publication").unwrap();
    assert!(
        repository
            .odb()
            .unwrap()
            .write(ObjectType::Commit, &buffer)
            .is_err()
    );
    let returned = repository.commit(
        Some("HEAD"),
        &signature,
        &signature,
        &message,
        &tree,
        &[&parent],
    );
    assert_eq!(
        git2::Version::get().libgit2_version(),
        (1, 9, 7),
        "reevaluate the pinned upstream reproduction after a dependency update"
    );
    assert_eq!(
        returned.unwrap(),
        expected,
        "libgit2 1.9.7 masks the write error"
    );
    let fresh = Repository::open(&workspace.path).unwrap();
    assert!(fresh.find_commit(expected).is_err());
    assert_eq!(fresh.head().unwrap().target(), Some(parent.id()));
    assert_eq!(
        fresh
            .find_reference(&fixture.reference_name())
            .unwrap()
            .target(),
        Some(parent.id())
    );
    drop(fresh);

    fs::remove_file(obstruction).unwrap();
    assert_eq!(
        repository
            .commit(
                Some("HEAD"),
                &signature,
                &signature,
                &message,
                &tree,
                &[&parent]
            )
            .unwrap(),
        expected
    );
    let fresh = Repository::open(&workspace.path).unwrap();
    assert_eq!(fresh.find_commit(expected).unwrap().tree_id(), tree.id());
    assert_eq!(fresh.head().unwrap().target(), Some(expected));
    assert_eq!(
        fresh
            .find_reference(&fixture.reference_name())
            .unwrap()
            .target(),
        Some(expected)
    );
    assert_eq!(head_commit(&fixture.project.canonical_path), fixture.base);
}

#[test]
fn integration_does_not_record_success_when_the_reference_did_not_advance() {
    for replace_head in [false, true] {
        let fixture = GitFixture::new();
        let (backend, mut workspace) = fixture.materialized_workspace();
        // Equal trees make status clean even if the wrong commit remains published.
        workspace.result_commit = Some(commit_file(
            &workspace.path,
            "README.md",
            "fixture\n",
            "empty contribution",
        ));
        let mut store = store_with_workspace_records(
            fixture.project.clone(),
            workspace.clone(),
        );
        let mut supervisor = WorkspaceSupervisor::new(backend);
        let plan = supervisor
            .prepare_integration(
                &mut store,
                workspace.scope(),
                11,
                IntegrationStrategy::Rebase,
            )
            .unwrap();
        let path = fixture.project.canonical_path.clone();
        let reference = plan.target_reference.clone();
        let base = Oid::from_str(&fixture.base).unwrap();
        let _action =
            injection::on_point("integration.reference.after", move || {
                let repository = Repository::open(path).unwrap();
                if replace_head {
                    let tip = repository.head().unwrap().target().unwrap();
                    repository
                        .reference("refs/heads/other", tip, false, "test")
                        .unwrap();
                    repository.set_head("refs/heads/other").unwrap();
                } else {
                    repository
                        .reference(
                            &reference,
                            base,
                            true,
                            "test unpublished update",
                        )
                        .unwrap();
                }
            });
        let operation = OPERATION_ID.parse().unwrap();
        let result = supervisor.integrate(
            &mut store,
            workspace.scope(),
            &plan,
            operation,
            "operator",
            12,
        );
        assert!(
            result.is_err(),
            "an unchanged tree does not prove publication: {result:?}"
        );
        assert_eq!(
            stored_workspace(&mut store, workspace.assignment_id).target_commit,
            None
        );
        assert_eq!(
            store
                .transaction(|r| r.events_after(workspace.run_id, 0, 100))
                .unwrap()
                .iter()
                .filter(|e| e.event_type == "workspace.integrated")
                .count(),
            0
        );

        if replace_head {
            Repository::open(&fixture.project.canonical_path)
                .unwrap()
                .set_head(&plan.target_reference)
                .unwrap();
        }
        let integrated = supervisor
            .integrate(
                &mut store,
                workspace.scope(),
                &plan,
                operation,
                "operator",
                13,
            )
            .unwrap();
        assert_eq!(
            integrated.target_commit,
            workspace.result_commit.clone().unwrap()
        );
        assert_eq!(
            supervisor
                .integrate(
                    &mut store,
                    workspace.scope(),
                    &plan,
                    operation,
                    "operator",
                    14
                )
                .unwrap(),
            integrated
        );
        assert_eq!(
            store
                .transaction(|r| r.events_after(workspace.run_id, 0, 100))
                .unwrap()
                .iter()
                .filter(|e| e.event_type == "workspace.integrated")
                .count(),
            1
        );
    }
}

#[test]
fn integration_commit_must_be_readable_outside_the_writer_object_database() {
    let fixture = GitFixture::new();
    let (_, workspace) = fixture.materialized_workspace();
    let repository = Repository::open(&workspace.path).unwrap();
    let parent = repository.head().unwrap().peel_to_commit().unwrap();
    let tree = parent.tree().unwrap();
    let signature =
        Signature::new("Test", "test@example.invalid", &Time::new(42, 0))
            .unwrap();
    let buffer = repository
        .commit_create_buffer(
            &signature,
            &signature,
            "unpublished commit",
            &tree,
            &[&parent],
        )
        .unwrap()
        .to_vec();
    let oid = Oid::hash_object(ObjectType::Commit, &buffer).unwrap();
    let candidate = IntegrationCandidate {
        tree_id: tree.id(),
        target_commit: oid,
        commit_buffer: Some(buffer),
    };
    let odb = repository.odb().unwrap();
    let _pack = odb.add_new_mempack_backend(1000).unwrap();
    let result = candidate.write_commit(&repository, &workspace);
    assert!(
        Repository::open(&workspace.path)
            .unwrap()
            .find_commit(oid)
            .is_err()
    );
    assert!(
        result.is_err(),
        "an object visible only in the writer is not published"
    );
}

#[test]
fn workspace_creation_observes_readable_objects_and_recovers_reference_write_failures()
 {
    let fixture = GitFixture::new();
    let mut workspace = fixture.workspace();
    workspace.base_commit = Some(fixture.base.clone());
    let mut store = store_with_workspace_records(
        fixture.project.clone(),
        workspace.clone(),
    );
    let mut supervisor =
        WorkspaceSupervisor::new(GitWorkspace::new(&fixture.state));
    let repository = Repository::open(&fixture.project.canonical_path).unwrap();
    let lock = repository
        .path()
        .join(format!("{}.lock", fixture.reference_name()));
    fs::create_dir_all(lock.parent().unwrap()).unwrap();
    fs::write(&lock, "injected reference lock").unwrap();
    assert!(
        supervisor
            .materialize(&mut store, workspace.scope(), 11)
            .is_err()
    );
    assert_eq!(
        stored_workspace(&mut store, workspace.assignment_id).state,
        ExternalResourceState::Unknown
    );
    assert!(!workspace.path.exists());
    fs::remove_file(lock).unwrap();
    for time in [12, 13] {
        assert_eq!(
            supervisor
                .materialize(&mut store, workspace.scope(), time)
                .unwrap(),
            ExternalResourceState::Observed
        );
    }
    let fresh = Repository::open(&workspace.path).unwrap();
    let base = fresh.head().unwrap().peel_to_commit().unwrap();
    assert_eq!(base.id().to_string(), fixture.base);
    assert!(base.tree().is_ok());
    assert_eq!(repository.worktrees().unwrap().len(), 1);

    let object = loose_object(&repository, base.id());
    let saved = fs::read(&object).unwrap();
    fs::remove_file(&object).unwrap();
    // A matching symbolic branch name alone must not establish a usable worktree.
    assert_eq!(
        supervisor
            .materialize(&mut store, workspace.scope(), 14)
            .unwrap(),
        ExternalResourceState::Unknown
    );
    fs::write(object, saved).unwrap();
    assert_eq!(
        supervisor
            .materialize(&mut store, workspace.scope(), 15)
            .unwrap(),
        ExternalResourceState::Observed
    );
    assert_eq!(repository.worktrees().unwrap().len(), 1);
}

#[test]
fn integration_trees_must_be_published_before_checkout_for_both_strategies() {
    for strategy in [IntegrationStrategy::Merge, IntegrationStrategy::Rebase] {
        let fixture = GitFixture::new();
        let (backend, mut workspace) = fixture.materialized_workspace();
        workspace.result_commit = Some(commit_file(
            &workspace.path,
            "worker.txt",
            "worker\n",
            "worker",
        ));
        let target = commit_file(
            &fixture.project.canonical_path,
            "target.txt",
            "target\n",
            "target",
        );
        let repository =
            Repository::open(&fixture.project.canonical_path).unwrap();
        let odb = repository.odb().unwrap();
        let _pack = odb.add_new_mempack_backend(1000).unwrap();
        let result = crate::workspace::integration_candidate(
            &repository,
            &workspace,
            Oid::from_str(&fixture.base).unwrap(),
            Oid::from_str(workspace.result_commit.as_ref().unwrap()).unwrap(),
            Oid::from_str(&target).unwrap(),
            42,
            strategy,
        );
        assert!(matches!(
            result,
            Err(WorkspaceBackendError::Git {
                action: "verify Git object publication through a fresh repository",
                ..
            })
        ));
        assert_eq!(head_commit(&fixture.project.canonical_path), target);
        assert!(!fixture.project.canonical_path.join("worker.txt").exists());
        assert_eq!(
            backend.result_commit(&workspace, &fixture.project).unwrap(),
            workspace.result_commit
        );
    }
}

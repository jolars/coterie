use super::*;
use crate::providers::terminal::{ForegroundIdentity, InheritedInput};

fn fixture() -> (Store, SessionScope, ForegroundIdentity) {
    let mut store = Store::open_in_memory().unwrap();
    let mut records = Records::fixture();
    records.session.process_owner = SessionProcessOwner::Foreground;
    records.session.provider_session_id = Some("process:123".into());
    let scope = SessionScope {
        run_id: records.run.id,
        agent_id: records.agent.id,
        session_id: records.session.id,
        generation: records.session.generation,
    };
    insert_claim_prerequisites(&mut store, &records);
    store
        .transaction(|r| r.insert_session(&records.session))
        .unwrap();
    (
        store,
        scope,
        ForegroundIdentity {
            process_id: 123,
            boot_id: "11111111-1111-4111-8111-111111111111".into(),
            start_ticks: 456,
            user_id: 1000,
            input: InheritedInput::Unavailable,
        },
    )
}

#[test]
fn foreground_evidence_is_immutable_idempotent_and_fenced() {
    let (mut store, scope, identity) = fixture();
    for expected in [
        SessionTransitionOutcome::Applied,
        SessionTransitionOutcome::Unchanged,
    ] {
        assert_eq!(
            store
                .transaction(|r| r.record_foreground_identity(scope, &identity))
                .unwrap(),
            expected
        );
    }
    let mut replacement = identity.clone();
    replacement.start_ticks += 1;
    assert!(
        store
            .transaction(|r| r.record_foreground_identity(scope, &replacement))
            .is_err()
    );
    assert!(
        store
            .connection
            .execute(
                "UPDATE foreground_process_identity SET identity_json = '{}'",
                []
            )
            .is_err()
    );
    assert_eq!(
        store
            .transaction(
                |r| r.foreground_identity(scope.run_id, scope.session_id)
            )
            .unwrap(),
        Some(identity.clone())
    );
    let stale = SessionScope {
        generation: scope.generation + 1,
        ..scope
    };
    assert_eq!(
        store
            .transaction(|r| r.record_foreground_identity(stale, &identity))
            .unwrap(),
        SessionTransitionOutcome::Stale
    );
    replacement.process_id += 1;
    assert_eq!(
        store
            .transaction(|r| r.record_foreground_identity(scope, &replacement))
            .unwrap(),
        SessionTransitionOutcome::Stale
    );
}

#[test]
fn foreground_evidence_rolls_back_with_failed_startup_observation() {
    let (mut store, scope, identity) = fixture();
    let result: Result<(), super::super::StoreError> = store.transaction(|r| {
        r.record_foreground_identity(scope, &identity)?;
        r.transaction
            .execute_batch("SELECT * FROM injected_missing_table")?;
        Ok(())
    });
    assert!(result.is_err());
    assert!(
        store
            .transaction(
                |r| r.foreground_identity(scope.run_id, scope.session_id)
            )
            .unwrap()
            .is_none()
    );
    assert_eq!(
        store
            .transaction(|r| r.record_foreground_identity(scope, &identity))
            .unwrap(),
        SessionTransitionOutcome::Applied
    );
}

#[test]
fn offline_doctor_preserves_legacy_foreground_uncertainty_and_database_bytes() {
    let database = TestDatabase::new();
    let mut store = Store::open(&database.0).unwrap();
    let mut records = Records::fixture();
    records.session.process_owner = SessionProcessOwner::Foreground;
    records.session.state = LifecycleState::Running;
    records.session.ended_at = None;
    records.session.provider_session_id =
        Some(format!("process:{}", std::process::id()));
    insert_claim_prerequisites(&mut store, &records);
    store
        .transaction(|r| r.insert_session(&records.session))
        .unwrap();
    drop(store);
    let before = std::fs::read(&database.0).unwrap();
    let mut store = Store::open_read_only(&database.0).unwrap();
    for _ in 0..2 {
        let report = crate::doctor::inspect_store(
            &mut store,
            records.run.id,
            database.0.parent().unwrap(),
        )
        .unwrap();
        let check = report
            .checks
            .iter()
            .find(|check| check.check == "foreground_terminal")
            .unwrap();
        assert_eq!(check.status, crate::doctor::CheckStatus::Unavailable);
        assert!(check.message.contains("No foreground process identity"));
    }
    drop(store);
    assert_eq!(std::fs::read(&database.0).unwrap(), before);
}

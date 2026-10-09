use super::*;

fn binding() -> Binding {
    Binding {
        instance: Uuid::from_u128(2),
        installation: Uuid::from_u128(1),
        owner: "501".into(),
        supervisor: "/private/supervisor".into(),
        state_directory: "/private/state".into(),
        device_identity: "existing-device".into(),
    }
}
fn scope(generation: u128) -> Scope {
    Scope {
        installation: Uuid::from_u128(1),
        instance: Uuid::from_u128(2),
        generation: Uuid::from_u128(generation),
    }
}
fn runtime(version: &str) -> Runtime {
    Runtime {
        version: version.into(),
        digest: format!("digest-{version}"),
        location: format!("/private/runtime/{version}"),
    }
}
fn legacy_update(phase: Phase, intent: Intent) -> Update {
    Update {
        binding: binding(),
        previous: runtime("old"),
        candidate: runtime("new"),
        intent,
        phase,
        rolled_back: false,
    }
}
fn with_update(phase: Phase) -> Envelope {
    Envelope::migrate(
        Journal {
            binding: binding(),
            intent: Intent::Enabled,
        },
        Some(legacy_update(phase, Intent::Enabled)),
    )
    .unwrap()
}

#[test]
fn aba_stale_stop_is_refused_even_when_intent_and_binding_match() {
    let g = scope(9);
    let first = Envelope::first(binding());
    let (a, token_a) = first.disable(g.clone()).unwrap();
    assert_eq!((a.revision(), a.intent()), (2, Intent::Disabled));
    a.authorize(&token_a, &g).unwrap();
    let b = a.enable().unwrap();
    assert!(b.stop().is_none());
    let (c, token_c) = b.disable(g.clone()).unwrap();
    assert_eq!((c.revision(), c.intent()), (4, Intent::Disabled));
    assert_eq!(a.binding(), c.binding());
    // The delayed Stop A and the controller's reply A both fail at revision 4.
    assert!(c.authorize(&token_a, &g).is_err());
    assert!(c.complete_stop(&token_a, &g).is_err());
    assert!(b.authorize(&token_a, &g).is_err());
    c.authorize(&token_c, &g).unwrap();
    assert_ne!(
        token_a.operation.operation_id,
        token_c.operation.operation_id
    );
}

#[test]
fn lease_order_enable_waits_then_unregister_still_fails_revision_check() {
    let g = scope(9);
    let (armed, token) = Envelope::first(binding()).disable(g.clone()).unwrap();
    // Server stops under the lease; Enable commits afterwards.
    armed.authorize(&token, &g).unwrap();
    let enabled = armed.enable().unwrap();
    assert!(enabled.complete_stop(&token, &g).is_err());
}

#[test]
fn retry_is_the_exact_stored_token_and_does_not_rewrite() {
    let g = scope(9);
    let (armed, token) = Envelope::first(binding()).disable(g.clone()).unwrap();
    armed.authorize(&token, &g).unwrap();
    armed.authorize(&token.clone(), &g).unwrap();
    assert_eq!(armed.stop(), Some(&token.operation));
    // A new action with the same intent is not a retry.
    let (again, fresh) = armed.disable(g.clone()).unwrap();
    assert_eq!(again.revision(), armed.revision() + 1);
    assert_ne!(fresh, token);
    assert!(again.authorize(&token, &g).is_err());
    // Matching in every field but operation_id, revision, scope or epoch is stale.
    let mut forged = token.clone();
    forged.operation.operation_id = Uuid::new_v4();
    assert!(armed.authorize(&forged, &g).is_err());
    let mut forged = token.clone();
    forged.operation.authorized_revision += 1;
    assert!(armed.authorize(&forged, &g).is_err());
    let mut forged = token.clone();
    forged.journal_epoch = Uuid::new_v4();
    assert!(armed.authorize(&forged, &g).is_err());
    assert!(armed.authorize(&token, &scope(10)).is_err());
}

#[test]
fn missing_stop_denies_even_a_well_formed_token() {
    let g = scope(9);
    let (armed, token) = Envelope::first(binding()).disable(g.clone()).unwrap();
    let completed = armed.complete_stop(&token, &g).unwrap();
    assert!(completed.stop().is_none());
    assert_eq!(completed.revision(), armed.revision() + 1);
    assert_eq!(completed.intent(), Intent::Disabled);
    assert!(completed.authorize(&token, &g).is_err());
    assert!(completed.complete_stop(&token, &g).is_err());
}

#[test]
fn removed_is_terminal_and_disable_cannot_downgrade_it() {
    let g = scope(9);
    let (removed, _) = Envelope::first(binding()).remove(g.clone()).unwrap();
    assert!(removed.enable().is_err());
    let (still, token) = removed.disable(g.clone()).unwrap();
    assert_eq!(still.intent(), Intent::Removed);
    assert_eq!(token.operation.reason, Reason::Remove);
}

#[test]
fn scope_must_match_binding() {
    let mut wrong = scope(9);
    wrong.instance = Uuid::from_u128(77);
    assert!(Envelope::first(binding()).disable(wrong).is_err());
    assert!(Envelope::first(binding()).disable(scope(0)).is_err());
}

#[test]
fn revision_overflow_fails_closed_and_epoch_repair_revokes_old_tokens() {
    let g = scope(9);
    let (armed, token) = Envelope::first(binding()).disable(g.clone()).unwrap();
    let mut spent = armed.clone();
    spent.revision = u64::MAX;
    spent.stop.as_mut().unwrap().authorized_revision = u64::MAX;
    let token_max = StopToken {
        journal_epoch: spent.journal_epoch(),
        operation: spent.stop().unwrap().clone(),
    };
    spent.authorize(&token_max, &g).unwrap();
    for result in [
        spent.enable().map(|_| ()),
        spent.disable(g.clone()).map(|_| ()),
        spent.remove(g.clone()).map(|_| ()),
        spent.arm_recovery(g.clone()).map(|_| ()),
        spent.complete_stop(&token_max, &g).map(|_| ()),
    ] {
        assert!(result.unwrap_err().to_string().contains("exhausted"));
    }
    let repaired = spent.repair_epoch();
    assert_eq!(
        (repaired.revision(), repaired.intent()),
        (1, Intent::Disabled)
    );
    assert_ne!(repaired.journal_epoch(), spent.journal_epoch());
    assert!(repaired.stop().is_none());
    assert!(repaired.authorize(&token, &g).is_err());
    assert!(repaired.authorize(&token_max, &g).is_err());
    let (rearmed, _) = repaired.arm_recovery(g).unwrap();
    assert_eq!(rearmed.revision(), 2);
}

#[test]
fn update_stop_consumes_operation_into_select_and_rollback_keeps_phase() {
    let g = scope(9);
    let envelope = with_update(Phase::Stop);
    let (armed, token) = envelope.arm_update_stop(g.clone()).unwrap();
    let selected = armed.complete_stop(&token, &g).unwrap();
    assert_eq!(selected.update().unwrap().phase, Phase::Select);
    assert!(selected.stop().is_none());
    assert!(selected.authorize(&token, &g).is_err());
    assert!(selected.arm_update_stop(g.clone()).is_err());

    let rollback = with_update(Phase::Rollback);
    assert!(rollback.arm_update_stop(g.clone()).is_err());
    let (armed, token) = rollback.arm_update_rollback(g.clone()).unwrap();
    let done = armed.complete_stop(&token, &g).unwrap();
    assert_eq!(done.update().unwrap().phase, Phase::Rollback);
    assert_eq!(done.intent(), Intent::Enabled);
}

#[test]
fn disable_supersedes_inflight_update_stop_and_stale_updater_is_refused() {
    let g = scope(9);
    let (armed, update_token) = with_update(Phase::Stop).arm_update_stop(g.clone()).unwrap();
    let (disabled, token) = armed.disable(g.clone()).unwrap();
    assert!(disabled.authorize(&update_token, &g).is_err());
    assert!(disabled.complete_stop(&update_token, &g).is_err());
    assert!(disabled.arm_update_stop(g.clone()).is_err());
    assert_eq!(disabled.update().unwrap().phase, Phase::Stop);
    disabled.authorize(&token, &g).unwrap();
}

#[test]
fn recovery_for_a_new_generation_preserves_terminal_intent() {
    let old = scope(9);
    let new = scope(10);
    let (armed, old_token) = Envelope::first(binding()).remove(old.clone()).unwrap();
    assert!(armed.authorize(&old_token, &new).is_err());
    let (rearmed, token) = armed.arm_recovery(new.clone()).unwrap();
    assert_eq!(rearmed.intent(), Intent::Removed);
    assert_eq!(token.operation.reason, Reason::Remove);
    rearmed.authorize(&token, &new).unwrap();
    assert!(rearmed.authorize(&old_token, &old).is_err());
    assert!(Envelope::first(binding()).arm_recovery(new).is_err());
}

#[test]
fn migration_keeps_lifecycle_intent_update_payload_and_fabricates_no_stop() {
    for intent in [Intent::Enabled, Intent::Disabled, Intent::Removed] {
        let journal = Journal {
            binding: binding(),
            intent,
        };
        let absent = Envelope::migrate(journal.clone(), None).unwrap();
        assert_eq!((absent.revision(), absent.intent()), (1, intent));
        assert!(absent.update().is_none() && absent.stop().is_none());
        assert!(!absent.journal_epoch().is_nil());
        for phase in [
            Phase::Stop,
            Phase::Select,
            Phase::Start,
            Phase::Check,
            Phase::Rollback,
            Phase::Complete,
        ] {
            // The legacy update's own stale Enabled is never imported.
            let mut update = legacy_update(phase, Intent::Enabled);
            update.rolled_back = true;
            let migrated = Envelope::migrate(journal.clone(), Some(update)).unwrap();
            let state = migrated.update().unwrap();
            assert_eq!(migrated.intent(), intent);
            assert_eq!(state.phase, phase);
            assert!(state.rolled_back && !state.update_id.is_nil());
            assert_eq!(state.candidate, runtime("new"));
            assert!(migrated.stop().is_none() && migrated.revision() == 1);
        }
    }
}

#[test]
fn migration_rejects_mismatched_binding_and_never_reuses_epoch() {
    let journal = Journal {
        binding: binding(),
        intent: Intent::Enabled,
    };
    let mut update = legacy_update(Phase::Stop, Intent::Enabled);
    update.binding.device_identity = "other-device".into();
    assert!(Envelope::migrate(journal.clone(), Some(update)).is_err());
    let a = Envelope::migrate(journal.clone(), None).unwrap();
    let b = Envelope::migrate(journal, None).unwrap();
    assert_ne!(a.journal_epoch(), b.journal_epoch());
}

#[test]
fn serialization_round_trips_and_rejects_unsafe_documents() {
    let g = scope(9);
    let (armed, _) = with_update(Phase::Stop).arm_update_stop(g).unwrap();
    assert_eq!(
        Envelope::from_slice(&armed.to_vec().unwrap()).unwrap(),
        armed
    );

    let value = serde_json::to_value(&armed).unwrap();
    let mutate = |f: &dyn Fn(&mut serde_json::Value)| {
        let mut v = value.clone();
        f(&mut v);
        Envelope::from_slice(&serde_json::to_vec(&v).unwrap())
    };
    assert!(mutate(&|_| {}).is_ok());
    assert!(mutate(&|v| v["schema"] = 3.into()).is_err());
    assert!(mutate(&|v| v["schema"] = 1.into()).is_err());
    assert!(mutate(&|v| v["extra"] = true.into()).is_err());
    assert!(mutate(&|v| v["journal_epoch"] = Uuid::nil().to_string().into()).is_err());
    assert!(mutate(&|v| v["revision"] = 0.into()).is_err());
    assert!(mutate(&|v| v["revision"] = 99.into()).is_err());
    assert!(mutate(&|v| v["stop"]["authorized_revision"] = 1.into()).is_err());
    assert!(
        mutate(&|v| v["stop"]["scope"]["instance"] = Uuid::from_u128(5).to_string().into())
            .is_err()
    );
    assert!(mutate(&|v| v["intent"] = "Disabled".into()).is_err());
    assert!(Envelope::from_slice(b"{}").is_err());
    assert!(Envelope::from_slice(&[]).is_err());
}

#[test]
fn rollback_stop_survives_disable_while_update_stop_does_not() {
    let g = scope(9);
    let (disabled, _) = with_update(Phase::Rollback).disable(g.clone()).unwrap();
    assert_eq!(disabled.intent(), Intent::Disabled);
    let (armed, token) = disabled.arm_update_rollback(g.clone()).unwrap();
    armed.authorize(&token, &g).unwrap();
    let done = armed.complete_stop(&token, &g).unwrap();
    assert_eq!(done.intent(), Intent::Disabled);

    let (disabled, _) = with_update(Phase::Stop).disable(g.clone()).unwrap();
    assert!(disabled.arm_update_stop(g).is_err());
}

#[test]
fn begin_update_and_phase_changes_are_new_revisions_that_kill_stale_stops() {
    let g = scope(9);
    let first = Envelope::first(binding());
    let started = first
        .begin_update(runtime("old"), runtime("new"))
        .unwrap()
        .unwrap();
    assert_eq!(started.revision(), 2);
    assert_eq!(started.update().unwrap().phase, Phase::Stop);
    // The same update again is a retry that writes nothing; another one is refused.
    assert!(started
        .begin_update(runtime("old"), runtime("new"))
        .unwrap()
        .is_none());
    assert!(started
        .begin_update(runtime("old"), runtime("other"))
        .is_err());
    let (armed, token) = started.arm_update_stop(g.clone()).unwrap();
    let moved = armed.set_update_phase(Phase::Select, false).unwrap();
    assert!(moved.stop().is_none() && moved.revision() == armed.revision() + 1);
    assert!(moved.authorize(&token, &g).is_err());
    assert!(first.set_update_phase(Phase::Select, false).is_err());
    // A completed update must name the committed selection; each update is new.
    let done = moved.set_update_phase(Phase::Complete, false).unwrap();
    assert!(done.begin_update(runtime("old"), runtime("x")).is_err());
    let again = done
        .begin_update(runtime("new"), runtime("x"))
        .unwrap()
        .unwrap();
    assert_ne!(
        again.update().unwrap().update_id,
        started.update().unwrap().update_id
    );
    let (removed, _) = first.remove(g).unwrap();
    assert!(removed
        .begin_update(runtime("old"), runtime("new"))
        .is_err());
}

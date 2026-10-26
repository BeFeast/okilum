use super::*;
use gpui_component::input::projection::SourceStamp;
use okilum_core::open_diagnostics::{capture, Report};
use serde_json::json;

fn input(text: &str) -> Input {
    Input {
        binding: Binding {
            owner: Owner::new(EntityId::from(1_u64)),
            workspace: json!({"brain_id":"brain", "root":"/isolated", "settings":{"format":1}}),
            path: "notes/current.md".into(),
        },
        source: SourceSnapshot {
            stamp: SourceStamp {
                document: 42,
                generation: 1,
            },
            text: Arc::from(text),
        },
    }
}
fn session(input: &Input) -> Session {
    Session::new(input.binding.owner.clone())
}
fn publish(session: &mut Session, input: &Input) -> Arc<Inventory> {
    let request = session.begin(Some(input.clone())).unwrap();
    session.adopt(request.compute(), Some(input)).unwrap()
}
fn no_parse(report: Report) {
    assert_eq!(
        report.counts.get("link_parse_bytes").copied().unwrap_or(0),
        0
    );
    assert_eq!(report.phases.get("parse").map_or(0, |m| m.calls), 0);
}
fn cold(report: Report, bytes: usize) {
    assert_eq!(report.counts["link_parse_bytes"], bytes as u64);
    assert!(
        report.phases["parse"].calls > 0,
        "actual parser positive control"
    );
}

#[test]
fn complete_producer_metadata_and_legacy_conversion_match_without_range_promotion() {
    let mut seen = [false; 4];
    for text in [
        "[**label**](target.md \"A title\") and [[note#Heading|alias]]\n",
        "| a | b |\n| - | - |\n| [[note|aliased]] | [[other\\|alias]] |\n",
        "[literal](path \"[[not|a wiki]]\") and [[folder/name#Part|e\u{301} 🧠]]\n",
        "[forward][id]\n\n[id]: first.md 'first'\n[id]: duplicate.md 'second'\n",
        "\u{feff}---\r\nname: value\r\n---\r\n\r\n> [👩‍💻 🇮🇱](target.md#Section)\r\n\r\nend\r\n",
        "```md\n[ignored](inside.md)\n```\n\n![image](image.png)\n\n[actual](outside.md)\n",
        "[r]: /u\npara [a](x.md) [b](y.md)\n",
        "[foo]: /url\n===\n[foo]\n",
        "[r]: /u\n\npara [a](x.md)\n",
        "> quote [a\n> b](x.md)\n",
    ] {
        let input = input(text);
        let mut session = session(&input);
        let inventory = publish(&mut session, &input);
        let view = inventory.view(&input).unwrap();
        let direct = document_links::parse(text);
        let legacy = super::super::CachedProvider::classify(input.source.clone());
        assert_eq!(view.parsed_links(), direct);
        assert_eq!(view.legacy_links(), legacy.note_links());
        for (full, old) in view.parsed_links().iter().zip(view.legacy_links()) {
            seen[0] |= full.exact_range;
            seen[1] |= !full.exact_range;
            seen[2] |= !full.title.is_empty();
            seen[3] |= full.wiki;
            // Compatibility ranges stay exactly legacy; they are NOT a new
            // validity certificate for ambiguous authored positions.
            assert_eq!(old.range, full.range);
            assert_eq!(old.label, full.range);
            assert_eq!(old.target, full.target);
            assert_eq!(old.wiki, full.wiki);
        }
    }
    assert_eq!(
        seen, [true; 4],
        "exact/inexact/title/wiki nonempty controls"
    );
}

fn foreign_inputs(original: &Input) -> Vec<Input> {
    let mut entity = original.clone();
    entity.binding.owner = Owner::new(EntityId::from(2_u64));
    let mut reopened = original.clone();
    reopened.binding.owner = Owner::new(EntityId::from(1_u64));
    let mut workspace = original.clone();
    // Same brain id/display/root, different complete semantic workspace value.
    workspace.binding.workspace["settings"]["format"] = json!(2);
    let mut path = original.clone();
    path.binding.path = "notes/other.md".into();
    let mut document = original.clone();
    document.source.stamp.document += 1;
    let mut generation = original.clone();
    generation.source.stamp.generation += 1;
    let mut bytes = original.clone();
    bytes.source.text = Arc::from("[changed](other.md)\n");
    vec![
        entity, reopened, workspace, path, document, generation, bytes,
    ]
}

#[test]
fn adoption_queries_and_held_views_check_the_complete_live_binding_and_source() {
    let input = input("[a](target.md)\n");
    let mut equal_value = input.clone();
    equal_value.source.text = Arc::from(input.source.text.to_string());
    assert!(!Arc::ptr_eq(&equal_value.source.text, &input.source.text));
    let mut session = session(&input);
    let request = session.begin(Some(input.clone())).unwrap();
    let held = session
        .adopt(request.compute(), Some(&equal_value))
        .unwrap();
    assert!(
        session.current(Some(&equal_value)).is_some(),
        "exact equality admits a new allocation"
    );
    for foreign in foreign_inputs(&input) {
        let mut session = self::session(&input);
        let pending = session.begin(Some(input.clone())).unwrap().compute();
        let (_, report) = capture(|| {
            assert!(session.adopt(pending, Some(&foreign)).is_err());
            assert!(session.current(Some(&input)).is_none());
        });
        no_parse(report);
        publish(&mut session, &input);
        assert!(session.current(Some(&foreign)).is_none());
        assert!(
            session.current(Some(&input)).is_none(),
            "query mismatch cannot resurrect an older current"
        );
        assert!(held.view(&foreign).is_err());
    }
    assert!(!held.view(&input).unwrap().parsed_links().is_empty());
    assert!(session.current(None).is_none());
    assert!(session.current(Some(&input)).is_none());
}

#[test]
fn actual_lifetime_and_request_owners_reject_identical_input_and_sequence_deliveries() {
    let input = input("[a](target.md)\n");
    let mut a = session(&input);
    let mut b = session(&input); // Same real app owner, independent request session.
    let a_request = a.begin(Some(input.clone())).unwrap();
    let b_request = b.begin(Some(input.clone())).unwrap();
    assert!(a.adopt(b_request.compute(), Some(&input)).is_err());
    assert!(
        a.adopt(a_request.compute(), Some(&input)).is_err(),
        "failed foreign delivery withdrew pending admission"
    );
    let held = publish(&mut a, &input);
    let foreign = b.begin(Some(input.clone())).unwrap().compute();
    assert!(a.adopt(foreign, Some(&input)).is_err());
    assert!(
        a.current(Some(&input)).is_none(),
        "foreign rejection must not issue old current rights"
    );
    let delayed = {
        let mut closed = session(&input);
        closed.begin(Some(input.clone())).unwrap().compute()
    }; // Real lifetime ends by dropping the session, no synthetic close operation.
    let mut reopened_input = input.clone();
    reopened_input.binding.owner = Owner::new(EntityId::from(1_u64));
    let mut reopened = session(&reopened_input);
    let request = reopened.begin(Some(reopened_input.clone())).unwrap();
    assert!(reopened.adopt(delayed, Some(&reopened_input)).is_err());
    assert!(reopened
        .adopt(request.compute(), Some(&reopened_input))
        .is_err());
    assert!(held.view(&reopened_input).is_err());
    assert!(held.view(&input).is_ok());
    // The original session also refuses a new lifetime supplied as live input.
    assert!(a.begin(Some(reopened_input)).is_err());
}

#[test]
fn every_attempt_supersedes_before_refusal_and_old_packets_cannot_restore_current() {
    let input = input("[a](target.md)\n");
    let mut session = session(&input);
    let held = publish(&mut session, &input);
    let mut oversize = input.clone();
    oversize.source.text = Arc::from("x".repeat(source_classifier::MAX_BYTES + 1));
    let mut foreign_owner = input.clone();
    foreign_owner.binding.owner = Owner::new(EntityId::from(2_u64));
    for refused in [None, Some(oversize), Some(foreign_owner)] {
        let pending = session.begin(Some(input.clone())).unwrap().compute();
        let (_, report) = capture(|| {
            assert!(session.begin(refused).is_err());
            assert!(session.adopt(pending, Some(&input)).is_err());
            assert!(session.current(Some(&input)).is_none());
        });
        no_parse(report);
    }
    let pending = session.begin(Some(input.clone())).unwrap();
    assert!(
        session.current(Some(&input)).is_none(),
        "partial/pending work has no current constructor"
    );
    let done = pending.compute();
    session.adopt(done.clone(), Some(&input)).unwrap();
    assert!(
        session.adopt(done, Some(&input)).is_err(),
        "duplicate complete packet is not a new request"
    );
    assert!(session.current(Some(&input)).is_none());
    assert!(held.view(&input).is_ok());
}

#[test]
fn two_unpainted_revisions_and_retries_never_become_current_by_old_delivery() {
    let input = input("[one](one.md)\n");
    let mut session = session(&input);
    let held = publish(&mut session, &input);
    let mut second = input.clone();
    second.source.stamp.generation += 1;
    second.source.text = Arc::from("prefix [two](two.md)\n");
    let mut third = second.clone();
    third.source.stamp.generation += 1;
    third.source.text = Arc::from("more prefix [three](three.md)\n");
    let second_request = session.begin(Some(second.clone())).unwrap();
    let third_request = session.begin(Some(third.clone())).unwrap();
    let second_done = second_request.compute();
    assert!(session.adopt(second_done, Some(&third)).is_err());
    assert!(
        session
            .adopt(third_request.compute(), Some(&third))
            .is_err(),
        "failure withdrew admission; reschedule explicitly"
    );
    let third_done = session.begin(Some(third.clone())).unwrap().compute();
    let third_inventory = session.adopt(third_done, Some(&third)).unwrap();
    assert_eq!(
        third_inventory.view(&third).unwrap().parsed_links()[0].target,
        "three.md"
    );
    assert!(held.view(&third).is_err());
    assert!(held.view(&input).is_ok());
    let request = session.begin(Some(third.clone())).unwrap();
    let retry = request.clone().compute();
    let original = request.compute();
    session.adopt(original, Some(&third)).unwrap();
    assert!(session.adopt(retry, Some(&third)).is_err());
    assert!(session.current(Some(&third)).is_none());
}

#[test]
fn existing_recorder_separates_real_cold_repeated_work_from_reads_and_failed_adoption() {
    let input = input("[a](target.md 'title') and [[note|label]]\n");
    let mut session = session(&input);
    let (request, report) = capture(|| session.begin(Some(input.clone())).unwrap());
    no_parse(report);
    let retry = request.clone();
    let (completion, report) = capture(|| request.compute());
    cold(report, input.source.text.len());
    let (retry, report) = capture(|| retry.compute());
    cold(report, input.source.text.len());
    let (inventory, report) = capture(|| session.adopt(completion, Some(&input)).unwrap());
    no_parse(report);
    let (_, report) = capture(|| {
        for _ in 0..4 {
            let current = session.current(Some(&input)).unwrap();
            assert!(Arc::ptr_eq(&current, &inventory));
            let view = current.view(&input).unwrap();
            assert!(!view.parsed_links().is_empty());
            assert!(!view.legacy_links().is_empty());
        }
        assert!(session.adopt(retry, Some(&input)).is_err());
        assert!(session.current(Some(&input)).is_none());
        assert!(inventory.view(&input).is_ok());
    });
    no_parse(report);
}

#[test]
fn existing_byte_cap_is_preserved_without_changing_authored_input() {
    let prefix = "[a](target.md)\n";
    let text = format!(
        "{prefix}{}",
        "x".repeat(source_classifier::MAX_BYTES - prefix.len())
    );
    let input = input(&text);
    assert_eq!(input.source.text.len(), source_classifier::MAX_BYTES);
    let mut session = session(&input);
    let inventory = publish(&mut session, &input);
    assert!(!inventory.view(&input).unwrap().parsed_links().is_empty());
    let mut oversize = input.clone();
    oversize.source.text = Arc::from(format!("{text}x"));
    let (_, report) = capture(|| {
        assert!(matches!(
            session.begin(Some(oversize)),
            Err(Error::SourceLimit)
        ));
        assert!(session.current(Some(&input)).is_none());
    });
    no_parse(report);
    assert_eq!(input.source.text.as_ref(), text);
}

#[test]
fn outer_pending_key_check_discards_old_work_without_revoking_the_latest_request() {
    let first = input("[first](first.md)\n");
    let mut session = session(&first);
    let old = session.begin(Some(first.clone())).unwrap();
    let old_key = old.key();
    let mut latest = first.clone();
    latest.source.stamp.generation += 1;
    latest.source.text = Arc::from("[latest](latest.md)\n");
    let request = session.begin(Some(latest.clone())).unwrap();
    let key = request.key();
    let obsolete = old.compute();
    let (_, report) = capture(|| {
        assert!(!session.is_pending(&old_key, Some(&latest)));
        assert!(session.is_pending(&key, Some(&latest)));
        drop(obsolete); // Real outer consumer must NOT call destructive adopt here.
        assert!(session.is_pending(&key, Some(&latest)));
    });
    no_parse(report);
    let inventory = session.adopt(request.compute(), Some(&latest)).unwrap();
    assert_eq!(
        inventory.view(&latest).unwrap().parsed_links()[0].target,
        "latest.md"
    );
}

use super::*;

#[test]
fn large2804_twenty_mib_round_trip_preserves_authored_tokens() {
    let raw = format!(
        "\u{feff}{{bare: \"{}\", \"duplicate\":1.50, \"duplicate\":-0, \"escaped\":\"\\u0041\"\r\n",
        "x".repeat(20 * 1024 * 1024)
    );
    let repair = repair("json", &raw).unwrap();
    assert_eq!(repair.source, raw);
    assert!(repair.proposed.contains("\"bare\":"));
    assert_eq!(json_validation(&repair.proposed), Ok(()));
    let Formatted::Changed(formatted) = format("json", &repair.proposed) else {
        panic!("large format refused")
    };
    assert!(formatted.starts_with('\u{feff}'));
    assert!(formatted.ends_with("\r\n"));
    assert_eq!(formatted.matches("\"duplicate\"").count(), 2);
    for token in ["1.50", "-0", "\\u0041"] {
        assert!(formatted.contains(token));
    }
    assert!(!formatted.replace("\r\n", "").contains('\n'));
    assert!(formatted.len() <= MAX_OUTPUT_BYTES);
    assert_eq!(json_validation(&formatted), Ok(()));
    assert_eq!(
        format("json", &formatted),
        Formatted::Unchanged("Already formatted.".into())
    );
}

#[test]
fn large2804_repair_is_linear_for_many_bare_keys() {
    let source = format!("{{{}last:2", "key:1,".repeat(600_000));
    let started = std::time::Instant::now();
    let repaired = repair("json", &source).unwrap();
    assert_eq!(repaired.proposed.matches("\"key\":1,").count(), 600_000);
    assert!(repaired.proposed.ends_with("\"last\":2}"));
    assert_eq!(json_validation(&repaired.proposed), Ok(()));
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "linear 3.6 MB Repair took {:?}",
        started.elapsed()
    );
}

#[test]
fn large2804_shared_limits_refuse_once_without_invalid_claims() {
    let source = " ".repeat(MAX_BYTES + 1);
    let Formatted::Unchanged(format_reason) = format("json", &source) else {
        panic!()
    };
    let repair = repair("json", &source).unwrap_err();
    let validate = json_validation_controlled(&source, &Control::default())
        .unwrap()
        .unwrap_err();
    for message in [format_reason, repair, validate] {
        assert!(
            message.contains("64 MiB") && message.contains("67108865"),
            "{message}"
        );
        assert!(!message.to_lowercase().contains("invalid"));
        assert_eq!(message.matches("this file").count(), 1);
    }
    let deep = format!("{}0{}", "[".repeat(129), "]".repeat(129));
    let reason = json_validation_controlled(&deep, &Control::default())
        .unwrap()
        .unwrap_err();
    assert!(reason.contains("128 levels"));
    assert!(!reason.contains("Invalid"));
    // Expansion beyond 64 MiB is rejected before output allocation.
    let expanding = format!(
        "{}{}0{}",
        "[".repeat(96),
        "0,".repeat(400_000),
        "]".repeat(96)
    );
    assert!(
        matches!(format("json", &expanding), Formatted::Unchanged(reason) if reason.contains("results up to 64 MiB"))
    );
    assert_eq!(
        json_validation_controlled(&expanding, &Control::default()),
        Some(Ok(Ok(()))),
        "validation does not allocate or inherit the pretty-output expansion"
    );
}

#[test]
fn large2804_maximum_input_memory_envelope() {
    // Run as a separate native process under /usr/bin/time: the complete input,
    // original repair, proposal and formatted output are live simultaneously.
    let source = format!("{{bare:\"{}\"", "x".repeat(MAX_BYTES - 128));
    let repaired = repair("json", &source).unwrap();
    assert!(repaired.proposed.len() <= MAX_BYTES);
    let Formatted::Changed(formatted) = format("json", &repaired.proposed) else {
        panic!()
    };
    assert!(formatted.len() <= MAX_OUTPUT_BYTES);
    assert_eq!(json_validation(&formatted), Ok(()));
    assert_eq!(
        formatted.bytes().filter(|b| *b == b'x').count(),
        MAX_BYTES - 128
    );
}

#[test]
fn large2804_cancel_never_returns_a_partial_result() {
    let source = format!("{{key:\"{}\"", "x".repeat(4 * 1024 * 1024));
    let control = Control::default();
    control.cancel();
    assert!(repair_controlled("json", &source, &control).is_none());
    assert!(format_controlled("json", &source, LineEnding::Lf, &control).is_none());
    assert!(json_validation_controlled(&source, &control).is_none());
    // Cancellation during quoted-string output, after the preallocation scan.
    let source = format!("\"{}\"", "x".repeat(1024 * 1024));
    let calls = std::cell::Cell::new(0);
    let stop = || {
        calls.set(calls.get() + 1);
        calls.get() > 70
    };
    assert_eq!(
        crate::json_view::pretty_controlled(&source, &stop),
        Ok(None)
    );
    assert!(calls.get() > 70);
    assert!(
        crate::json_view::pretty(&source).is_ok(),
        "positive control: same source really formats"
    );
}

#[test]
fn large2804_cancel_pulse_cannot_publish_a_truncated_string() {
    let source = format!("\"{}\"", "x".repeat(1024 * 1024));
    let calls = std::cell::Cell::new(0);
    let pulse = || {
        calls.set(calls.get() + 1);
        calls.get() == 70
    };
    assert!(
        crate::json_view::pretty_controlled(&source, &pulse)
            .unwrap()
            .is_none(),
        "a cancellation pulse must not publish a prefix"
    );
}

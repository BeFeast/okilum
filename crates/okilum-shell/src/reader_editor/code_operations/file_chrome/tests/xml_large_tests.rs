//! Functional XML acceptance through the actual code_operations worker and host.
use super::*;
use ::core::prelude::v1::test;
use gpui_component::input::Redo;

fn exact(actual: &str, expected: &str, label: &str) {
    let a = actual.as_bytes();
    let b = expected.as_bytes();
    let first = a
        .iter()
        .zip(b)
        .position(|(a, b)| a != b)
        .unwrap_or(a.len().min(b.len()));
    assert!(
        a == b,
        "{label}: first mismatch at byte {first}; lengths {} / {}; actual {:?}; expected {:?}",
        a.len(),
        b.len(),
        &a[first..a.len().min(first + 48)],
        &b[first..b.len().min(first + 48)]
    );
}

/// A hand-written layout oracle, not XML's formatter, whitespace stripping or
/// a JSON/token oracle. Every payload/markup byte, including meaningful spaces,
/// entity spelling, attribute spacing, comments and CDATA, has one exact place.
fn corpus() -> (String, String) {
    let declaration = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>";
    let doctype = "<!DOCTYPE records [<!ENTITY declared \"D &amp; E\">]>";
    let root = "<records xmlns=\"urn:okilum:fixture\">";
    let mut source = format!("\u{feff}{declaration}{doctype}{root}");
    let mut expected = format!("\u{feff}{declaration}\r\n{doctype}\r\n{root}\r\n");
    let payload = "λЖשלום🙂 e\u{301} &amp; &#65; ".repeat(28);
    let mixed = "<mixed> before <b a = 'x &amp; y'>שלום🙂</b>\t &declared; &#x5D0; after </mixed>";
    let raw = "<raw><![CDATA[<literal> & raw שלום🙂\r\nline2]]></raw>";
    let comment = "<!-- keep  two  spaces & < > -->";
    let instruction = "<?keep a='b'?>";
    let preserved = "<preserved xml:space='preserve'> \t<x/> \r\n <y/>  </preserved>";
    let prose_start = "<prose xmlns='http://www.w3.org/1999/xhtml'>";
    let prose = "<p><b>a</b><i>b</i></p>";
    for n in 0..3000 {
        let start = format!("<record id='{n:05}' spaced = \"A &quot; B\">");
        source.push_str(&format!("{start}<payload>{payload}</payload>{mixed}{raw}{comment}{instruction}{preserved}{prose_start}{prose}</prose></record>"));
        expected.push_str(&format!("  {start}\r\n    <payload>{payload}</payload>\r\n    {mixed}\r\n    {raw}\r\n    {comment}\r\n    {instruction}\r\n    {preserved}\r\n    {prose_start}\r\n      {prose}\r\n    </prose>\r\n  </record>\r\n"));
    }
    source.push_str("</records>\r\n");
    expected.push_str("</records>\r\n");
    assert!(source.len() > 3_500_000 && source.len() < 4 * 1024 * 1024);
    assert!(expected.len() > source.len());
    (source, expected)
}

fn run_worker(
    reader: &Entity<Reader>,
    input: &Entity<EditorState>,
    visual: &mut VisualTestContext,
    action: Action,
) {
    let (send, gate) = futures::channel::oneshot::channel();
    let before = text(input, visual);
    reader.update_in(visual, |r, window, cx| {
        let scope = r.code_scope(cx).expect("actual XML scope");
        assert_eq!(scope.language, "xml");
        assert_eq!(scope.range.end, before.len());
        assert_eq!(
            scope.range.start,
            usize::from(before.starts_with('\u{feff}')) * 3
        );
        r.editing.as_mut().unwrap().operations.worker_gate = Some(gate);
        r.code_operation(action, window, cx);
        assert!(
            r.editing.as_ref().unwrap().operations.busy,
            "worker admission fired"
        );
    });
    frame(visual);
    exact(
        &text(input, visual),
        &before,
        "held worker cannot apply partial source",
    );
    let (_, status, actions) = drawn(input);
    assert_eq!(status, action.busy_text());
    assert!(
        actions.contains(&"file-operation-cancel"),
        "busy host exposes Cancel"
    );
    send.send(())
        .expect("actual queued worker still owns the gate");
    frame(visual);
    reader.read_with(visual, |r, _| {
        let ops = &r.editing.as_ref().unwrap().operations;
        assert!(
            !ops.busy && ops.running.is_none(),
            "worker delivered to the real host"
        );
    });
}

#[gpui::test]
fn xml2804_multimegabyte_worker_preserves_mixed_markup_and_exact_undo_redo(
    cx: &mut TestAppContext,
) {
    let (source, expected) = corpus();
    let (reader, _file, input, visual, temp) = open(cx, "acceptance.xml", &source);
    let path = temp.path().join("vault/acceptance.xml");
    run_worker(&reader, &input, visual, Action::Validate);
    assert!(drawn(&input).1.contains("No syntax problems found"));
    exact(&text(&input, visual), &source, "Validate is read-only");
    run_worker(&reader, &input, visual, Action::Format);
    exact(&text(&input, visual), &expected, "complete wide Format");
    exact(
        &std::fs::read_to_string(&path).unwrap(),
        &source,
        "no partial canonical write",
    );
    visual.dispatch_action(Undo);
    frame(visual);
    exact(
        &text(&input, visual),
        &source,
        "one Undo restores every source byte",
    );
    visual.dispatch_action(Redo);
    frame(visual);
    exact(
        &text(&input, visual),
        &expected,
        "one Redo restores complete format",
    );
    reader.update(visual, |r, cx| assert!(r.save_source(cx)));
    exact(
        &std::fs::read_to_string(&path).unwrap(),
        &expected,
        "canonical save keeps all XML bytes",
    );
    visual.dispatch_action(Undo);
    frame(visual);
    exact(
        &text(&input, visual),
        &source,
        "save does not split the Undo group",
    );
}

fn plain_once(input: &Entity<EditorState>, expected: &str) {
    let (_, actual, _) = drawn(input);
    assert_eq!(
        actual, expected,
        "one plain refusal, without a host prefix/suffix"
    );
    assert_eq!(actual.matches('.').count(), 1, "one sentence");
    assert!(!actual.to_lowercase().contains("invalid"));
    assert!(!actual.to_lowercase().contains("not valid"));
}

#[gpui::test]
fn xml2804_output_limit_is_one_plain_worker_refusal_and_no_edit(cx: &mut TestAppContext) {
    // 63 KB input, valid below XML's depth limit, but indenting 9,000 levels
    // cannot fit the existing 64 MiB output budget. No huge editor is needed.
    let source = format!("{}{}", "<x>".repeat(9000), "</x>".repeat(9000));
    let (reader, _file, input, visual, temp) = open(cx, "output-limit.xml", &source);
    run_worker(&reader, &input, visual, Action::Validate);
    assert!(
        drawn(&input).1.contains("No syntax problems found"),
        "valid input positive control"
    );
    run_worker(&reader, &input, visual, Action::Format);
    plain_once(
        &input,
        &format!(
            "Format supports results up to 64 MiB; this file is {} bytes.",
            source.len()
        ),
    );
    exact(
        &text(&input, visual),
        &source,
        "output refusal leaves source intact",
    );
    exact(
        &std::fs::read_to_string(temp.path().join("vault/output-limit.xml")).unwrap(),
        &source,
        "output refusal leaves disk intact",
    );
}

#[gpui::test]
fn xml2804_depth_limit_validate_and_format_are_plain_non_verdicts(cx: &mut TestAppContext) {
    let source = format!("{}{}", "<x>".repeat(10001), "</x>".repeat(10001));
    let (reader, _file, input, visual, temp) = open(cx, "depth-limit.xml", &source);
    for (action, name) in [(Action::Validate, "Validate"), (Action::Format, "Format")] {
        run_worker(&reader, &input, visual, action);
        plain_once(
            &input,
            &format!(
                "{name} supports XML nesting up to 10000 levels; this file is {} bytes.",
                source.len()
            ),
        );
        exact(
            &text(&input, visual),
            &source,
            "depth refusal leaves source intact",
        );
    }
    exact(
        &std::fs::read_to_string(temp.path().join("vault/depth-limit.xml")).unwrap(),
        &source,
        "depth refusal leaves disk intact",
    );
}

#[gpui::test]
fn xml2804_multimegabyte_mixed_newline_refusal_is_plain_and_read_only(cx: &mut TestAppContext) {
    let (crlf, _) = corpus();
    let source = crlf.replacen("\r\n", "\n", 1);
    let (reader, _file, input, visual, temp) = open(cx, "mixed-newlines.xml", &source);
    run_worker(&reader, &input, visual, Action::Validate);
    assert!(drawn(&input).1.contains("No syntax problems found"));
    run_worker(&reader, &input, visual, Action::Format);
    plain_once(&input, "Mixed line endings; source unchanged.");
    exact(
        &text(&input, visual),
        &source,
        "mixed-newline refusal leaves meaningful text intact",
    );
    exact(
        &std::fs::read_to_string(temp.path().join("vault/mixed-newlines.xml")).unwrap(),
        &source,
        "mixed-newline refusal leaves disk intact",
    );
}

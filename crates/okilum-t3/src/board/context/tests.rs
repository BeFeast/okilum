use super::*;
struct Area(std::path::PathBuf);
impl Area {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("okilum-board-notes-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(path.join("ops")).unwrap();
        Self(path)
    }
    fn write(&self, path: &str, text: &str) {
        std::fs::write(self.0.join(path), text).unwrap();
    }
}
impl Drop for Area {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn question(body: &str) -> String {
    format!("---\nboard_context:\n  kind: decision\n  title: Choose the next action\n  reason: Two safe alternatives remain\n  consequence: Work waits for a decision\n  issue: 42\n  resolution: answer.md\n---\n{body}\n")
}
fn answer(revision: &str) -> String {
    format!("---\nboard_resolution:\n  decision: question.md\n  revision: {revision}\n  by: Oleg\n  at: '2026-10-30T18:00:00Z'\n  outcome: Choose the first explicit alternative\n---\nThe actual decision and rationale.\n")
}
#[test]
fn exact_resolution_closes_only_that_question_revision_not_new_work_or_old_acceptance() {
    let area = Area::new();
    area.write(
        "ops/board-context.toml",
        "version=1\nnotes=['question.md']\n",
    );
    let original = question("## Alternatives\n- One\n- Two");
    area.write("question.md", &original);
    let mut snapshot = Snapshot::default();
    snapshot.accept(refresh(&area.0), 100);
    assert_eq!(
        snapshot.active(100, 30).len(),
        1,
        "a missing resolution cannot erase a decision"
    );
    let revision = snapshot.notes["question.md"]
        .value
        .as_ref()
        .unwrap()
        .revision
        .clone();
    area.write("answer.md", &answer(&revision));
    snapshot.accept(refresh(&area.0), 101);
    assert!(
        snapshot.active(101, 30).is_empty(),
        "positive control: explicit matching resolution actually closes the question"
    );
    assert_eq!(snapshot.resolutions["answer.md"].last_success, Some(101));
    assert_eq!(
        snapshot.active(200, 30).len(),
        1,
        "stale resolution cannot conceal a currently unconfirmed decision"
    );
    std::fs::remove_file(area.0.join("answer.md")).unwrap();
    snapshot.accept(refresh(&area.0), 102);
    assert_eq!(
        snapshot.active(102, 30).len(),
        1,
        "missing proof retains the old fact but cannot close the question"
    );
    area.write("answer.md", &answer(&revision));
    let changed=question("## Alternatives\nA different question, even if its issue merged and an older build was accepted.");
    area.write("question.md", &changed);
    snapshot.accept(refresh(&area.0), 102);
    let item = snapshot.active(102, 30).remove(0);
    assert!(!item.resolved);
    assert!(item.problem.unwrap().contains("earlier revision"));
    assert_ne!(item.note.unwrap().revision, revision);
}
#[test]
fn missing_renamed_and_changed_notes_retain_inspectable_errors_without_writing_sources() {
    let area = Area::new();
    area.write(
        "ops/board-context.toml",
        "version=1\nnotes=['question.md']\n",
    );
    area.write("question.md", &question("Actual Markdown context"));
    let before = std::fs::read(area.0.join("question.md")).unwrap();
    let stamp = std::fs::metadata(area.0.join("question.md"))
        .unwrap()
        .modified()
        .unwrap();
    let mut snapshot = Snapshot::default();
    snapshot.accept(refresh(&area.0), 100);
    assert_eq!(std::fs::read(area.0.join("question.md")).unwrap(), before);
    assert_eq!(
        std::fs::metadata(area.0.join("question.md"))
            .unwrap()
            .modified()
            .unwrap(),
        stamp
    );
    std::fs::rename(area.0.join("question.md"), area.0.join("renamed.md")).unwrap();
    snapshot.accept(refresh(&area.0), 110);
    let item = snapshot.active(110, 30).remove(0);
    assert!(item.note.is_some());
    assert!(item.problem.as_ref().unwrap().contains("question.md"));
    assert_eq!(
        snapshot.notes["question.md"].last_success,
        Some(100),
        "a fresh binding read cannot freshen a missing note"
    );
    assert!(item.problem.unwrap().contains("10s ago"));
    area.write(
        "ops/board-context.toml",
        "version=1\nnotes=['renamed.md']\n",
    );
    snapshot.accept(refresh(&area.0), 111);
    assert!(
        snapshot.notes.contains_key("renamed.md") && !snapshot.notes.contains_key("question.md")
    );
    assert_eq!(snapshot.active(111, 30)[0].path, "renamed.md");
}
#[test]
fn resolution_reference_is_not_self_referential_and_line_endings_do_not_change_the_question() {
    let original = question("Question **with Markdown**, not a flattened status report.");
    let first = parse_note("question.md", &original).unwrap();
    let linked = parse_note(
        "question.md",
        &original.replace("answer.md", "another-answer.md"),
    )
    .unwrap();
    assert_eq!(
        first.revision, linked.revision,
        "adding a closure link must not invalidate its own target revision"
    );
    let crlf = parse_note("question.md", &original.replace('\n', "\r\n")).unwrap();
    assert_eq!(first.revision, crlf.revision);
    let changed = parse_note(
        "question.md",
        &original.replace("Two safe alternatives", "Three safe alternatives"),
    )
    .unwrap();
    assert_ne!(first.revision, changed.revision);
}
#[test]
fn try_requires_the_exact_delivery_record_and_platform_and_never_uses_old_acceptance_as_resolution()
{
    use delivery::{Build, Channel, Platform, Row};
    let source = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let text=format!("---\nboard_context:\n  kind: try\n  title: Try the result\n  reason: The exact build has this change\n  consequence: Report the observed behavior\n  issue: 42\n  platform: linux\n  channel: beta\ndelivery:\n  - issue: 42\n    pr: 7\n    platform: linux\n    channel: beta\n    build: '100'\n    source: {source}\n    artifact: https://updates.example/100.AppImage\n    scenario: Open the exact Board\n---\nDetailed ordinary Markdown scenario.\n");
    let note = parse_note("try.md", &text).unwrap();
    let evidence = note.evidence[0].clone();
    let item = Item {
        path: "try.md".into(),
        note: Some(note),
        problem: None,
        resolved: false,
    };
    let row = Row {
        track: (Platform::Linux, Channel::Beta),
        build: Some(Build {
            platform: Platform::Linux,
            channel: Channel::Beta,
            build: "100".into(),
            source: source.into(),
            artifact: evidence.artifact.clone(),
            published_at: None,
            release_url: None,
        }),
        state: "available".into(),
        evidence: Some(evidence),
        ready: true,
        accepted: false,
    };
    assert!(
        Snapshot::available(&item, &row),
        "positive control: real matching scenario evidence grants Try"
    );
    let mut other = row.clone();
    other.track.0 = Platform::Macos;
    assert!(!Snapshot::available(&item, &other));
    other = row.clone();
    other.evidence.as_mut().unwrap().build = "101".into();
    assert!(!Snapshot::available(&item, &other));
    other = row.clone();
    other.evidence = None;
    assert!(!Snapshot::available(&item, &other));
    other = row;
    other.ready = false;
    other.accepted = true;
    assert!(!Snapshot::available(&item, &other));
    // A historical acceptance field outside board_context is ordinary note data,
    // never decision closure evidence.
    let old = question("This issue was merged; an earlier scenario worked.")
        .replace(
            "---\nboard_context:",
            "---\naccepted_by: Oleg\naccepted_build: 99\nboard_context:",
        )
        .replace("  resolution: answer.md\n", "");
    let decision = parse_note("question.md", &old).unwrap();
    assert!(decision.meta.resolution.is_none());
}
#[test]
fn next_stage_is_explicit_and_current_scope_is_not_a_supported_binding() {
    let area = Area::new();
    area.write(
        "ops/board-context.toml",
        "version=1\nnotes=[]\nnext_stage='next.md'\n",
    );
    area.write("next.md","---\nboard_context:\n  kind: next_stage\n  title: A later stage\n  reason: Its own outcome\n  consequence: Its own explicit scope\n---\nFuture plan, not current membership.\n");
    let mut snapshot = Snapshot::default();
    snapshot.accept(refresh(&area.0), 100);
    assert!(snapshot.active(100, 30).is_empty());
    assert_eq!(
        snapshot.next(100, 30).unwrap().note.unwrap().meta.kind,
        Kind::NextStage
    );
    area.write(
        "ops/board-context.toml",
        "version=1\nnotes=[]\ncurrent_scope='next.md'\n",
    );
    assert!(refresh(&area.0).config.is_err());
    assert!(parse_note(
        "next.md",
        "---\nboard_context: {kind: current_scope}\n---\nNo hidden current-stage section.\n"
    )
    .is_err());
}
#[cfg(unix)]
#[test]
fn context_note_symlinks_cannot_escape_the_project() {
    let area = Area::new();
    let outside = Area::new();
    outside.write("outside.md", &question("Private outside content"));
    area.write("ops/board-context.toml", "version=1\nnotes=['escape.md']\n");
    std::os::unix::fs::symlink(outside.0.join("outside.md"), area.0.join("escape.md")).unwrap();
    let update = refresh(&area.0);
    assert!(update.notes[0].1.as_ref().unwrap_err().contains("escapes"));
}

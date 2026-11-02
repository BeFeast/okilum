use super::*;

// Non-Clone deliberately: production owns GPUI entities and FileEditor.
struct Surface {
    source: String,
    mode: &'static str,
    caret: usize,
    scroll: usize,
    history: Vec<String>,
}

struct Editor {
    draft: String,
    durable: bool,
    conflict: bool,
}
type Host = Panes<Surface, Editor>;

fn identity(path: &str) -> Identity {
    Identity::new(
        std::env::temp_dir().join("okilum-pane-fixture"),
        path.into(),
    )
    .unwrap()
}

fn document(path: &str) -> Document<Surface, Editor> {
    Document::new(
        identity(path),
        Surface {
            source: format!("saved {path}"),
            mode: "Read",
            caret: 0,
            scroll: 0,
            history: vec![path.into()],
        },
    )
}

fn host() -> (Host, PaneId, PaneId) {
    let mut panes = Host::new(document("a.md"));
    let a = panes.active();
    let b = panes.open_beside(document("b.md")).ok().unwrap();
    (panes, a, b)
}

fn edit(panes: &mut Host, id: PaneId) {
    panes
        .claim_editor(id, |_| {
            Ok::<_, ()>(Editor {
                draft: "unsaved exact bytes\r\n".into(),
                durable: false,
                conflict: false,
            })
        })
        .unwrap();
}

// Mock existing lifecycle, not a replacement save algorithm. Durability by
// itself must not permit a revision conflict to leave the active document.
fn navigation_guard(doc: &mut Document<Surface, Editor>) -> Result<(), &'static str> {
    match doc.editor() {
        Some(editor) if editor.conflict => Err("revision conflict"),
        Some(editor) if !editor.durable => Err("recovery failed"),
        _ => Ok(()),
    }
}

#[test]
fn beside_and_focus_retain_independent_surfaces_and_original_editor() {
    let (mut panes, a, b) = host();
    edit(&mut panes, a);
    let original = panes.document_mut(a).unwrap();
    original.surface.mode = "Live Preview";
    original.surface.caret = 41;
    original.surface.scroll = 170;
    original.surface.history.push("earlier.md".into());
    for id in [a, b, a, b] {
        assert!(panes.focus(id));
    }
    assert_eq!(panes.previous(), Some(a));
    let original = panes.document(a).unwrap();
    assert_eq!(original.surface.mode, "Live Preview");
    assert_eq!((original.surface.caret, original.surface.scroll), (41, 170));
    assert_eq!(original.surface.history, ["a.md", "earlier.md"]);
    assert_eq!(original.editor().unwrap().draft, "unsaved exact bytes\r\n");
    let other = panes.document(b).unwrap();
    assert_eq!(
        (
            other.surface.mode,
            other.surface.caret,
            other.surface.scroll
        ),
        ("Read", 0, 0)
    );
    assert_eq!(other.surface.source, "saved b.md");
    assert!(panes.open_beside(document("c.md")).is_err());
    assert_eq!(panes.ids().count(), 2);
}

#[test]
fn same_note_edit_focuses_owner_without_opening_or_copying_draft() {
    let mut panes = Host::new(document("a.md"));
    let a = panes.active();
    edit(&mut panes, a);
    let b = panes.open_beside(document("a.md")).ok().unwrap();
    assert_eq!(
        panes.claim_editor(b, |_| -> Result<Editor, ()> {
            panic!("must not open a competing FileEditor")
        }),
        Err(EditError::Owned(a))
    );
    assert_eq!(panes.active(), a);
    assert!(panes.document(b).unwrap().editor().is_none());
    assert_eq!(panes.document(b).unwrap().surface.source, "saved a.md");
    // A sibling-window lock refusal must not register a phantom owner.
    let (mut other, c, _) = host();
    assert_eq!(
        other.claim_editor(c, |_| Err::<Editor, _>("locked")),
        Err(EditError::Open("locked"))
    );
    assert!(other.document(c).unwrap().editor().is_none());
}

#[test]
fn stale_aba_load_is_dropped_and_current_inactive_pane_load_publishes_once() {
    let (mut panes, a, b) = host();
    let old = panes.begin_load(a, identity("c.md")).unwrap();
    panes.begin_load(a, identity("d.md")).unwrap();
    let current = panes.begin_load(a, identity("c.md")).unwrap();
    assert_eq!(
        panes.complete_load(old, navigation_guard, |_| panic!("stale publish")),
        Err(ExitError::Stale)
    );
    // Positive control: latest completion really applies, in the inactive pane.
    panes
        .complete_load(current, navigation_guard, |surface| {
            surface.source = "accepted c".into();
            surface.history.push("c.md".into());
        })
        .unwrap();
    assert_eq!(panes.active(), b);
    assert_eq!(panes.document(a).unwrap().identity().relative(), "c.md");
    assert_eq!(panes.document(a).unwrap().surface.history, ["a.md", "c.md"]);
    assert_eq!(panes.document(b).unwrap().identity().relative(), "b.md");
    assert_eq!(
        panes.complete_load(current, navigation_guard, |_| panic!("double publish")),
        Err(ExitError::Stale)
    );
}

#[test]
fn load_completion_rechecks_edits_made_during_preparation() {
    let (mut panes, a, _) = host();
    let ticket = panes.begin_load(a, identity("c.md")).unwrap();
    edit(&mut panes, a);
    panes.document_mut(a).unwrap().recovery.offer = true;
    assert_eq!(
        panes.complete_load(ticket, navigation_guard, |_| panic!("unsafe publish")),
        Err(ExitError::Guard("recovery failed"))
    );
    assert_eq!(panes.document(a).unwrap().identity().relative(), "a.md");
    assert!(panes.document(a).unwrap().recovery.offer);
    assert!(panes.document(a).unwrap().editor().is_some());
    assert!(!panes.is_current(ticket));
    panes.document_mut(a).unwrap().editor_mut().unwrap().durable = true;
    let retry = panes.begin_load(a, identity("c.md")).unwrap();
    panes
        .complete_load(retry, navigation_guard, |surface| {
            surface.source = "c".into()
        })
        .unwrap();
    assert!(panes.document(a).unwrap().editor().is_none());
    assert!(!panes.document(a).unwrap().recovery.offer);
}

#[test]
fn unsplit_failure_keeps_layout_draft_focus_and_pending_work_then_retry_closes() {
    let (mut panes, a, b) = host();
    edit(&mut panes, a);
    let ticket = panes.begin_load(a, identity("c.md")).unwrap();
    assert!(matches!(
        panes.unsplit(navigation_guard),
        Err(ExitError::Guard("recovery failed"))
    ));
    assert_eq!(panes.ids().collect::<Vec<_>>(), [a, b]);
    assert_eq!(panes.active(), b);
    assert!(panes.is_current(ticket));
    let editor = panes.document_mut(a).unwrap().editor_mut().unwrap();
    assert_eq!(editor.draft, "unsaved exact bytes\r\n");
    editor.durable = true;
    editor.conflict = true;
    assert!(matches!(
        panes.unsplit(navigation_guard),
        Err(ExitError::Guard("revision conflict"))
    ));
    panes
        .document_mut(a)
        .unwrap()
        .editor_mut()
        .unwrap()
        .conflict = false;
    drop(panes.unsplit(navigation_guard).ok().unwrap());
    assert_eq!(panes.ids().collect::<Vec<_>>(), [b]);
    assert_eq!(panes.active(), b);
    assert_eq!(
        panes.complete_load(ticket, navigation_guard, |_| panic!("closed pane publish")),
        Err(ExitError::Stale)
    );
    assert!(matches!(
        panes.close(b, navigation_guard),
        Err(ExitError::LastPane)
    ));
    // A new pane cannot reuse the closed pane's worker identity.
    let c = panes.open_beside(document("c.md")).ok().unwrap();
    assert_ne!(a, c);
}

#[test]
fn closing_active_pane_focuses_survivor_and_quit_attempts_both_drafts() {
    let (mut panes, a, b) = host();
    edit(&mut panes, a);
    edit(&mut panes, b);
    let mut visited = Vec::new();
    assert_eq!(
        panes.guard_all(|id, document| {
            visited.push(id);
            navigation_guard(document)
        }),
        Err(vec![(a, "recovery failed"), (b, "recovery failed")])
    );
    assert_eq!(visited, [a, b]);
    panes.document_mut(b).unwrap().editor_mut().unwrap().durable = true;
    drop(panes.close(b, navigation_guard).ok().unwrap());
    assert_eq!(panes.active(), a);
    assert!(panes.document(a).unwrap().editor().is_some());
}

#[test]
fn cancelled_load_and_closed_document_work_cannot_publish() {
    let (mut panes, a, b) = host();
    let work = panes.document_ticket(a).unwrap();
    panes.focus(b);
    assert!(panes.accepts_document_work(work));
    let load = panes.begin_load(a, identity("c.md")).unwrap();
    assert!(!panes.accepts_document_work(work));
    assert!(panes.cancel_load(load));
    assert!(!panes.cancel_load(load));
    assert_eq!(
        panes.complete_load(load, navigation_guard, |_| panic!("cancelled publish")),
        Err(ExitError::Stale)
    );
    let work = panes.document_ticket(a).unwrap();
    assert!(panes.accepts_document_work(work));
    drop(panes.close(a, navigation_guard).ok().unwrap());
    assert!(!panes.accepts_document_work(work));
}

#[test]
fn identity_is_vault_scoped_and_rejects_unresolved_paths() {
    let root = std::env::temp_dir().join("okilum-pane-fixture");
    for path in [
        "",
        "../a.md",
        "a/../b.md",
        "/a.md",
        "a//b.md",
        "./a.md",
        "a\\b.md",
        "a\0.md",
    ] {
        assert!(
            Identity::new(root.clone(), path.into()).is_none(),
            "{path:?}"
        );
    }
    let first = identity("notes/a.md");
    let second = Identity::new(root.join("other"), "notes/a.md".into()).unwrap();
    assert_ne!(first, second);
    assert_eq!(first.root(), root);
}

/// These tests use real canonical reads, FileEditor revision checks, journal
/// failures and advisory locks. They are not native split-view acceptance.
#[cfg(unix)]
mod file_documents {
    use super::*;
    use crate::reader_document::Loaded;
    use okilum_core::{
        file_editor::{FileEditor, Save},
        Vault,
    };

    struct Fixture {
        _directory: tempfile::TempDir,
        vault: Vault,
        drafts: std::path::PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let directory = tempfile::tempdir().unwrap();
            let root = directory.path().join("notes");
            std::fs::create_dir(&root).unwrap();
            std::fs::write(root.join("a.md"), "---\ntitle: A\n---\n# A\r\n").unwrap();
            std::fs::write(root.join("b.md"), "# B\n").unwrap();
            let vault = Vault::scan(&root).unwrap();
            let drafts = directory.path().join("drafts");
            Self {
                _directory: directory,
                vault,
                drafts,
            }
        }
        fn identity(&self, relative: &str) -> Identity {
            Identity::new(self.vault.root.clone(), relative.into()).unwrap()
        }
        fn read(&self, relative: &str) -> Loaded {
            Loaded::read(&self.vault, self.identity(relative)).unwrap()
        }
    }

    fn save(document: &mut Loaded) -> Result<(), &'static str> {
        match document.editor_mut().map(FileEditor::save).transpose() {
            Ok(Some(Save::Conflict)) => Err("revision conflict"),
            Err(_) => Err("save failed"),
            _ => Ok(()),
        }
    }

    #[test]
    fn canonical_source_editor_and_duplicate_window_lock_have_one_owner() {
        let fixture = Fixture::new();
        let mut panes = Panes::new(fixture.read("a.md"));
        let a = panes.active();
        panes.claim_source_editor(a, &fixture.drafts).unwrap();
        let exact = "---\ntitle: A\n---\n# A\r\n";
        assert_eq!(panes.document(a).unwrap().editor().unwrap().text(), exact);
        assert_eq!(
            panes
                .document(a)
                .unwrap()
                .surface
                .canonical_source
                .as_deref(),
            Some(exact)
        );
        let duplicate = panes.open_beside(fixture.read("a.md")).ok().unwrap();
        assert!(
            matches!(panes.claim_source_editor(duplicate, &fixture.drafts), Err(EditError::Owned(owner)) if owner == a)
        );
        assert!(FileEditor::open(&fixture.vault.root.join("a.md"), &fixture.drafts).is_err());
        // Positive control: a different document really can acquire its editor.
        let mut sibling = Panes::new(fixture.read("b.md"));
        sibling
            .claim_source_editor(sibling.active(), &fixture.drafts)
            .unwrap();
        assert_eq!(
            sibling
                .document(sibling.active())
                .unwrap()
                .editor()
                .unwrap()
                .text(),
            "# B\n"
        );
    }

    #[test]
    fn external_change_blocks_unsplit_without_clobbering_source_or_draft() {
        let fixture = Fixture::new();
        let mut panes = Panes::new(fixture.read("a.md"));
        let a = panes.active();
        let b = panes.open_beside(fixture.read("b.md")).ok().unwrap();
        panes.claim_source_editor(a, &fixture.drafts).unwrap();
        let draft = "# Exact draft\r\n";
        panes
            .document_mut(a)
            .unwrap()
            .editor_mut()
            .unwrap()
            .set_text(draft.into())
            .unwrap();
        std::fs::write(fixture.vault.root.join("a.md"), "# External\n").unwrap();
        assert!(matches!(
            panes.unsplit(save),
            Err(ExitError::Guard("revision conflict"))
        ));
        assert_eq!(panes.active(), b);
        assert_eq!(panes.ids().count(), 2);
        assert_eq!(panes.document(a).unwrap().editor().unwrap().text(), draft);
        assert_eq!(
            std::fs::read_to_string(fixture.vault.root.join("a.md")).unwrap(),
            "# External\n"
        );
        drop(panes);
        let recovered =
            FileEditor::open(&fixture.vault.root.join("a.md"), &fixture.drafts).unwrap();
        assert_eq!(recovered.text(), draft);
        assert!(recovered.dirty());
    }

    #[test]
    fn recovery_failure_blocks_publication_then_fresh_read_and_save_succeed() {
        let fixture = Fixture::new();
        let mut panes = Panes::new(fixture.read("a.md"));
        let a = panes.active();
        panes.claim_source_editor(a, &fixture.drafts).unwrap();
        let original = std::fs::read_to_string(fixture.vault.root.join("a.md")).unwrap();
        // Break the actual recovery namespace without touching another owner.
        let saved_drafts = fixture.drafts.with_extension("retained");
        std::fs::rename(&fixture.drafts, &saved_drafts).unwrap();
        std::fs::write(&fixture.drafts, "not a directory").unwrap();
        let draft = "# Must survive\r\n";
        let pending = panes
            .document_mut(a)
            .unwrap()
            .editor_mut()
            .unwrap()
            .queue_text(draft.into());
        assert!(pending.persist().is_err());
        let load = panes.begin_load(a, fixture.identity("b.md")).unwrap();
        let prepared = fixture.read("b.md");
        assert_eq!(
            panes.complete_load(load, save, |surface| *surface = prepared.surface),
            Err(ExitError::Guard("save failed"))
        );
        assert_eq!(panes.document(a).unwrap().identity().relative(), "a.md");
        assert_eq!(panes.document(a).unwrap().editor().unwrap().text(), draft);
        assert_eq!(
            std::fs::read_to_string(fixture.vault.root.join("a.md")).unwrap(),
            original
        );
        assert!(!panes.is_current(load));
        // Positive control: after repairing only the synthetic failure, the real
        // editor saves and the fresh ticket publishes the independently read note.
        std::fs::remove_file(&fixture.drafts).unwrap();
        std::fs::rename(saved_drafts, &fixture.drafts).unwrap();
        let load = panes.begin_load(a, fixture.identity("b.md")).unwrap();
        let prepared = fixture.read("b.md");
        panes
            .complete_load(load, save, |surface| *surface = prepared.surface)
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(fixture.vault.root.join("a.md")).unwrap(),
            draft
        );
        assert_eq!(panes.document(a).unwrap().identity().relative(), "b.md");
        assert!(panes.document(a).unwrap().editor().is_none());
    }

    #[test]
    fn failed_read_and_cancelled_preparation_retain_accepted_editor() {
        let fixture = Fixture::new();
        let mut panes = Panes::new(fixture.read("a.md"));
        let a = panes.active();
        panes.claim_source_editor(a, &fixture.drafts).unwrap();
        panes
            .document_mut(a)
            .unwrap()
            .editor_mut()
            .unwrap()
            .set_text("# Retained draft\n".into())
            .unwrap();
        let failed = panes.begin_load(a, fixture.identity("missing.md")).unwrap();
        assert!(Loaded::read(&fixture.vault, fixture.identity("missing.md")).is_err());
        assert!(panes.cancel_load(failed));
        let ticket = panes.begin_load(a, fixture.identity("b.md")).unwrap();
        let prepared = fixture.read("b.md");
        assert!(panes.cancel_load(ticket));
        assert_eq!(
            panes.complete_load(ticket, save, |surface| *surface = prepared.surface),
            Err(ExitError::Stale)
        );
        assert_eq!(
            panes.document(a).unwrap().editor().unwrap().text(),
            "# Retained draft\n"
        );
        assert_eq!(panes.document(a).unwrap().identity().relative(), "a.md");
        let mut other_vault = Vault::from_note_paths([]);
        other_vault.root = fixture.vault.root.join("different");
        assert!(Loaded::read(&other_vault, fixture.identity("a.md")).is_err());
    }

    #[test]
    fn source_open_refuses_symlink_redirection_and_hard_links() {
        use std::os::unix::fs::symlink;
        let fixture = Fixture::new();
        symlink(
            fixture.vault.root.join("a.md"),
            fixture.vault.root.join("alias.md"),
        )
        .unwrap();
        std::fs::hard_link(
            fixture.vault.root.join("b.md"),
            fixture.vault.root.join("hard.md"),
        )
        .unwrap();
        for relative in ["alias.md", "hard.md"] {
            let mut panes = Panes::new(fixture.read(relative));
            assert!(matches!(
                panes.claim_source_editor(panes.active(), &fixture.drafts),
                Err(EditError::Open(_))
            ));
            assert!(panes.document(panes.active()).unwrap().editor().is_none());
        }
        // An outside target may not even enter the document loader.
        let outside = fixture._directory.path().join("outside.md");
        std::fs::write(&outside, "# Outside must not change\n").unwrap();
        symlink(&outside, fixture.vault.root.join("outside.md")).unwrap();
        assert!(Loaded::read(&fixture.vault, fixture.identity("outside.md")).is_err());
        assert_eq!(
            std::fs::read_to_string(outside).unwrap(),
            "# Outside must not change\n"
        );
    }

    #[test]
    fn quitting_attempts_both_real_drafts_and_restart_recovers_each() {
        let fixture = Fixture::new();
        let mut panes = Panes::new(fixture.read("a.md"));
        let a = panes.active();
        let b = panes.open_beside(fixture.read("b.md")).ok().unwrap();
        for (id, text) in [(a, "# Draft A\r\n"), (b, "# Draft B\n")] {
            panes.claim_source_editor(id, &fixture.drafts).unwrap();
            panes
                .document_mut(id)
                .unwrap()
                .editor_mut()
                .unwrap()
                .set_text(text.into())
                .unwrap();
        }
        std::fs::write(fixture.vault.root.join("a.md"), "# External A\n").unwrap();
        std::fs::write(fixture.vault.root.join("b.md"), "# External B\n").unwrap();
        let mut visited = Vec::new();
        assert_eq!(
            panes.guard_all(|id, document| {
                visited.push(id);
                save(document)
            }),
            Err(vec![(a, "revision conflict"), (b, "revision conflict")])
        );
        assert_eq!(visited, [a, b]);
        drop(panes);
        for (relative, text, external) in [
            ("a.md", "# Draft A\r\n", "# External A\n"),
            ("b.md", "# Draft B\n", "# External B\n"),
        ] {
            let path = fixture.vault.root.join(relative);
            let recovered = FileEditor::open(&path, &fixture.drafts).unwrap();
            assert_eq!(recovered.text(), text);
            assert_eq!(std::fs::read_to_string(path).unwrap(), external);
        }
    }
}

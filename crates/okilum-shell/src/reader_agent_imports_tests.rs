//! End-to-end regression checks use only the pre-existing Reader API, so the
//! same tests can be compiled against main as a negative control.
use super::*;
use ::core::prelude::v1::test;

struct Fixture<'a> {
    reader: Entity<Reader>,
    visual: &'a mut VisualTestContext,
    root: PathBuf,
    _directory: tempfile::TempDir,
}
fn open<'a>(cx: &'a mut TestAppContext, note: &str) -> Fixture<'a> {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("vault");
    std::fs::create_dir_all(root.join("project/imports")).unwrap();
    let source = "# Agent instructions\n\nBefore import.\n\n@imports/rules.md\n\nAfter import.\n\n```text\n@not-an-import.md\n```\n";
    for path in ["project/AGENTS.md", "@AgentContract.md"] {
        std::fs::write(root.join(path), source).unwrap();
    }
    std::fs::write(
        root.join("project/imports/rules.md"),
        "[Relative destination](target.md)\n\n**IMPORTED POSITIVE CONTROL**\n\n@nested.md\n",
    )
    .unwrap();
    std::fs::write(
        root.join("project/imports/target.md"),
        "# Correct relative target\n",
    )
    .unwrap();
    std::fs::write(
        root.join("project/imports/nested.md"),
        "# Nested import positive control\n",
    )
    .unwrap();
    // A wrong host-relative resolution has a real competing target.
    std::fs::write(root.join("project/target.md"), "# Wrong host target\n").unwrap();
    cx.update(|cx| {
        crate::test_init(cx);
        bind_keys(cx);
        cx.set_reduce_motion(true);
    });
    let mut reader = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let entity = cx.new(|cx| {
            Reader::new(
                Opts {
                    vault: Some(root.clone()),
                    note: Some(note.into()),
                    session_directory: Some(directory.path().join("state")),
                    index_dir: Some(directory.path().join("index")),
                    ..Default::default()
                },
                window,
                cx,
            )
        });
        reader = Some(entity.clone());
        Root::new(entity, window, cx)
    });
    visual.simulate_resize(size(px(1400.), px(960.)));
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    assert!(
        visual.debug_bounds("reader-column").is_some(),
        "positive control: actual Reader document painted"
    );
    Fixture {
        reader: reader.unwrap(),
        visual,
        root,
        _directory: directory,
    }
}
impl Fixture<'_> {
    fn draw(&mut self) {
        self.visual.run_until_parked();
        self.visual.update(|window, cx| window.draw(cx).clear(cx));
    }
    fn click(&mut self, selector: &'static str) {
        let bounds = self
            .visual
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("missing {selector}"));
        self.visual
            .simulate_click(bounds.center(), Modifiers::default());
        self.draw();
    }
}
#[gpui::test]
fn issue1588_line_click_and_inline_nested_expansion(cx: &mut TestAppContext) {
    let mut f = open(cx, "project/AGENTS.md");
    assert!(f
        .visual
        .debug_bounds("agent-line-not-an-import.md")
        .is_none());
    let label = f
        .visual
        .debug_bounds("agent-line-imports/rules.md")
        .unwrap();
    let disclosure = f
        .visual
        .debug_bounds("agent-disclosure-imports/rules.md")
        .unwrap();
    assert!(
        (label.center().y - disclosure.center().y).abs() <= px(4.),
        "the disclosure and @path are on the same document line"
    );
    f.click("agent-disclosure-imports/rules.md");
    let line = f
        .visual
        .debug_bounds("agent-line-imports/rules.md")
        .unwrap();
    let body = f
        .visual
        .debug_bounds("agent-expanded-imports/rules.md")
        .expect("expansion painted at the directive");
    assert!(body.origin.y >= line.bottom());
    let panel = f
        .reader
        .update(f.visual, |r, cx| r.render_agent_document(cx).is_some());
    assert!(panel, "auxiliary panel remains available");
    f.click("agent-disclosure-nested.md");
    assert!(f.visual.debug_bounds("agent-expanded-nested.md").is_some());
    f.click("agent-line-nested.md");
    assert_eq!(
        f.reader.read_with(f.visual, |r, _| r.current_rel.clone()),
        "project/imports/nested.md"
    );
    assert_eq!(std::fs::read_to_string(f.root.join("project/AGENTS.md")).unwrap(), "# Agent instructions\n\nBefore import.\n\n@imports/rules.md\n\nAfter import.\n\n```text\n@not-an-import.md\n```\n");
}
#[gpui::test]
fn issue1588_expanded_markdown_link_uses_imported_origin(cx: &mut TestAppContext) {
    let mut f = open(cx, "project/AGENTS.md");
    for live in [false, true] {
        if live {
            f.reader.update_in(f.visual, |r, window, cx| {
                r.open_note_at("project/AGENTS.md", None, None, window, cx)
            });
            f.draw();
            f.reader
                .update_in(f.visual, |r, window, cx| r.open_live_preview(window, cx));
            f.draw();
        }
        f.click("agent-disclosure-imports/rules.md");
        let body = f
            .visual
            .debug_bounds("agent-expanded-imports/rules.md")
            .unwrap();
        // The imported document starts with one Markdown link. Click its first
        // line through TextView's real native link handler, not handle_link directly.
        f.visual.simulate_click(
            point(body.origin.x + px(60.), body.origin.y + px(10.)),
            Modifiers::default(),
        );
        f.draw();
        assert_eq!(
            f.reader.read_with(f.visual, |r, _| r.current_rel.clone()),
            "project/imports/target.md"
        );
    }
}
#[gpui::test]
fn issue1588_entered_project_note_and_contract_keep_document_features(cx: &mut TestAppContext) {
    let mut f = open(cx, "@AgentContract.md");
    assert!(okilum_core::quick_open::FileFilter::Default.admits("@AgentContract.md"));
    assert!(!okilum_core::quick_open::FileFilter::AgentFiles.admits("@AgentContract.md"));
    f.reader.update(f.visual, |r, cx| {
        r.project_open = true;
        r.project_navigation.active = Some(okilum_t3::projects::Project {
            folder: r.vault_root.clone(),
            name: "Fixture project".into(),
            kind: okilum_t3::projects::Kind::Code,
            environment: None,
            manager_project: None,
            manager: okilum_t3::projects::Manager::NotStarted,
            appearance: Default::default(),
        });
        cx.notify();
    });
    f.draw();
    assert!(
        f.reader
            .update(f.visual, |r, cx| r.render_agent_document(cx).is_some()),
        "entered-project note retains agent panel"
    );
    assert!(
        f.visual
            .debug_bounds("agent-line-imports/rules.md")
            .is_some(),
        "root contract receives document decorations without changing tree classification"
    );
}
#[gpui::test]
fn issue1588_source_command_click_and_live_preview_disclosure(cx: &mut TestAppContext) {
    let mut f = open(cx, "project/AGENTS.md");
    f.reader
        .update_in(f.visual, |r, window, cx| r.toggle_source(window, cx));
    f.draw();
    let input = f
        .reader
        .read_with(f.visual, |r, _| r.source_input().unwrap());
    let at = input.read_with(f.visual, |input, _| {
        let offset = input.value().find("@imports").unwrap();
        input
            .range_to_bounds(&(offset..offset + 1))
            .unwrap()
            .center()
    });
    f.visual.simulate_click(
        at,
        Modifiers {
            platform: true,
            control: true,
            ..Default::default()
        },
    );
    f.draw();
    assert_eq!(
        f.reader.read_with(f.visual, |r, _| r.current_rel.clone()),
        "project/imports/rules.md"
    );
    f.reader.update_in(f.visual, |r, window, cx| {
        if r.editing.is_some() {
            r.toggle_source(window, cx);
        }
        r.open_note_at("project/AGENTS.md", None, None, window, cx);
    });
    f.draw();
    f.reader
        .update_in(f.visual, |r, window, cx| r.open_live_preview(window, cx));
    f.draw();
    f.click("agent-disclosure-imports/rules.md");
    assert!(f
        .visual
        .debug_bounds("agent-expanded-imports/rules.md")
        .is_some());
    f.visual
        .update(|_, cx| reader_features::set_enabled(reader_features::Feature::Agents, false, cx));
    f.draw();
    assert!(f
        .visual
        .debug_bounds("agent-disclosure-imports/rules.md")
        .is_none());
    f.visual
        .update(|_, cx| reader_features::set_enabled(reader_features::Feature::Agents, true, cx));
    f.draw();
    f.click("agent-disclosure-imports/rules.md");
    assert!(
        f.visual
            .debug_bounds("agent-expanded-imports/rules.md")
            .is_some(),
        "enabling Agents updates the existing Live Preview without a source edit"
    );
}

#[gpui::test]
fn issue1588_imported_task_queries_have_no_editing_authority(cx: &mut TestAppContext) {
    let mut f = open(cx, "project/AGENTS.md");
    let imported = "```tasks\nnot done\n```\n";
    let path = f.root.join("project/imports/rules.md");
    std::fs::write(&path, imported).unwrap();
    f.draw();
    f.click("agent-disclosure-imports/rules.md");
    assert!(
        f.visual.debug_bounds("reader-code-language-0").is_some(),
        "positive control: imported task query is visible as a read-only code block"
    );
    assert!(f.visual.debug_bounds("tasks-empty").is_none());
    assert!(f.visual.debug_bounds("tasks-unavailable").is_none());
    assert_eq!(std::fs::read_to_string(path).unwrap(), imported);
}

use super::*;
use crate::*;
use ::core::prelude::v1::test;

#[test]
fn explicit_creation_never_overwrites_or_creates_parent_folders() {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().canonicalize().unwrap();
    let existing = root.join("existing.md");
    let bytes = b"---\r\ntitle: Keep\r\n---\r\n# Exact bytes\r\n";
    std::fs::write(&existing, bytes).unwrap();
    let (opened, created) = resolve_explicit_path(&existing, None).unwrap();
    assert_eq!(opened, existing);
    assert!(!created);
    assert_eq!(std::fs::read(&existing).unwrap(), bytes);
    let new = root.join("new.MD");
    assert!(
        reader_open::OpenIntent::validate(&new, None, None).is_err(),
        "validation remains read-only"
    );
    assert!(!new.exists());
    let (opened, created) = resolve_explicit_path(&new, None).unwrap();
    assert!(created);
    assert_eq!(opened, new);
    assert_eq!(std::fs::read(&new).unwrap(), b"");
    let absent_parent = root.join("missing-folder/new.md");
    let error = resolve_explicit_path(&absent_parent, None).unwrap_err();
    assert!(reader_open::open_error_text(&error).contains("missing-folder"));
    assert!(reader_open::open_error_text(&error).contains("folder"));
    assert!(!root.join("missing-folder").exists());
    for path in [root.join("not-a-note.txt"), root.join("missing-directory")] {
        assert!(resolve_explicit_path(&path, None).is_err());
        assert!(!path.exists());
    }
    let outside = root.join("outside.md");
    std::fs::create_dir(root.join("scope")).unwrap();
    assert!(resolve_explicit_path(&outside, Some(&root.join("scope"))).is_err());
    assert!(
        !outside.exists(),
        "an invalid explicit-root request creates nothing"
    );
}

fn init(cx: &mut gpui::TestAppContext, state: &Path) {
    cx.update(|cx| {
        gpui_component::init(cx);
        bind_keys(cx);
        reader_open::install(cx);
        cx.set_global(reader_history::TestSessionDirectory(state.to_path_buf()));
        reader_ui_state::install(state, cx);
    });
}

fn settle(visual: &mut gpui::VisualTestContext) {
    visual.run_until_parked();
    visual.executor().advance_clock(Duration::from_millis(300));
    visual.run_until_parked();
}

#[gpui::test]
fn missing_cli_markdown_opens_an_empty_editor_without_a_vault(cx: &mut gpui::TestAppContext) {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().join("folder");
    std::fs::create_dir(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let state = fixture.path().join("state");
    init(cx, &state);
    let existing = root.join("existing.md");
    std::fs::write(&existing, "# Existing positive control\n").unwrap();
    assert!(reader_open::OpenIntent::validate(&existing, None, None).is_ok());
    let path = root.join("new-note.MD");
    assert!(!path.exists());
    let reader_open::Command::Launch(mut opts) =
        reader_open::parse_args([path.clone().into_os_string()], Opts::default()).unwrap()
    else {
        panic!("CLI must dispatch a Reader open");
    };
    opts.session_directory = Some(state);
    opts.index_dir = Some(fixture.path().join("cache"));
    let mut reader = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| Reader::new(*opts, window, cx));
        reader = Some(view.clone());
        Root::new(view, window, cx)
    });
    let reader = reader.unwrap();
    settle(visual);
    reader.read_with(visual, |r, cx| {
        assert!(
            r.loading.is_some(),
            "positive control: requested opener actually ran"
        );
        assert!(
            path.is_file(),
            "CREATE1835: an explicit missing Markdown path must create a file"
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"");
        assert!(
            r.single_file,
            "outside a vault follows the existing quick-view path"
        );
        assert_eq!(r.current_rel, "new-note.MD");
        assert!(r.editing.is_some(), "a newly created note opens its editor");
        assert!(r.source_input().unwrap().read(cx).value().is_empty());
        assert_eq!(
            std::fs::read_to_string(&existing).unwrap(),
            "# Existing positive control\n"
        );
    });
}

#[gpui::test]
fn running_vault_accepts_missing_cli_and_os_paths_without_overwriting(
    cx: &mut gpui::TestAppContext,
) {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().join("vault");
    std::fs::create_dir(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let state = fixture.path().join("state");
    init(cx, &state);
    let existing = root.join("existing.md");
    let bytes = "# Keep this note\n\nOriginal content.\n";
    std::fs::write(&existing, bytes).unwrap();
    let mut reader = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            Reader::new(
                Opts {
                    vault: Some(root.clone()),
                    note: Some("existing.md".into()),
                    session_directory: Some(state),
                    index_dir: Some(fixture.path().join("cache")),
                    ..Default::default()
                },
                window,
                cx,
            )
        });
        reader = Some(view.clone());
        Root::new(view, window, cx)
    });
    let reader = reader.unwrap();
    settle(visual);
    let windows = visual.update(|_, cx| cx.windows().len());
    for (rel, os_event) in [("cli-new.md", false), ("os-new.md", true)] {
        let path = root.join(rel);
        visual.update(|_, cx| {
            if os_event {
                let path = path.to_string_lossy().replace('\\', "/");
                let url = format!(
                    "file://{}{}",
                    if path.starts_with('/') { "" } else { "/" },
                    path
                );
                reader_open::dispatch_urls(vec![url], cx);
            } else {
                reader_open::open_window(
                    Opts {
                        open_path: Some(path.clone()),
                        ..Default::default()
                    },
                    cx,
                )
                .unwrap();
            }
        });
        settle(visual);
        reader.read_with(visual, |r, cx| {
            assert_eq!(
                cx.windows().len(),
                windows,
                "the existing vault window is reused"
            );
            assert_eq!(r.current_rel, rel);
            assert!(!r.single_file);
            assert!(r.editing.is_some());
            assert_eq!(std::fs::read(&path).unwrap(), b"");
            assert_eq!(std::fs::read_to_string(&existing).unwrap(), bytes);
        });
    }
}

//! Mounted production menu item and existing composer, never a synthetic callback.
use super::*;
use ::core::prelude::v1::test;
use futures::channel::oneshot;
use gpui::{TestAppContext, VisualTestContext};
use gpui_component::menu::DropdownMenu as _;

struct Hold {
    root: PathBuf,
    reached: oneshot::Sender<()>,
    release: oneshot::Receiver<()>,
    finished: oneshot::Sender<()>,
}
thread_local! { static HOLD: RefCell<Option<Hold>> = const { RefCell::new(None) }; }

pub(super) async fn pause_prepared(root: &Path) -> Option<oneshot::Sender<()>> {
    let hold = HOLD.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.as_ref().is_some_and(|hold| hold.root == root) {
            slot.take()
        } else {
            None
        }
    });
    let hold = hold?;
    let _ = hold.reached.send(());
    hold.release
        .await
        .expect("fixture releases actual completed preparation");
    Some(hold.finished)
}

// A small menu host mounts the production PopupMenuItem unmodified. The shared
// Reader menu route is the separately cleared two-line integration, not a test hook.
struct MenuHost {
    reader: Option<Entity<Reader>>,
    root: PathBuf,
}
impl Render for MenuHost {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let mut surface = v_flex().size_full();
        if let Some(reader) = &self.reader {
            let weak = reader.downgrade();
            let root = self.root.clone();
            surface = surface
                .child(
                    Button::new("capture-test-menu")
                        .label("Capture menu")
                        .debug_selector(|| "capture-test-menu".into())
                        .dropdown_menu(move |menu, _, cx| {
                            menu.item(menu_item(weak.clone(), root.clone(), cx))
                        }),
                )
                .child(reader.clone());
        }
        surface
    }
}
struct Fixture {
    reader: Entity<Reader>,
    host: Entity<MenuHost>,
    root: PathBuf,
    session: PathBuf,
    original: Vec<u8>,
    _directory: tempfile::TempDir,
}
fn fixture(cx: &mut TestAppContext) -> (Fixture, &mut VisualTestContext) {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("vault");
    let session = directory.path().join("state");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&session).unwrap();
    let original = b"# Untouched\r\nUnicode: \xce\xbb\r\n".to_vec();
    std::fs::write(root.join("start.md"), &original).unwrap();
    cx.update(|cx| {
        crate::test_init(cx);
        bind_keys(cx);
    });
    let mut entities = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let reader = cx.new(|cx| {
            Reader::new(
                Opts {
                    vault: Some(root.clone()),
                    note: Some("start.md".into()),
                    session_directory: Some(session.clone()),
                    index_dir: Some(directory.path().join("index")),
                    panel_settings_override: Some(directory.path().join("panels.json")),
                    ..Default::default()
                },
                window,
                cx,
            )
        });
        let host = cx.new(|_| MenuHost {
            reader: Some(reader.clone()),
            root: root.clone(),
        });
        entities = Some((reader, host.clone()));
        Root::new(host, window, cx)
    });
    let (reader, host) = entities.unwrap();
    visual.simulate_resize(size(px(1400.), px(1000.)));
    visual.run_until_parked();
    (
        Fixture {
            reader,
            host,
            root,
            session,
            original,
            _directory: directory,
        },
        visual,
    )
}
fn menu(visual: &mut VisualTestContext) {
    visual.run_until_parked();
    let bounds = visual
        .debug_bounds("capture-test-menu")
        .expect("real menu trigger painted");
    visual.simulate_click(bounds.center(), Modifiers::default());
    visual.run_until_parked();
    assert!(
        visual.update(|window, cx| window.is_action_available(&gpui_base::actions::SelectDown, cx)),
        "actual PopupMenu focused"
    );
}
fn choose(visual: &mut VisualTestContext) {
    visual.simulate_keystrokes("down enter");
    visual.run_until_parked();
}
fn composer(root: &Path) -> Option<Entity<CaptureComposer>> {
    ENTRIES.with(|entries| {
        entries
            .borrow()
            .iter()
            .find(|((_, path), _)| path == root)
            .and_then(|(_, slot)| slot.composer.as_ref())
            .and_then(WeakEntity::upgrade)
    })
}
fn click(visual: &mut VisualTestContext, selector: &'static str) {
    let bounds = visual
        .debug_bounds(selector)
        .expect("actual capture control painted");
    visual.simulate_click(bounds.center(), Modifiers::default());
    visual.run_until_parked();
}
fn unchanged(f: &Fixture) {
    assert_eq!(std::fs::read(f.root.join("start.md")).unwrap(), f.original);
    assert_eq!(
        std::fs::read_dir(&f.root).unwrap().count(),
        1,
        "opening/checkpointing never publishes notes"
    );
}
fn hold(
    root: &Path,
) -> (
    oneshot::Receiver<()>,
    oneshot::Sender<()>,
    oneshot::Receiver<()>,
) {
    let (reached, waiting) = oneshot::channel();
    let (release, held) = oneshot::channel();
    let (finished, completed) = oneshot::channel();
    HOLD.with(|slot| {
        assert!(slot.borrow().is_none());
        *slot.borrow_mut() = Some(Hold {
            root: root.to_owned(),
            reached,
            release: held,
            finished,
        });
    });
    (waiting, release, completed)
}
async fn open(f: &Fixture, visual: &mut VisualTestContext) -> Entity<CaptureComposer> {
    visual
        .condition(&f.reader, |reader, _| binding(reader).ready)
        .await;
    let (reached, release, finished) = hold(&f.root);
    menu(visual);
    choose(visual);
    reached.await.unwrap();
    assert!(
        composer(&f.root).is_none(),
        "pre-publication hold actually intercepted preparation"
    );
    release.send(()).unwrap();
    finished.await.unwrap();
    let view = composer(&f.root).expect("current owner publishes real composer");
    visual.condition(&view, |view, _| !view.loading).await;
    visual.run_until_parked();
    assert!(visual.debug_bounds("capture-composer-dialog").is_some());
    view
}

#[gpui::test]
async fn capture_ready_menu_close_reopen_keeps_durable_draft(cx: &mut TestAppContext) {
    let (f, visual) = fixture(cx);
    let view = open(&f, visual).await;
    visual.simulate_input("Retained \u{03bb} capture\n");
    visual
        .condition(&view, |view, _| view.persisted == Some(view.generation))
        .await;
    let (text, records) = view.read_with(visual, |view, _| {
        (
            view.draft.text.clone(),
            view.entry_namespace.as_ref().unwrap().records.clone(),
        )
    });
    assert!(!text.is_empty(), "real focused input reached composer");
    click(visual, "capture-cancel");
    visual
        .condition(&view, |view, _| view.entry_retained.is_none())
        .await;
    assert!(visual.debug_bounds("capture-composer-dialog").is_none());
    let recovered = capture::load_recovery(&f.root, &records).unwrap();
    assert!(recovered.drafts.iter().any(|draft| draft.text == text));
    let reopened = open(&f, visual).await;
    reopened.read_with(visual, |view, _| {
        assert!(view.recovered.iter().any(|draft| draft.text == text))
    });
    unchanged(&f);
}

async fn stale(cx: &mut TestAppContext, kind: &str) {
    let (f, visual) = fixture(cx);
    visual
        .condition(&f.reader, |reader, _| binding(reader).ready)
        .await;
    let (reached, release, finished) = hold(&f.root);
    menu(visual);
    choose(visual);
    reached.await.unwrap();
    assert!(composer(&f.root).is_none());
    match kind {
        "root" => f.reader.update(visual, |reader, cx| {
            reader.vault_root = f.root.with_file_name("other-vault");
            cx.notify();
        }),
        "session" => f.reader.update(visual, |reader, cx| {
            reader.session_directory = Some(f.session.with_file_name("other-state"));
            cx.notify();
        }),
        "reader" => {
            f.host.update(visual, |host, cx| {
                host.reader = None;
                cx.notify();
            });
            // Root/session cases exercise a still-live captured Reader. This
            // case additionally destroys the actual asking entity below.
        }
        _ => unreachable!(),
    }
    if kind == "reader" {
        let root = f.root.clone();
        let original = f.original.clone();
        drop(f.reader);
        release.send(()).unwrap();
        finished.await.unwrap();
        assert!(composer(&root).is_none());
        assert_eq!(std::fs::read(root.join("start.md")).unwrap(), original);
    } else {
        release.send(()).unwrap();
        finished.await.unwrap();
        assert!(
            composer(&f.root).is_none(),
            "stale completion cannot publish a composer"
        );
        assert!(!visual.update(|window, cx| window.has_active_dialog(cx)));
        unchanged(&f);
    }
}
#[gpui::test]
async fn capture_ready_root_rebind_after_preparation_refuses(cx: &mut TestAppContext) {
    stale(cx, "root").await;
}
#[gpui::test]
async fn capture_ready_session_rebind_after_preparation_refuses(cx: &mut TestAppContext) {
    stale(cx, "session").await;
}
#[gpui::test]
async fn capture_ready_reader_release_after_preparation_refuses(cx: &mut TestAppContext) {
    stale(cx, "reader").await;
}

#[gpui::test]
async fn capture_ready_failed_checkpoint_retains_composer_and_text(cx: &mut TestAppContext) {
    let (f, visual) = fixture(cx);
    let view = open(&f, visual).await;
    visual.simulate_input("Do not discard on storage failure");
    visual
        .condition(&view, |view, _| view.persisted == Some(view.generation))
        .await;
    let (text, identity) = view.read_with(visual, |view, _| {
        (
            view.draft.text.clone(),
            view.entry_namespace
                .as_ref()
                .unwrap()
                .records
                .join("identity.json"),
        )
    });
    let bytes = std::fs::read(&identity).unwrap();
    let mut changed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    changed["root"] = serde_json::json!(f.root.with_file_name("foreign"));
    std::fs::write(&identity, serde_json::to_vec(&changed).unwrap()).unwrap();
    click(visual, "capture-cancel");
    visual
        .condition(&view, |view, _| !view.busy && view.error.is_some())
        .await;
    view.read_with(visual, |view, _| {
        assert_eq!(view.draft.text, text);
        assert!(view.entry_retained.is_some());
        assert!(
            !view.entry_close_ready,
            "failed identity/checkpoint never authorizes close"
        );
    });
    assert!(visual.debug_bounds("capture-composer-dialog").is_some());
    std::fs::write(&identity, bytes).unwrap();
    click(visual, "capture-cancel");
    visual
        .condition(&view, |view, _| view.entry_retained.is_none())
        .await;
    unchanged(&f);
}

//! Non-shipping acceptance against the real platform, not TestAppContext.
//! The external driver observes clipboard and selected Finder/Explorer items.
use super::*;

pub(crate) fn entry() -> bool {
    let Some(directory) = std::env::var_os("OKILUM_EMBED_NATIVE_FIXTURE") else {
        return false;
    };
    assert!(!build_info::gpui_test_support(), "native platform required");
    let directory = PathBuf::from(directory);
    let root = directory.join("vault").canonicalize().unwrap();
    platform::text_platform::application()
        .with_assets(Assets)
        .run(move |cx| {
            reader_features::install_defaults(cx);
            gpui_component::init(cx);
            source_presentation::code_languages::install();
            reader_mermaid::register();
            bind_keys(cx);
            brand::load_fonts(cx).unwrap();
            brand::apply_theme(cx);
            let mut reader = None;
            let handle = cx
                .open_window(WindowOptions::default(), |window, cx| {
                    let entity = cx.new(|cx| {
                        Reader::new(
                            Opts {
                                vault: Some(root.clone()),
                                note: Some("note.md".into()),
                                session_directory: Some(directory.join("state")),
                                index_dir: Some(directory.join("index")),
                                ..Default::default()
                            },
                            window,
                            cx,
                        )
                    });
                    reader = Some(entity.clone());
                    cx.new(|cx| Root::new(entity, window, cx))
                })
                .unwrap();
            let reader = reader.unwrap();
            cx.activate(true);
            cx.spawn(async move |cx| {
                cx.background_executor().timer(Duration::from_secs(3)).await;
                for (phase, command) in [
                    ("relative", Command::CopyRelative),
                    ("absolute", Command::CopyPath),
                    ("reveal", Command::Reveal),
                ] {
                    handle
                        .update(cx, |_, window, cx| {
                            reader.update(cx, |reader, cx| {
                                let source = std::fs::read_to_string(root.join("note.md")).unwrap();
                                let reference = okilum_core::render::image_reference::references(
                                    &source,
                                    &reader.vault,
                                    &reader.current_rel,
                                )
                                .into_iter()
                                .find(|reference| {
                                    source.get(reference.range.clone())
                                        == Some("![[attachments/manual space.pdf]]")
                                })
                                .expect("authored attachment range survives the production parser");
                                let target = Target::reference(reader, &reference);
                                assert_eq!(
                                    target.relative.as_deref(),
                                    Some("attachments/manual space.pdf"),
                                    "production resolver decodes the captured path"
                                );
                                assert!(target.url.is_some(), "attachment is resolved");
                                reader.image_command(&ImageCommand { target, command }, window, cx);
                            });
                        })
                        .unwrap();
                    std::fs::write(directory.join(format!("ready-{phase}")), b"ready").unwrap();
                    let start = std::time::Instant::now();
                    while !directory.join(format!("ack-{phase}")).exists() {
                        assert!(
                            start.elapsed() < Duration::from_secs(30),
                            "native {phase} not observed"
                        );
                        cx.background_executor()
                            .timer(Duration::from_millis(100))
                            .await;
                    }
                }
                std::fs::write(directory.join("complete"), b"native actions observed").unwrap();
                cx.update(|cx| cx.quit());
            })
            .detach();
        });
    true
}

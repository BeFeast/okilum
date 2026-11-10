//! Real Reader-hosted render exercise on the OS main stack, without test-support.
//! The fixture runner owns the vault and every state directory.
use super::*;
use std::time::{Duration, Instant};

pub(crate) fn entry() -> bool {
    let Some(directory) = std::env::var_os("OKILUM_READER_NATIVE_FIXTURE") else {
        return false;
    };
    assert!(!build_info::gpui_test_support(), "real platform required");
    let directory = PathBuf::from(directory);
    platform::text_platform::application()
        .with_assets(Assets)
        .run(move |cx| {
            reader_features::install_defaults(cx);
            gpui_component::init(cx);
            source_presentation::code_languages::install();
            bind_keys(cx);
            brand::load_fonts(cx).unwrap();
            brand::apply_theme(cx);
            reader_ui_state::install(&directory.join("state"), cx);
            cx.activate(true);
            cx.spawn(async move |cx| {
                let mut windows = Vec::new();
                for name in [
                    "data.json",
                    "array.json",
                    "data.csv",
                    "gateway.log",
                    "document.pdf",
                    "image.png",
                    "archive.zip",
                    "scene.canvas",
                    "drawing.excalidraw",
                    "source.rs",
                    "note.md",
                ] {
                    let root = directory.join("vault");
                    let mut reader = None;
                    let handle = cx
                        .update(|cx| {
                            cx.open_window(
                                WindowOptions {
                                    window_bounds: Some(WindowBounds::Windowed(Bounds {
                                        origin: point(px(0.), px(0.)),
                                        size: size(px(1100.), px(800.)),
                                    })),
                                    ..Default::default()
                                },
                                |window, cx| {
                                    let view = cx.new(|cx| {
                                        Reader::new(
                                            Opts {
                                                vault: Some(root),
                                                note: Some(name.into()),
                                                index_dir: Some(directory.join("index")),
                                                session_directory: Some(directory.join("session")),
                                                ..Default::default()
                                            },
                                            window,
                                            cx,
                                        )
                                    });
                                    reader = Some(view.clone());
                                    cx.new(|cx| Root::new(view, window, cx))
                                },
                            )
                        })
                        .unwrap();
                    let reader = reader.unwrap();
                    let start = Instant::now();
                    let mut previous = start;
                    let mut ticks = 0;
                    loop {
                        cx.background_executor()
                            .timer(Duration::from_millis(100))
                            .await;
                        let now = Instant::now();
                        assert!(
                            now.duration_since(previous) < Duration::from_secs(5),
                            "Reader event loop blocked on {name}"
                        );
                        previous = now;
                        assert!(
                            start.elapsed() < Duration::from_secs(60),
                            "Reader did not open {name}"
                        );
                        ticks += 1;
                        let ready = cx.update(|cx| {
                            let r = reader.read(cx);
                            r.selected_file() == name
                                && (name.ends_with(".md")
                                    || r.file_preview.as_ref().is_some_and(|p| {
                                        p.unavailable.is_none()
                                            && p.code
                                                .as_ref()
                                                .is_none_or(|c| c.read(cx).native_source_ready())
                                    }))
                        });
                        if ready && ticks >= 10 {
                            break;
                        }
                    }
                    println!("READER_NATIVE file={name} mode=read mounted=Reader ticks={ticks}");
                    let editable = cx.update(|cx| {
                        let r = reader.read(cx);
                        name.ends_with(".md")
                            || (reader_delimited::editable(name) && r.code_file_editable(cx))
                    });
                    if editable {
                        handle
                            .update(cx, |_, window, cx| {
                                reader.update(cx, |r, cx| r.toggle_source(window, cx))
                            })
                            .unwrap();
                        cx.background_executor()
                            .timer(Duration::from_millis(800))
                            .await;
                        cx.update(|cx| {
                            assert!(
                                reader.read(cx).editing.is_some(),
                                "Reader did not enter edit mode: {name}"
                            )
                        });
                        println!("READER_NATIVE file={name} mode=edit mounted=Reader");
                    }
                    // Real renders of both neutral and warning component states,
                    // including all theme palettes, are covered by Reader tests.
                    windows.push(handle);
                }
                std::fs::write(directory.join("complete"), b"Reader native read/edit pass")
                    .unwrap();
                cx.update(|cx| cx.quit());
            })
            .detach();
        });
    true
}

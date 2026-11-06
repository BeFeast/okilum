//! Non-shipping native event-loop and clipboard acceptance, without test-support.
use super::*;

pub(crate) fn entry() -> bool {
    let Some(directory) = std::env::var_os("OKILUM_DATA_VIEW_NATIVE_FIXTURE") else {
        return false;
    };
    assert!(!build_info::gpui_test_support(), "real platform required");
    let directory = PathBuf::from(directory);
    platform::text_platform::application().with_assets(Assets).run(move |cx| {
        reader_features::install_defaults(cx);
        gpui_component::init(cx);
        source_presentation::code_languages::install();
        bind_keys(cx);
        brand::load_fonts(cx).unwrap();
        brand::apply_theme(cx);
        cx.activate(true);
        cx.spawn(async move |cx| {
            for name in ["data.xml", "big.json", "big.xml", "big.txt"] {
                let started = Instant::now();
                let mut preview = None;
                let handle = cx.update(|cx| cx.open_window(WindowOptions::default(), |window, cx| {
                    let view = cx.new(|cx| CodePreview::new(directory.clone(), name.into(), directory.join(name), window, cx));
                    preview = Some(view.clone());
                    cx.new(|cx| Root::new(view, window, cx))
                })).unwrap();
                let preview = preview.unwrap();
                let mut previous = started;
                let mut maximum = Duration::ZERO;
                let mut ticks = 0;
                loop {
                    cx.background_executor().timer(Duration::from_millis(100)).await;
                    let now = Instant::now();
                    maximum = maximum.max(now.duration_since(previous));
                    previous = now;
                    ticks += 1;
                    assert!(maximum < Duration::from_secs(2), "{name} native event loop blocked {maximum:?}");
                    assert!(started.elapsed() < Duration::from_secs(90), "{name} never became ready");
                    let ready = cx.update(|cx| preview.read(cx).document.is_some()
                        && preview.read(cx).input().is_some_and(|input| !input.read(cx).highlighting_pending()));
                    if ready && ticks >= 6 { break; }
                }
                handle.update(cx, |_, _, cx| {
                    preview.update(cx, |p, cx| {
                        if name == "big.json" || name == "big.xml" {
                            p.fold_all(true, cx);
                            assert_eq!(p.structure.visible, vec![0]);
                            p.fold_all(false, cx);
                        }
                        let doc = p.document.as_ref().unwrap();
                        let input = p.input().unwrap();
                        assert!(!input.read(cx).is_editable());
                        if name == "big.json" {
                            assert!(p.structure_view, "minified native view uses the complete AST");
                            assert_eq!(p.structure.visible.len(), 3);
                            assert!(doc.text.len() > 32 * 1024 * 1024);
                            assert!(input.read(cx).text().len() <= 32 * 1024);
                            let matches = doc.find("NEEDLE");
                            assert_eq!(matches.len(), 1);
                            let range = doc.node_range(matches[0].start).unwrap();
                            cx.write_to_clipboard(ClipboardItem::new_string(doc.text[range].to_owned()));
                        } else if name == "big.xml" {
                            assert!(p.structure_view);
                            assert!(doc.structure.len() > 100_000);
                            assert!(input.read(cx).text().len() <= 32 * 1024);
                            let matches = doc.find("NEEDLE");
                            assert_eq!(matches.len(), 1);
                            let range = doc.node_range(matches[0].start).unwrap();
                            cx.write_to_clipboard(ClipboardItem::new_string(doc.text[range].to_owned()));
                        } else if name == "data.xml" {
                            input.update(cx, |s, cx| {
                                s.set_all_folded(true, cx);
                                assert!(!s.folded_ranges().is_empty());
                                s.set_all_folded(false, cx);
                                s.set_selected_range(doc.text.find("שלום").unwrap()..doc.text.find("שלום").unwrap(), cx);
                            });
                            p.copy_node(cx);
                        } else {
                            assert!(doc.text.len() >= 32 * 1024 * 1024 - 256);
                            assert_eq!(input.read(cx).text().len(), doc.text.len());
                            cx.write_to_clipboard(ClipboardItem::new_string(doc.text[..8].to_owned()));
                        }
                    });
                }).unwrap();
                let receipt = serde_json::json!({"file":name,"ticks":ticks,"max_event_loop_gap_ms":maximum.as_millis(),"ready_ms":started.elapsed().as_millis(),"real_native_platform":true});
                std::fs::write(directory.join(format!("ready-{name}")), receipt.to_string()).unwrap();
                let start = Instant::now();
                while !directory.join(format!("ack-{name}")).exists() {
                    assert!(start.elapsed() < Duration::from_secs(30), "system clipboard not observed");
                    cx.background_executor().timer(Duration::from_millis(100)).await;
                }
                handle.update(cx, |_, window, _| window.remove_window()).unwrap();
            }
            std::fs::write(directory.join("complete"), b"native viewer observed").unwrap();
            cx.update(|cx| cx.quit());
        }).detach();
    });
    true
}

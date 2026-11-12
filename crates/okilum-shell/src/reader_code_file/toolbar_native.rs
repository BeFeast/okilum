//! Opt-in real Reader timing gate for toolbar step 5. No test-support platform.
use super::*;
use std::time::Duration;

pub(super) fn entry() -> bool {
    let Some(directory) = std::env::var_os("OKILUM_CODE_TOOLBAR_NATIVE_FIXTURE") else {
        return false;
    };
    assert!(!build_info::gpui_test_support());
    let directory = PathBuf::from(directory);
    platform::text_platform::application().with_assets(Assets).run(move |cx| {
        reader_features::install_defaults(cx); gpui_component::init(cx);
        crate::bind_keys(cx); source_presentation::code_languages::install();
        brand::load_fonts(cx).unwrap(); brand::apply_theme(cx);
        reader_ui_state::install(&directory.join("state"), cx);
        cx.activate(true);
        let mut reader = None;
        let handle = cx.open_window(WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds { origin: point(px(0.),px(0.)), size:size(px(1100.),px(800.)) })),
            ..Default::default()
        }, |w,cx| {
            let entity = cx.new(|cx|Reader::new(Opts {vault:Some(directory.join("vault")),note:Some("api.json".into()),index_dir:Some(directory.join("index")),session_directory:Some(directory.join("session")),..Default::default()},w,cx));
            reader=Some(entity.clone()); cx.new(|cx|Root::new(entity,w,cx))
        }).unwrap();
        let reader=reader.unwrap(); let window:AnyWindowHandle=handle.into();
        cx.spawn(async move |cx| {
            let start=Instant::now();
            let code=loop {
                cx.background_executor().timer(Duration::from_millis(50)).await;
                assert!(start.elapsed()<Duration::from_secs(60),"Reader Tree never ready");
                let code=cx.update(|cx| reader.read(cx).file_preview.as_ref().and_then(|p|p.code.clone()));
                if let Some(code)=code {
                    if cx.update(|cx|code.read(cx).document.is_some() && code.read(cx).view==View::Tree) {break code;}
                }
            };
            // Drain one real native draw after the worker installed its snapshot.
            window.update(cx,|_,w,cx|{w.refresh();w.draw(cx).clear(cx);}).unwrap();
            let (paths,bytes)=cx.update(|cx|{
                let doc=code.read(cx).document.as_ref().unwrap();
                let paths=(0..doc.structure.len()).find(|i|doc.key_text(*i)==Some("paths")).unwrap();
                assert_eq!(doc.container(paths),Some((document::ContainerKind::Object,414)));
                (paths,doc.text.len())
            });
            let open_ms=window.update(cx,|_,w,cx|{
                let start=Instant::now();
                code.update(cx,|p,cx|{p.structure.toggle(paths,p.document.as_ref().unwrap());cx.notify();});
                w.draw(cx).clear(cx);
                start.elapsed().as_secs_f64()*1000.
            }).unwrap();
            assert!(open_ms<=100.,"paths opening exceeded 100 ms: {open_ms}");
            let mut frames=Vec::new();
            for marked in [false,true] {
                window.update(cx,|_,w,cx|{
                    let target=code.read(cx).find_target(code.clone(),cx);
                    target.set_query(if marked {"description"} else {""},w,cx);
                }).unwrap();
                loop {
                    cx.background_executor().timer(Duration::from_millis(20)).await;
                    if !cx.update(|cx|code.read(cx).searching) {break;}
                    assert!(start.elapsed()<Duration::from_secs(90));
                }
                cx.update(|cx|{
                    let p=code.read(cx);
                    if marked {assert_eq!(p.scrollbar_marks.len(),1000,"positive control: capped marks actually present");}
                    else {assert!(p.scrollbar_marks.is_empty());}
                });
                // Find may reveal another branch; restore the paths branch for
                // both passes. Every scroll sample shapes newly exposed rows.
                code.update(cx,|p,cx|{
                    if p.structure.folded(paths) {p.structure.toggle(paths,p.document.as_ref().unwrap());}
                    cx.notify();
                });
                for sample in 0..48 {
                    cx.background_executor().timer(Duration::from_millis(20)).await;
                    let ms=window.update(cx,|_,w,cx|{
                        let rows_before=structure::native_row_builds();
                        let offset_before=code.read(cx).structure.scroll.0.borrow().base_handle.offset().y;
                        let frame=Instant::now();
                        code.update(cx,|p,cx|{
                            let at=p.structure.visible.binary_search(&paths).unwrap();
                            p.structure.scroll.scroll_to_item_strict(at+1+(sample*7)%390,ScrollStrategy::Top);
                            cx.notify();
                        });
                        w.draw(cx).clear(cx);
                        let ms=frame.elapsed().as_secs_f64()*1000.;
                        let rows=structure::native_row_builds()-rows_before;
                        let offset=code.read(cx).structure.scroll.0.borrow().base_handle.offset().y;
                        assert!(rows>0,"scroll sample must build real visible Tree rows");
                        assert_ne!(offset,offset_before,"scroll sample must move the native viewport");
                        (ms,rows,offset.as_f32())
                    }).unwrap();
                    frames.push(serde_json::json!({"marks":marked,"sample":sample,"cpu_draw_ms":ms.0,"rows_built":ms.1,"scroll_y":ms.2}));
                    let ms=ms.0;
                    assert!(ms<=16.,"Tree scroll frame exceeded 16 ms (marks={marked}, sample={sample}): {ms}");
                }
            }
            let receipt=serde_json::json!({"real_native_platform":true,"mounted":"Reader","bytes":bytes,"paths_keys":414,"open_ms":open_ms,"frames":frames,"source_sha":env!("OKILUM_SOURCE_COMMIT")});
            std::fs::write(directory.join("toolbar-receipt.json"),receipt.to_string()).unwrap();
            diagnostic_println!("CODE2810_NATIVE_READER_PASS {receipt}");
            cx.update(|cx|cx.quit());
        }).detach();
    });
    true
}

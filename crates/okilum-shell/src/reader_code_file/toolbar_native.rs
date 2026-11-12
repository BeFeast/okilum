//! Opt-in real Reader timing gate for toolbar step 5. No test-support platform.
use super::*;
use std::time::Duration;
pub(crate) mod trace;

fn submit_frame(window: &mut Window) -> f64 {
    let mut frames = gpui::profiler::FrameTimingCollector::new();
    let started = Instant::now();
    window.present_if_needed();
    let present_ms = started.elapsed().as_secs_f64() * 1000.;
    assert!(
        frames.collect_unseen().iter().any(|event| matches!(
            event,
            gpui::profiler::FrameEvent::Present(frame)
                if frame.window_id == window.window_handle().window_id()
                    && frame.present_start >= started
        )),
        "native budget must submit each newly drawn frame"
    );
    present_ms
}

// Diagnostic only: retain the hard wall-clock frame gate, but distinguish
// actual UI work from scheduler/OS stalls in native Windows failures.
#[cfg(target_os = "windows")]
fn thread_cpu_ms() -> Option<f64> {
    use windows_sys::Win32::{
        Foundation::FILETIME,
        System::Threading::{GetCurrentThread, GetThreadTimes},
    };
    let empty = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let (mut created, mut exited, mut kernel, mut user) = (empty, empty, empty, empty);
    // SAFETY: the current-thread pseudo handle is valid and all outputs point
    // to live FILETIME storage for the duration of this synchronous call.
    let ok = unsafe {
        GetThreadTimes(
            GetCurrentThread(),
            &mut created,
            &mut exited,
            &mut kernel,
            &mut user,
        )
    };
    assert_ne!(ok, 0, "GetThreadTimes failed on the native UI thread");
    let ticks = |t: FILETIME| (u64::from(t.dwHighDateTime) << 32) | u64::from(t.dwLowDateTime);
    Some((ticks(kernel) + ticks(user)) as f64 / 10_000.)
}
#[cfg(not(target_os = "windows"))]
fn thread_cpu_ms() -> Option<f64> {
    None
}

#[cfg(target_os = "windows")]
fn memory_and_cycles() -> (u32, usize, u64, u64) {
    #[repr(C)]
    struct Counters {
        cb: u32,
        faults: u32,
        sizes: [usize; 8],
    }
    #[link(name = "psapi")]
    unsafe extern "system" {
        fn GetProcessMemoryInfo(
            process: *mut std::ffi::c_void,
            counters: *mut Counters,
            size: u32,
        ) -> i32;
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn QueryThreadCycleTime(thread: *mut std::ffi::c_void, cycles: *mut u64) -> i32;
        fn GetSystemTimePreciseAsFileTime(time: *mut windows_sys::Win32::Foundation::FILETIME);
    }
    let mut counters = Counters {
        cb: std::mem::size_of::<Counters>() as u32,
        faults: 0,
        sizes: [0; 8],
    };
    let mut cycles = 0;
    unsafe {
        use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetCurrentThread};
        assert_ne!(
            GetProcessMemoryInfo(
                GetCurrentProcess(),
                &mut counters,
                std::mem::size_of::<Counters>() as u32
            ),
            0
        );
        assert_ne!(QueryThreadCycleTime(GetCurrentThread(), &mut cycles), 0);
    }
    let mut wall = windows_sys::Win32::Foundation::FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    unsafe {
        GetSystemTimePreciseAsFileTime(&mut wall);
    }
    (
        counters.faults,
        counters.sizes[1],
        cycles,
        (u64::from(wall.dwHighDateTime) << 32) | u64::from(wall.dwLowDateTime),
    )
}
#[cfg(not(target_os = "windows"))]
fn memory_and_cycles() -> (u32, usize, u64, u64) {
    (0, 0, 0, 0)
}

pub(super) fn entry() -> bool {
    let Some(directory) = std::env::var_os("OKILUM_CODE_TOOLBAR_NATIVE_FIXTURE") else {
        return false;
    };
    assert!(!build_info::gpui_test_support());
    let smoke = std::env::var_os("OKILUM_CODE_TOOLBAR_NATIVE_SMOKE").is_some();
    assert_eq!(
        smoke,
        cfg!(debug_assertions),
        "native stack smoke requires debug; CPU budgets require production release"
    );
    let directory = PathBuf::from(directory);
    platform::text_platform::application().with_assets(Assets).run(move |cx| {
        reader_features::install_defaults(cx); gpui_component::init(cx);
        crate::bind_keys(cx); source_presentation::code_languages::install();
        brand::load_fonts(cx).unwrap(); brand::apply_theme(cx);
        reader_ui_state::install(&directory.join("state"), cx);
        gpui::profiler::set_trace_enabled(true);
        cx.activate(true);
        let startup_text_probe=platform::text_diagnostics::Probe::start();
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
            window.update(cx,|_,w,cx|{w.refresh();w.draw(cx).clear(cx);submit_frame(w);}).unwrap();
            let initial_rasterizations=startup_text_probe.report()["work"]["glyph_bitmap"]["calls"].as_u64().unwrap();
            assert!(initial_rasterizations>0,"native glyph observer must see initial painting");
            drop(startup_text_probe);
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
                let ms=start.elapsed().as_secs_f64()*1000.;
                submit_frame(w);
                ms
            }).unwrap();
            diagnostic_println!("CODE2810_OPEN wall_ms={open_ms}");
            assert!(smoke || open_ms<=100.,"paths opening exceeded 100 ms: {open_ms}");
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
                let mut scroll_rasterizations=0;
                for sample in 0..48 {
                    cx.background_executor().timer(Duration::from_millis(20)).await;
                    let ms=window.update(cx,|_,w,cx|{
                        let rows_before=structure::native_row_builds();
                        let offset_before=code.read(cx).structure.scroll.0.borrow().base_handle.offset().y;
                        let cpu_before=thread_cpu_ms();
                        let memory_before=memory_and_cycles();
                        let text_probe=platform::text_diagnostics::Probe::start();
                        let frame=Instant::now();
                        code.update(cx,|p,cx|{
                            let at=p.structure.visible.binary_search(&paths).unwrap();
                            p.structure.scroll.scroll_to_item_strict(at+1+(sample*7)%390,ScrollStrategy::Top);
                            cx.notify();
                        });
                        let update_ms=frame.elapsed().as_secs_f64()*1000.;
                        trace::reset();
                        let draw_start=Instant::now();
                        let arena=w.draw(cx);
                        let draw_ms=draw_start.elapsed().as_secs_f64()*1000.;
                        let clear_start=Instant::now();
                        arena.clear(cx);
                        let clear_ms=clear_start.elapsed().as_secs_f64()*1000.;
                        let ms=frame.elapsed().as_secs_f64()*1000.;
                        // Window::draw only builds the scene. Without submission,
                        // native GPU atlas commands can accumulate across samples;
                        // the production event loop always presents after drawing.
                        // Keep the full update/draw/clear CPU budget unchanged.
                        let rows=structure::native_row_builds()-rows_before;
                        let offset=code.read(cx).structure.scroll.0.borrow().base_handle.offset().y;
                        assert!(rows>0,"scroll sample must build real visible Tree rows");
                        assert_ne!(offset,offset_before,"scroll sample must move the native viewport");
                        let cpu_ms=thread_cpu_ms().zip(cpu_before).map(|(end,start)|end-start);
                        let memory_after=memory_and_cycles();
                        let text_work=text_probe.report();
                        let mut subtree_work=trace::report();
                        subtree_work["native_memory_and_cycles"]=serde_json::json!({"wall_bounds_filetime_100ns":[memory_before.3,memory_after.3],"process_page_faults":memory_after.0.saturating_sub(memory_before.0),"working_set":memory_after.1,"thread_cycles":memory_after.2.saturating_sub(memory_before.2)});
                        let present_ms=submit_frame(w);
                        (ms,rows,offset.as_f32(),update_ms,cpu_ms,draw_ms,clear_ms,text_work,subtree_work,present_ms)
                    }).unwrap();
                    scroll_rasterizations+=ms.7["work"]["glyph_bitmap"]["calls"].as_u64().unwrap();
                    frames.push(serde_json::json!({"marks":marked,"sample":sample,"cpu_draw_ms":ms.0,"rows_built":ms.1,"scroll_y":ms.2,"update_ms":ms.3,"thread_cpu_ms":ms.4,"draw_ms":ms.5,"clear_ms":ms.6,"text_work":ms.7,"subtree_work":ms.8,"present_ms":ms.9}));
                    diagnostic_println!("CODE2810_FRAME marks={marked} sample={sample} wall_ms={} update_ms={} thread_cpu_ms={:?} draw_ms={} clear_ms={} text_work={} subtree_work={} present_ms={}",ms.0,ms.3,ms.4,ms.5,ms.6,ms.7,ms.8,ms.9);
                    let ms=ms.0;
                    assert!(smoke || ms<=16.,"Tree scroll frame exceeded 16 ms (marks={marked}, sample={sample}): {ms}");
                }
                assert!(scroll_rasterizations<=1,"Tree scroll rasterized new glyph variants after warm-up (marks={marked}): {scroll_rasterizations}");
            }
            let receipt=serde_json::json!({"real_native_platform":true,"mounted":"Reader","bytes":bytes,"paths_keys":414,"initial_rasterizations":initial_rasterizations,"open_ms":open_ms,"frames":frames,"source_sha":env!("OKILUM_SOURCE_COMMIT"),"measurement":if smoke {"debug-smoke"} else {"production-release"}});
            std::fs::write(directory.join("toolbar-receipt.json"),receipt.to_string()).unwrap();
            diagnostic_println!("CODE2810_NATIVE_READER_PASS {receipt}");
            cx.update(|cx|cx.quit());
        }).detach();
    });
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[gpui::test]
    fn code2810_native_budget_submits_each_draw_before_next_sample(cx: &mut TestAppContext) {
        let (_, visual, _temp) =
            super::super::view_tests::fixture(cx, "data.json", "{\"answer\":42}");
        gpui::profiler::set_trace_enabled(true);
        visual.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
            submit_frame(window);
        });
        gpui::profiler::set_trace_enabled(false);
    }
}

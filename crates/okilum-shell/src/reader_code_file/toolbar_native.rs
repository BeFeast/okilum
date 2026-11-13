//! Opt-in real Reader timing gate for toolbar step 5. No test-support platform.
use super::*;
use std::time::Duration;
pub(super) mod trace;
#[cfg(target_os = "windows")]
mod qpc_clock;

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
#[cfg(target_os = "macos")]
fn thread_cpu_ms() -> Option<f64> {
    let mut value = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: main-thread identity is queried without mutation, and the
    // CLOCK_THREAD_CPUTIME_ID output is valid aligned timespec storage.
    unsafe {
        assert_eq!(libc::pthread_main_np(), 1, "CPU clock must measure native UI main thread");
        assert_eq!(libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut value), 0);
    }
    Some(value.tv_sec as f64 * 1000. + value.tv_nsec as f64 / 1_000_000.)
}
#[cfg(target_os = "macos")]
fn mac_clock_metadata() -> serde_json::Value {
    let mut resolution = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    let mut tid = 0;
    // SAFETY: these read-only queries address the current pthread; all output
    // pointers remain live for the synchronous calls. No stack setting changes.
    let stack_size = unsafe {
        assert_eq!(libc::pthread_main_np(), 1);
        assert_eq!(libc::pthread_threadid_np(libc::pthread_self(), &mut tid), 0);
        assert_eq!(libc::clock_getres(libc::CLOCK_THREAD_CPUTIME_ID, &mut resolution), 0);
        libc::pthread_get_stacksize_np(libc::pthread_self())
    };
    let resolution_ns = resolution.tv_sec as u64 * 1_000_000_000 + resolution.tv_nsec as u64;
    assert!(tid > 0 && stack_size > 0 && resolution_ns > 0);
    serde_json::json!({"api":"CLOCK_THREAD_CPUTIME_ID","clock_id":libc::CLOCK_THREAD_CPUTIME_ID,
        "resolution_ns":resolution_ns,"pid":std::process::id(),"native_tid":tid,
        "main_thread":true,"pthread_stack_bytes":stack_size,
        "limitation":"thread CPU cannot distinguish Ready from Waiting"})
}
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn thread_cpu_ms() -> Option<f64> {
    None
}

pub(super) fn entry() -> bool {
    let Some(directory) = std::env::var_os("OKILUM_CODE_TOOLBAR_NATIVE_FIXTURE") else {
        return false;
    };
    assert!(!build_info::gpui_test_support());
    let smoke = std::env::var_os("OKILUM_CODE_TOOLBAR_NATIVE_SMOKE").is_some();
    let collect_all = std::env::var_os("OKILUM_DIAG_COLLECT_ALL").is_some();
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
            #[cfg(target_os="windows")]
            let frame_clock=qpc_clock::Clock::new();
            #[cfg(target_os="macos")]
            let mac_clock=mac_clock_metadata();
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
            let mut budget_violations=Vec::new();
            if open_ms>100. {budget_violations.push(serde_json::json!({"kind":"open","wall_ms":open_ms,"limit_ms":100}));}
            assert!(smoke || collect_all || open_ms<=100.,"paths opening exceeded 100 ms: {open_ms}");
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
                        #[cfg(target_os="windows")]
                        let clock_before=frame_clock.anchor((marked as u64)*96+(sample as u64)*2);
                        let rows_before=structure::native_row_builds();
                        let offset_before=code.read(cx).structure.scroll.0.borrow().base_handle.offset().y;
                        #[cfg(target_os="macos")]
                        let cpu_wall_before=Instant::now();
                        let cpu_before=thread_cpu_ms();
                        let text_probe=platform::text_diagnostics::Probe::start();
                        #[cfg(target_os="windows")]
                        let start_before=qpc_clock::qpc();
                        let frame=Instant::now();
                        #[cfg(target_os="windows")]
                        let start_after=qpc_clock::qpc();
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
                        #[cfg(target_os="windows")]
                        let end_before=qpc_clock::qpc();
                        let ms=frame.elapsed().as_secs_f64()*1000.;
                        #[cfg(target_os="windows")]
                        let end_after=qpc_clock::qpc();
                        // Window::draw only builds the scene. Without submission,
                        // native GPU atlas commands can accumulate across samples;
                        // the production event loop always presents after drawing.
                        // Keep the full update/draw/clear CPU budget unchanged.
                        let rows=structure::native_row_builds()-rows_before;
                        let offset=code.read(cx).structure.scroll.0.borrow().base_handle.offset().y;
                        assert!(rows>0,"scroll sample must build real visible Tree rows");
                        assert_ne!(offset,offset_before,"scroll sample must move the native viewport");
                        let cpu_after=thread_cpu_ms();
                        #[cfg(target_os="macos")]
                        let cpu_wall_ms=cpu_wall_before.elapsed().as_secs_f64()*1000.;
                        let cpu_ms=cpu_after.zip(cpu_before).map(|(end,start)|end-start);
                        let text_work=text_probe.report();
                        let mut subtree_work=trace::report();
                        #[cfg(target_os="macos")]
                        {
                            subtree_work["diagnostic_macos_cpu"]=serde_json::json!({
                                "clock":mac_clock,"before_cpu_ms":cpu_before,"after_cpu_ms":cpu_after,
                                "cpu_bracket_wall_ms":cpu_wall_ms,"cpu_bracket_margin_ms":cpu_wall_ms-ms
                            });
                        }
                        #[cfg(target_os="windows")]
                        {
                            let clock_after=frame_clock.anchor((marked as u64)*96+(sample as u64)*2+1);
                            subtree_work["diagnostic_qpc"]=serde_json::json!({
                                "version":1,"start_qpc_bounds":[start_before,start_after],
                                "end_qpc_bounds":[end_before,end_after],
                                "before":clock_before,"after":clock_after
                            });
                        }
                        let present_ms=submit_frame(w);
                        (ms,rows,offset.as_f32(),update_ms,cpu_ms,draw_ms,clear_ms,text_work,subtree_work,present_ms)
                    }).unwrap();
                    scroll_rasterizations+=ms.7["work"]["glyph_bitmap"]["calls"].as_u64().unwrap();
                    frames.push(serde_json::json!({"marks":marked,"sample":sample,"cpu_draw_ms":ms.0,"rows_built":ms.1,"scroll_y":ms.2,"update_ms":ms.3,"thread_cpu_ms":ms.4,"draw_ms":ms.5,"clear_ms":ms.6,"text_work":ms.7,"subtree_work":ms.8,"present_ms":ms.9}));
                    diagnostic_println!("CODE2810_FRAME marks={marked} sample={sample} wall_ms={} update_ms={} thread_cpu_ms={:?} draw_ms={} clear_ms={} text_work={} subtree_work={} present_ms={}",ms.0,ms.3,ms.4,ms.5,ms.6,ms.7,ms.8,ms.9);
                    let ms=ms.0;
                    if ms>16. {budget_violations.push(serde_json::json!({"kind":"frame","marks":marked,"sample":sample,"wall_ms":ms,"limit_ms":16}));}
                    assert!(smoke || collect_all || ms<=16.,"Tree scroll frame exceeded 16 ms (marks={marked}, sample={sample}): {ms}");
                }
                assert!(scroll_rasterizations<=1,"Tree scroll rasterized new glyph variants after warm-up (marks={marked}): {scroll_rasterizations}");
            }
            let receipt=serde_json::json!({"real_native_platform":true,"mounted":"Reader","bytes":bytes,"paths_keys":414,"initial_rasterizations":initial_rasterizations,"open_ms":open_ms,"frames":frames,"source_sha":env!("OKILUM_SOURCE_COMMIT"),"measurement":if smoke {"debug-smoke"} else {"production-release"}});
            if collect_all {
                let strict_pass=budget_violations.is_empty();
                let mut diagnostic=receipt;
                diagnostic["diagnostic_only"]=serde_json::json!(true);
                diagnostic["strict_pass"]=serde_json::json!(strict_pass);
                diagnostic["budget_violations"]=serde_json::json!(budget_violations);
                std::fs::write(directory.join("toolbar-diagnostic-receipt.json"),diagnostic.to_string()).unwrap();
                diagnostic_println!("CODE2811_QPC_CENSUS_COMPLETE frames={} strict_pass={strict_pass}",frames.len());
                assert!(strict_pass,"original native budgets failed after complete diagnostic census");
                cx.update(|cx|cx.quit());
                return;
            }
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

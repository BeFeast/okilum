//! Opt-in real Reader timing gate for toolbar step 5. No test-support platform.
use super::*;
pub(super) use okilum_shell_viewers::reader_code_file::toolbar_native::trace;
use std::time::Duration;

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

/// Budget for UI-thread Running time in every scroll frame.
const RUNNING_BUDGET_MS: f64 = 16.;
/// Wall-clock budget most frames must still meet.
const WALL_BUDGET_MS: f64 = 16.;
/// Frames per 48-frame pass that may exceed the wall budget while their
/// Running time stays inside it, and the wall ceiling for those frames.
/// Hosted Windows runners render through WARP on the same four CPUs, so the UI
/// thread waits Ready for 30-40 ms in about 3% of frames while it runs under
/// 12 ms (#2845 ETW census; slowest census frame 41.7 ms, green runs since
/// then at most one miss per job, 38.8 ms). No such source is established on
/// macOS, where single frames missed at 20.1 and 27.4 ms with Running under
/// 7 ms, so it allows one frame per pass. A regression that waits longer, or in
/// more frames, still fails.
///
/// Accepted residual risk: without kernel tracing the gate cannot tell why a
/// frame waited. A new UI-thread wait (lock, synchronous I/O) that stays within
/// these counts and ceilings passes, as scheduler waits do. The old rule
/// (every frame <=16 ms wall) caught it but failed on runner scheduling alone.
#[cfg(target_os = "windows")]
const WALL_MISSES_ALLOWED: usize = 2;
#[cfg(target_os = "windows")]
const WALL_CEILING_MS: f64 = 50.;
#[cfg(not(target_os = "windows"))]
const WALL_MISSES_ALLOWED: usize = 1;
#[cfg(not(target_os = "windows"))]
const WALL_CEILING_MS: f64 = 50.;

/// Thread CPU clock with sub-millisecond resolution. GetThreadTimes ticks at
/// 15.6 ms on Windows, too coarse for a 16 ms budget, so Windows counts thread
/// cycles and converts them with a rate calibrated on this thread. Windows
/// counts those cycles with the time-stamp counter, so the rate is constant
/// only on a CPU with an invariant TSC; `calibrate` requires one.
#[cfg(target_os = "windows")]
struct RunningClock {
    cycles_per_ms: f64,
}

#[cfg(target_os = "windows")]
impl RunningClock {
    fn cycles() -> u64 {
        use windows_sys::Win32::System::{
            Threading::GetCurrentThread, WindowsProgramming::QueryThreadCycleTime,
        };
        let mut cycles = 0;
        // SAFETY: the current-thread pseudo handle is valid and the output
        // points to live storage for this synchronous call.
        let ok = unsafe { QueryThreadCycleTime(GetCurrentThread(), &mut cycles) };
        assert_ne!(ok, 0, "QueryThreadCycleTime failed on the native UI thread");
        cycles
    }

    /// An invariant TSC (CPUID 0x8000_0007, EDX bit 8) ticks at one rate in
    /// every P-, C- and T-state, so one calibrated rate converts every frame.
    /// Without it the counter may slow inside a frame and read short.
    #[cfg(target_arch = "x86_64")]
    fn require_invariant_tsc() {
        use std::arch::x86_64::__cpuid;
        // Leaf 0x8000_0007 is read only after the maximum extended leaf
        // confirms it exists.
        let max = __cpuid(0x8000_0000).eax;
        let power = (max >= 0x8000_0007).then(|| __cpuid(0x8000_0007).edx);
        let invariant = power.is_some_and(|edx| edx & (1 << 8) != 0);
        diagnostic_println!("CODE2810_INVARIANT_TSC max_leaf={max:#x} invariant={invariant}");
        assert!(
            invariant,
            "thread cycles can only be converted to time with an invariant TSC"
        );
    }

    #[cfg(not(target_arch = "x86_64"))]
    fn require_invariant_tsc() {
        panic!("the Windows Running meter needs an x86_64 invariant TSC");
    }

    /// Cycles only accrue while the thread runs, so preemption during a spin
    /// lowers the measured rate, and a low rate overstates Running time. The
    /// highest of several spins is kept.
    fn calibrate() -> Self {
        Self::require_invariant_tsc();
        let mut best = 0f64;
        for _ in 0..5 {
            let (wall, cycles) = (Instant::now(), Self::cycles());
            while wall.elapsed() < Duration::from_millis(20) {
                std::hint::spin_loop();
            }
            let ms = wall.elapsed().as_secs_f64() * 1000.;
            best = best.max((Self::cycles() - cycles) as f64 / ms);
        }
        assert!(best > 0., "thread cycle counter did not advance");
        Self {
            cycles_per_ms: best,
        }
    }

    fn now_ms(&self) -> f64 {
        Self::cycles() as f64 / self.cycles_per_ms
    }
}

#[cfg(unix)]
struct RunningClock;

#[cfg(unix)]
impl RunningClock {
    fn calibrate() -> Self {
        Self
    }

    fn now_ms(&self) -> f64 {
        let mut t = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: `t` is live storage for this synchronous call.
        let ok = unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut t) };
        assert_eq!(
            ok, 0,
            "thread CPU clock unavailable on the native UI thread"
        );
        t.tv_sec as f64 * 1000. + t.tv_nsec as f64 / 1_000_000.
    }
}

impl RunningClock {
    /// Positive control: a 20 ms busy spin must read as Running time, so a
    /// meter that reports zero cannot pass the per-frame Running gate. The
    /// best of five spins counts: a hosted runner may preempt any one of them.
    fn verified() -> Self {
        let clock = Self::calibrate();
        let mut best = (0f64, 0f64);
        for _ in 0..5 {
            let (wall, running) = (Instant::now(), clock.now_ms());
            while wall.elapsed() < Duration::from_millis(20) {
                std::hint::spin_loop();
            }
            let spin = (
                wall.elapsed().as_secs_f64() * 1000.,
                clock.now_ms() - running,
            );
            diagnostic_println!(
                "CODE2810_RUNNING_CONTROL wall_ms={} running_ms={}",
                spin.0,
                spin.1
            );
            if spin.1 > best.1 {
                best = spin;
            }
        }
        let (wall_ms, running_ms) = best;
        assert!(
            running_ms >= 18.,
            "thread Running meter missed a 20 ms busy spin: {running_ms} ms of {wall_ms} ms"
        );
        clock
    }
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
                let code=cx.update(|cx| reader.read(cx).pane.file_preview.as_ref().and_then(|p|p.code.clone()));
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
            let running=RunningClock::verified();
            let mut frames=Vec::new();
            for marked in [false,true] {
                let mut wall_misses=0;
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
                        let cpu_before=running.now_ms();
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
                        let cpu_ms=running.now_ms()-cpu_before;
                        let text_work=text_probe.report();
                        let subtree_work=trace::report();
                        let present_ms=submit_frame(w);
                        (ms,rows,offset.as_f32(),update_ms,cpu_ms,draw_ms,clear_ms,text_work,subtree_work,present_ms)
                    }).unwrap();
                    scroll_rasterizations+=ms.7["work"]["glyph_bitmap"]["calls"].as_u64().unwrap();
                    frames.push(serde_json::json!({"marks":marked,"sample":sample,"cpu_draw_ms":ms.0,"rows_built":ms.1,"scroll_y":ms.2,"update_ms":ms.3,"running_ms":ms.4,"draw_ms":ms.5,"clear_ms":ms.6,"text_work":ms.7,"subtree_work":ms.8,"present_ms":ms.9}));
                    diagnostic_println!("CODE2810_FRAME marks={marked} sample={sample} wall_ms={} update_ms={} running_ms={} draw_ms={} clear_ms={} text_work={} subtree_work={} present_ms={}",ms.0,ms.3,ms.4,ms.5,ms.6,ms.7,ms.8,ms.9);
                    let (wall,running_ms)=(ms.0,ms.4);
                    // A meter that runs ahead of the wall clock is miscalibrated.
                    assert!(running_ms<=wall*1.05+1.,"Running {running_ms} ms exceeds wall {wall} ms (marks={marked}, sample={sample})");
                    if smoke {continue;}
                    assert!(running_ms<=RUNNING_BUDGET_MS,"Tree scroll frame ran {running_ms} ms on the UI thread (marks={marked}, sample={sample}, wall {wall} ms)");
                    assert!(wall<=WALL_CEILING_MS,"Tree scroll frame took {wall} ms wall (marks={marked}, sample={sample}, running {running_ms} ms)");
                    if wall>WALL_BUDGET_MS {wall_misses+=1;}
                }
                assert!(wall_misses<=WALL_MISSES_ALLOWED,"{wall_misses} of 48 Tree scroll frames exceeded {WALL_BUDGET_MS} ms wall (marks={marked}); allowed {WALL_MISSES_ALLOWED}");
                assert!(scroll_rasterizations<=1,"Tree scroll rasterized new glyph variants after warm-up (marks={marked}): {scroll_rasterizations}");
            }
            let receipt=serde_json::json!({"real_native_platform":true,"mounted":"Reader","bytes":bytes,"paths_keys":414,"initial_rasterizations":initial_rasterizations,"open_ms":open_ms,"frames":frames,"source_sha":env!("OKILUM_SOURCE_COMMIT"),"running_budget_ms":RUNNING_BUDGET_MS,"wall_budget_ms":WALL_BUDGET_MS,"wall_misses_allowed":WALL_MISSES_ALLOWED,"wall_ceiling_ms":WALL_CEILING_MS,"measurement":if smoke {"debug-smoke"} else {"production-release"}});
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

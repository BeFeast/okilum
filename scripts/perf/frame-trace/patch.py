#!/usr/bin/env python3
"""Measurement-only patch of throwaway COPIES of the gpui-pre crates (#1004, #1290).

Writes one line per presented frame with the time of each phase when OKILUM_FRAME_TRACE is
set: "1" prints to stderr, any other value is a file the lines are appended to (a Windows GUI
build has no stderr). Never applied to the repository, never shipped: gpui core stays
unpatched. Build the app against the copies with
    cargo build ... --config "patch.crates-io.gpui-pre.path='<copy>'"
(and the same for gpui-pre-windows on a Windows target); see scripts/perf/README.md.

usage: patch.py GPUI_PRE_COPY [GPUI_PRE_WINDOWS_COPY | - [GPUI_PRE_APPLE_COPY]]

[frame] fields, microseconds unless noted (t = wall clock, Unix microseconds, at the end of the frame):
  gap       since the previous presented frame        draws   draws since the previous present
  draw      whole draw()                              req     request_layout (view render)
  layout    compute_layout (taffy + text measuring)   prepaint  prepaint minus layout
  paint     paint                                     post    draw minus req/prepaint/paint
  present   platform_window.draw(scene): encode + submit + Present
  cache     time in the line-layout cache (lookups AND misses), outermost call only
  shape     time in the platform text shaper on cache misses (part of cache)
  cache_calls / shapes   outermost cache calls / shaper calls
  quads glyphs paths underlines   scene sizes
[win] (Windows only, written just before its [frame]):
  setup     pre_draw + upload of the scene buffers (batches = present - setup - swap)
  swap      swap_chain.Present(0, 0)
  idle      main thread blocked in GetMessageW since the previous frame
  wheel, paint_msg, task, timer, move, other (+ _n counts)
            time in the window procedures by message class since the previous frame; the outermost
            call only. paint_msg contains the draw itself; task is the foreground executor
            (WM_GPUI_TASK_DISPATCHED_ON_MAIN_THREAD): every cx.spawn / background-result update.
  fg_timeouts, deferred
            times the foreground-task loop hit its 10 ms budget; draws deferred as re-entrant.
  vs_n, vs_us, vs_max
            ticks of the VSyncProvider thread (DwmFlush, then it invalidates every window) since the
            previous frame: how many, their total wait, the longest.
[mac] (macOS only, written just before its [frame]; absent when the frame returned early):
  drawable  layer.next_drawable() - blocks while all drawables are in flight
  encode    render_frame: instance buffers + command encoding
  commit    present_drawable + commit (or commit + wait_until_scheduled + present)
"""
import re
import sys

root = sys.argv[1]
windows_root = sys.argv[2] if len(sys.argv) > 2 and sys.argv[2] != "-" else None
apple_root = sys.argv[3] if len(sys.argv) > 3 else None


def patch(path, edits, append=""):
    s = open(path).read()
    for old, new, count in edits:
        found = s.count(old)
        assert found == count, (path, old[:80], found, count)
        s = s.replace(old, new)
    open(path, "w").write(s + append)
    print("patched", path)


FRAME_TRACE = '''

pub(crate) mod frame_trace {
    use std::cell::Cell;
    use std::io::Write;
    use std::sync::{Mutex, OnceLock};
    use std::time::{Duration, Instant};

    pub fn on() -> bool {
        static ON: OnceLock<bool> = OnceLock::new();
        *ON.get_or_init(|| std::env::var_os("OKILUM_FRAME_TRACE").is_some())
    }
    /// One line to stderr, or appended to the file named by OKILUM_FRAME_TRACE.
    pub fn sink(line: &str) {
        static FILE: OnceLock<Option<Mutex<std::fs::File>>> = OnceLock::new();
        let file = FILE.get_or_init(|| {
            let value = std::env::var_os("OKILUM_FRAME_TRACE")?;
            if value == "1" {
                return None;
            }
            std::fs::OpenOptions::new().create(true).append(true).open(value).ok().map(Mutex::new)
        });
        match file {
            Some(file) => {
                let _ = writeln!(file.lock().unwrap(), "{line}");
            }
            None => eprintln!("{line}"),
        }
    }
    // 0 request_layout, 1 compute_layout, 2 prepaint (all, includes 1), 3 paint, 4 draw total,
    // 5 present, 6 line-layout cache (outermost calls), 7 shaping on cache misses
    thread_local! {
        pub static ACC: Cell<[u64; 8]> = const { Cell::new([0; 8]) };
        pub static COUNTS: Cell<[u32; 2]> = const { Cell::new([0; 2]) };
        pub static DEPTH: Cell<u32> = const { Cell::new(0) };
        pub static DRAWS: Cell<u32> = const { Cell::new(0) };
        pub static LAST: Cell<Option<Instant>> = const { Cell::new(None) };
        pub static COUNT: Cell<u64> = const { Cell::new(0) };
    }
    pub fn add(index: usize, d: Duration) {
        ACC.with(|a| {
            let mut v = a.get();
            v[index] += d.as_micros() as u64;
            a.set(v);
        });
    }
    pub fn draw_done() {
        DRAWS.with(|d| d.set(d.get() + 1));
    }
    /// Times one outermost call into the line-layout cache.
    pub struct Span(Option<Instant>);
    pub fn span() -> Span {
        if !on() {
            return Span(None);
        }
        DEPTH.with(|d| {
            let depth = d.get();
            d.set(depth + 1);
            Span((depth == 0).then(Instant::now))
        })
    }
    impl Drop for Span {
        fn drop(&mut self) {
            if !on() {
                return;
            }
            DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
            if let Some(start) = self.0 {
                add(6, start.elapsed());
                COUNTS.with(|c| {
                    let mut v = c.get();
                    v[0] += 1;
                    c.set(v);
                });
            }
        }
    }
    /// Times one call of the platform text shaper (a cache miss).
    pub fn shaped<T>(f: impl FnOnce() -> T) -> T {
        if !on() {
            return f();
        }
        let start = Instant::now();
        let out = f();
        add(7, start.elapsed());
        COUNTS.with(|c| {
            let mut v = c.get();
            v[1] += 1;
            c.set(v);
        });
        out
    }
    pub fn emit(scene: &crate::Scene) {
        let now = Instant::now();
        let gap = LAST.with(|l| {
            let g = l.get().map(|t| now.duration_since(t).as_micros() as u64);
            l.set(Some(now));
            g
        });
        let acc = ACC.with(|a| a.replace([0; 8]));
        let [cache_calls, shapes] = COUNTS.with(|c| c.replace([0; 2]));
        let draws = DRAWS.with(|d| d.replace(0));
        let n = COUNT.with(|c| {
            c.set(c.get() + 1);
            c.get()
        });
        let [req, cmp, pre, paint, draw, present, cache, shape] = acc;
        let post = draw.saturating_sub(req + pre + paint);
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_micros())
            .unwrap_or(0);
        sink(&format!(
            "[frame] t={t} n={n} gap={} draws={draws} draw={draw} req={req} layout={cmp} prepaint={} paint={paint} post={post} present={present} cache={cache} shape={shape} cache_calls={cache_calls} shapes={shapes} quads={} glyphs={} paths={} underlines={}",
            gap.map(|g| g as i64).unwrap_or(-1),
            pre.saturating_sub(cmp),
            scene.quads.len(),
            scene.monochrome_sprites.len() + scene.subpixel_sprites.len(),
            scene.paths.len(),
            scene.underlines.len(),
        ));
    }
}
'''

# ---- gpui-pre: window.rs (frame phases) -------------------------------------------------
patch(
    root + "/src/window.rs",
    [
        (
            "    pub fn draw(&mut self, cx: &mut App) -> ArenaClearNeeded {\n",
            "    pub fn draw(&mut self, cx: &mut App) -> ArenaClearNeeded {\n        let __draw_start = std::time::Instant::now();\n",
            1,
        ),
        (
            "        arena_scope.exit(&cx.element_arena)\n    }\n",
            "        if frame_trace::on() {\n            frame_trace::add(4, __draw_start.elapsed());\n            frame_trace::draw_done();\n        }\n        arena_scope.exit(&cx.element_arena)\n    }\n",
            1,
        ),
        (
            "        let root_layout_id = root_element.request_layout(self, cx);\n",
            "        let __t_req = std::time::Instant::now();\n        let root_layout_id = root_element.request_layout(self, cx);\n        if frame_trace::on() {\n            frame_trace::add(0, __t_req.elapsed());\n        }\n        let __t_pre = std::time::Instant::now();\n",
            1,
        ),
        (
            "        self.invalidator.set_phase(DrawPhase::Paint);\n        root_element.paint(self, cx);\n",
            "        self.invalidator.set_phase(DrawPhase::Paint);\n        if frame_trace::on() {\n            frame_trace::add(2, __t_pre.elapsed());\n        }\n        let __t_paint = std::time::Instant::now();\n        root_element.paint(self, cx);\n        if frame_trace::on() {\n            frame_trace::add(3, __t_paint.elapsed());\n        }\n",
            1,
        ),
        (
            "        let mut layout_engine = self.layout_engine.take().unwrap();\n        layout_engine.compute_layout(layout_id, available_space, self, cx);\n",
            "        let mut layout_engine = self.layout_engine.take().unwrap();\n        let __t_cmp = std::time::Instant::now();\n        layout_engine.compute_layout(layout_id, available_space, self, cx);\n        if frame_trace::on() {\n            frame_trace::add(1, __t_cmp.elapsed());\n        }\n",
            1,
        ),
        (
            "        self.platform_window.draw(&self.rendered_frame.scene);\n        #[cfg(feature = \"profiler\")]\n        self.window_profiler.record_present(",
            "        let __t_present = std::time::Instant::now();\n        self.platform_window.draw(&self.rendered_frame.scene);\n        if frame_trace::on() {\n            frame_trace::add(5, __t_present.elapsed());\n            frame_trace::emit(&self.rendered_frame.scene);\n        }\n        #[cfg(feature = \"profiler\")]\n        self.window_profiler.record_present(",
            1,
        ),
    ],
    append=FRAME_TRACE,
)

# ---- gpui-pre: line layout cache (lookups and shaping) ----------------------------------
LAYOUT = root + "/src/text_system/line_layout.rs"
s = open(LAYOUT).read()
# the two functions that start with the plain key and the two that start with the hashed key
for anchor in ("        let key = &CacheKeyRef {\n", "        let key_ref = HashedCacheKeyRef {\n"):
    assert s.count(anchor) == 2, (anchor, s.count(anchor))
    s = s.replace(anchor, "        let __span = crate::window::frame_trace::span();\n" + anchor)
# platform shaping on a miss: two sites, both `let mut layout = self .platform_text_system .layout_line(&text, font_size, runs);`
shaping = re.compile(
    r"let mut layout = self\s*\.platform_text_system\s*\.layout_line\(&text, font_size, runs\);"
)
assert len(shaping.findall(s)) == 2, len(shaping.findall(s))
s = shaping.sub(
    "let mut layout = crate::window::frame_trace::shaped(|| self.platform_text_system.layout_line(&text, font_size, runs));",
    s,
)
open(LAYOUT, "w").write(s)
print("patched", LAYOUT)

# ---- gpui-pre-windows: render / Present split --------------------------------------------
if windows_root:
    patch(
        windows_root + "/src/directx_renderer.rs",
        [
            (
                "        self.render(scene, background_appearance)?;\n        self.present()\n    }\n",
                "        self.render(scene, background_appearance)?;\n        let __t1 = std::time::Instant::now();\n        let __r = self.present();\n        win_trace::emit(__t1.elapsed());\n        __r\n    }\n",
                1,
            ),
            (
                "        self.pre_draw(&match background_appearance {",
                "        let __ts = std::time::Instant::now();\n        self.pre_draw(&match background_appearance {",
                1,
            ),
            (
                "        self.upload_scene_buffers(scene)?;\n",
                "        self.upload_scene_buffers(scene)?;\n        win_trace::set_setup(__ts.elapsed());\n",
                1,
            ),
        ],
        append='''

pub(crate) mod win_trace {
    use std::cell::Cell;
    use std::io::Write;
    use std::sync::{Mutex, OnceLock};
    use std::time::{Duration, Instant};

    fn on() -> bool {
        static ON: OnceLock<bool> = OnceLock::new();
        *ON.get_or_init(|| std::env::var_os("OKILUM_FRAME_TRACE").is_some())
    }
    use std::sync::atomic::{AtomicU64, Ordering};
    // The vsync thread (DwmFlush) ticks on its own thread: count, total and longest wait since the last frame.
    static VS_N: AtomicU64 = AtomicU64::new(0);
    static VS_US: AtomicU64 = AtomicU64::new(0);
    static VS_MAX: AtomicU64 = AtomicU64::new(0);
    pub fn vsync(d: Duration) {
        if on() {
            let us = d.as_micros() as u64;
            VS_N.fetch_add(1, Ordering::Relaxed);
            VS_US.fetch_add(us, Ordering::Relaxed);
            VS_MAX.fetch_max(us, Ordering::Relaxed);
        }
    }
    // Message classes: 0 wheel, 1 paint (includes the draw), 2 foreground tasks, 3 timer, 4 mouse move, 5 other.
    thread_local! {
        static SETUP: Cell<u64> = const { Cell::new(0) };
        static MSG_US: Cell<[u64; 6]> = const { Cell::new([0; 6]) };
        static MSG_N: Cell<[u32; 6]> = const { Cell::new([0; 6]) };
        static IDLE_US: Cell<u64> = const { Cell::new(0) };
        static DEPTH: Cell<u32> = const { Cell::new(0) };
    }
    thread_local! {
        static EVENTS: Cell<[u32; 2]> = const { Cell::new([0; 2]) };
    }
    /// 0: the foreground-task loop hit its 10 ms budget; 1: a draw was deferred as re-entrant.
    pub fn count(which: usize) {
        if on() {
            EVENTS.with(|c| {
                let mut v = c.get();
                v[which] += 1;
                c.set(v);
            });
        }
    }
    pub fn set_setup(d: Duration) {
        if on() {
            SETUP.with(|s| s.set(d.as_micros() as u64));
        }
    }
    /// Time the main thread spent blocked in GetMessageW (nothing to do).
    pub fn add_idle(d: Duration) {
        if on() {
            IDLE_US.with(|c| c.set(c.get() + d.as_micros() as u64));
        }
    }
    fn classify(msg: u32) -> usize {
        match msg {
            0x020A => 0, // WM_MOUSEWHEEL
            0x000F => 1, // WM_PAINT
            0x0403 => 2, // WM_GPUI_TASK_DISPATCHED_ON_MAIN_THREAD (WM_USER + 3)
            0x0113 => 3, // WM_TIMER
            0x0200 => 4, // WM_MOUSEMOVE
            _ => 5,
        }
    }
    /// Times one window-procedure call; only the outermost counts (a foreground task that pumps
    /// messages re-enters the procedure, and that time is already inside the task).
    pub struct MsgSpan {
        tracked: bool,
        outer: bool,
        class: usize,
        start: Option<Instant>,
    }
    pub fn msg_span(msg: u32) -> MsgSpan {
        let tracked = on();
        let mut outer = false;
        if tracked {
            DEPTH.with(|d| {
                outer = d.get() == 0;
                d.set(d.get() + 1);
            });
        }
        MsgSpan { tracked, outer, class: classify(msg), start: tracked.then(Instant::now) }
    }
    impl Drop for MsgSpan {
        fn drop(&mut self) {
            if !self.tracked {
                return;
            }
            DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
            if let (true, Some(start)) = (self.outer, self.start) {
                let us = start.elapsed().as_micros() as u64;
                MSG_US.with(|c| {
                    let mut v = c.get();
                    v[self.class] += us;
                    c.set(v);
                });
                MSG_N.with(|c| {
                    let mut v = c.get();
                    v[self.class] += 1;
                    c.set(v);
                });
            }
        }
    }
    pub fn emit(swap: Duration) {
        if !on() {
            return;
        }
        let setup = SETUP.with(|s| s.replace(0));
        let us = MSG_US.with(|c| c.replace([0; 6]));
        let n = MSG_N.with(|c| c.replace([0; 6]));
        let idle = IDLE_US.with(|c| c.replace(0));
        let [fg_timeouts, deferred] = EVENTS.with(|c| c.replace([0; 2]));
        let (vs_n, vs_us, vs_max) = (VS_N.swap(0, Ordering::Relaxed), VS_US.swap(0, Ordering::Relaxed), VS_MAX.swap(0, Ordering::Relaxed));
        static FILE: OnceLock<Option<Mutex<std::fs::File>>> = OnceLock::new();
        let file = FILE.get_or_init(|| {
            let value = std::env::var_os("OKILUM_FRAME_TRACE")?;
            if value == "1" {
                return None;
            }
            std::fs::OpenOptions::new().create(true).append(true).open(value).ok().map(Mutex::new)
        });
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_micros())
            .unwrap_or(0);
        let line = format!(
            "[win] t={t} setup={setup} swap={} idle={idle} wheel={} wheel_n={} paint_msg={} paint_n={} task={} task_n={} timer={} timer_n={} move={} move_n={} other={} other_n={} vs_n={vs_n} vs_us={vs_us} vs_max={vs_max} fg_timeouts={fg_timeouts} deferred={deferred}",
            swap.as_micros(), us[0], n[0], us[1], n[1], us[2], n[2], us[3], n[3], us[4], n[4], us[5], n[5],
        );
        match file {
            Some(file) => {
                let _ = writeln!(file.lock().unwrap(), "{line}");
            }
            None => eprintln!("{line}"),
        }
    }
}
''',
    )

if windows_root:
    patch(
        windows_root + "/src/window.rs",
        [
            (
                "    let inner = unsafe { &*ptr };\n    let result = if let Some(inner) = inner.upgrade() {\n        inner.handle_msg(hwnd, msg, wparam, lparam)",
                "    let inner = unsafe { &*ptr };\n    let __span = crate::directx_renderer::win_trace::msg_span(msg);\n    let result = if let Some(inner) = inner.upgrade() {\n        inner.handle_msg(hwnd, msg, wparam, lparam)",
                1,
            ),
        ],
    )
    patch(
        windows_root + "/src/events.rs",
        [
            (
                "            log::debug!(\"deferring re-entrant draw of window {handle:?}\");\n",
                "            log::debug!(\"deferring re-entrant draw of window {handle:?}\");\n            crate::directx_renderer::win_trace::count(1);\n",
                1,
            ),
        ],
    )
    patch(
        windows_root + "/src/platform.rs",
        [
            (
                "                    log::debug!(\"foreground task timeout reached\");\n",
                "                    log::debug!(\"foreground task timeout reached\");\n                    crate::directx_renderer::win_trace::count(0);\n",
                1,
            ),
            (
                "                    vsync_provider.wait_for_vsync();\n                    if check_device_lost(&directx_device.device)",
                "                    let __v = std::time::Instant::now();\n                    vsync_provider.wait_for_vsync();\n                    crate::directx_renderer::win_trace::vsync(__v.elapsed());\n                    if check_device_lost(&directx_device.device)",
                1,
            ),
            (
                "    let inner = unsafe { &*ptr };\n    let result = if let Some(inner) = inner.upgrade() {\n        if cfg!(debug_assertions) {",
                "    let inner = unsafe { &*ptr };\n    let __span = crate::directx_renderer::win_trace::msg_span(msg);\n    let result = if let Some(inner) = inner.upgrade() {\n        if cfg!(debug_assertions) {",
                1,
            ),
            (
                "            while GetMessageW(&mut msg, None, 0, 0).as_bool() {\n                if translate_accelerator(&msg).is_none() {",
                "            loop {\n                let __g = std::time::Instant::now();\n                let __got = GetMessageW(&mut msg, None, 0, 0).as_bool();\n                crate::directx_renderer::win_trace::add_idle(__g.elapsed());\n                if !__got {\n                    break;\n                }\n                if translate_accelerator(&msg).is_none() {",
                1,
            ),
        ],
    )

# ---- gpui-pre-apple: where a Metal present waits ------------------------------------------
if apple_root:
    patch(
        apple_root + "/src/metal_renderer.rs",
        [
            (
                "        let drawable = if let Some(drawable) = layer.next_drawable() {\n            drawable\n        } else {",
                "        let __td = std::time::Instant::now();\n        let __next = layer.next_drawable();\n        mac_trace::set(0, __td.elapsed());\n        let drawable = if let Some(drawable) = __next {\n            drawable\n        } else {",
                1,
            ),
            (
                "        let command_buffer = match self.render_frame(scene, drawable.texture(), viewport_size) {\n            Ok(command_buffer) => command_buffer,",
                "        let __te = std::time::Instant::now();\n        let __rendered = self.render_frame(scene, drawable.texture(), viewport_size);\n        mac_trace::set(1, __te.elapsed());\n        let command_buffer = match __rendered {\n            Ok(command_buffer) => command_buffer,",
                1,
            ),
            (
                "        if self.presents_with_transaction {\n            command_buffer.commit();\n            command_buffer.wait_until_scheduled();\n            drawable.present();\n        } else {\n            command_buffer.present_drawable(drawable);\n            command_buffer.commit();\n        }\n    }\n",
                "        let __tc = std::time::Instant::now();\n        if self.presents_with_transaction {\n            command_buffer.commit();\n            command_buffer.wait_until_scheduled();\n            drawable.present();\n        } else {\n            command_buffer.present_drawable(drawable);\n            command_buffer.commit();\n        }\n        mac_trace::set(2, __tc.elapsed());\n        mac_trace::emit();\n    }\n",
                1,
            ),
        ],
        append='''

mod mac_trace {
    use std::cell::Cell;
    use std::io::Write;
    use std::sync::{Mutex, OnceLock};
    use std::time::Duration;

    fn on() -> bool {
        static ON: OnceLock<bool> = OnceLock::new();
        *ON.get_or_init(|| std::env::var_os("OKILUM_FRAME_TRACE").is_some())
    }
    thread_local! {
        static ACC: Cell<[u64; 3]> = const { Cell::new([0; 3]) };
    }
    pub fn set(index: usize, d: Duration) {
        if on() {
            ACC.with(|a| {
                let mut v = a.get();
                v[index] = d.as_micros() as u64;
                a.set(v);
            });
        }
    }
    pub fn emit() {
        if !on() {
            return;
        }
        let [drawable, encode, commit] = ACC.with(|a| a.replace([0; 3]));
        static FILE: OnceLock<Option<Mutex<std::fs::File>>> = OnceLock::new();
        let file = FILE.get_or_init(|| {
            let value = std::env::var_os("OKILUM_FRAME_TRACE")?;
            if value == "1" {
                return None;
            }
            std::fs::OpenOptions::new().create(true).append(true).open(value).ok().map(Mutex::new)
        });
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_micros())
            .unwrap_or(0);
        let line = format!("[mac] t={t} drawable={drawable} encode={encode} commit={commit}");
        match file {
            Some(file) => {
                let _ = writeln!(file.lock().unwrap(), "{line}");
            }
            None => eprintln!("{line}"),
        }
    }
}
''',
    )
print("done")

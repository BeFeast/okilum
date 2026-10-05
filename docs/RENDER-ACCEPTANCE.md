# Render acceptance — scroll frame pacing, for the record

`docs/PRD.md` §7 closed the framework gate on 2026-09-05 (GPUI go, judged in
daily use). This file is not a gate. It records how scroll frame pacing is
measured, and what the current vendor measures on the evidence rig, so the
number has a home and a rerunnable method the next time a vendor bump lands.

Issue: #50. Measured 2026-09-05 on `main` @ `735d4b5` (vendor gpui-kit
`928c3eb` + patches 0004/0006, `gpui-pre` 0.3.3).

## Hardware and software under test

| | |
|---|---|
| Host | linux-test-host — ThinkPad T460, Intel Core i7-6600U (2c/4t), 8 GB |
| GPU | Intel HD Graphics 520 (SKL GT2), `i915` + ANV |
| Mesa | 26.2.2-arch3.2 (`vulkaninfo --summary`: Vulkan 1.4.354) |
| Kernel | 7.2.2-1-cachyos |
| Compositor | Hyprland 0.56.2, nested evidence rig (Wayland backend, no seat), output `HEADLESS-1` 1920x1080@60, scale 1 |
| Window | Tessera fullscreen on that output, 1920x1080 |
| Note | `Dev/Areas/ok-player/_index.md` from the spike corpus snapshot — 131 641 B, the same 132 KB note the spike measured |
| Index | throwaway `--index-dir` on tmpfs |

The spike's historical row was taken on **different hardware** (Probe G on
linux-reference-host, Latitude 7420 / Intel Iris Xe / Mesa 26.2.1; Probe 2 on legacy-gpu-host, i5-6500 /
Radeon R7 360). It is kept here because it is the number the issue quotes, not
because it is a baseline for this host. Same-host comparison is the
pre-vendor-bump binary section below.

## Method

Two instruments, both reused from the spike. Run all three of each; a single
run on this host is within ~1 ms of the others, so three is enough to show that.

### A. Self-driven sweep (spike parity — this is what produced p50 16.9 / p99 40.9)

The spike reader (`~/dev/spikes/tessera/gpui-reader/src/main.rs`, `--sweep`)
scrolls the list by 120 px once per frame from `Window::on_next_frame` until the
bottom, recording the inter-frame interval. No input injection, so it is immune
to seat and pointer-focus problems, and it includes the app's own re-render.
It is a worst-case stress: 120 px every frame is ~1.5x faster than the injected
wheel below.

The product binary has no such flag (the port removed the probe instrumentation
on purpose). For a rerun, add it temporarily to `crates/tessera-shell/src/main.rs`
and do not commit it: an `Opts.sweep: bool` set by `"--sweep"` in `main()`, and
this block at the end of `Reader::new`, immediately before `this` is returned
(verbatim from the spike, with p95 added):

```rust
if opts.sweep {
    fn sweep_step(
        list: gpui::ListState,
        view_id: gpui::EntityId,
        times: Rc<std::cell::RefCell<Vec<std::time::Instant>>>,
        window: &mut Window,
        cx: &mut App,
    ) {
        times.borrow_mut().push(std::time::Instant::now());
        list.scroll_by(px(120.));
        cx.notify(view_id);
        let px_off = -f32::from(list.scroll_px_offset_for_scrollbar().y);
        let max_off = f32::from(list.max_offset_for_scrollbar().y);
        let n = times.borrow().len();
        if px_off < max_off - 1.0 && n < 1000 {
            let l = list.clone();
            let t = times.clone();
            window.on_next_frame(move |w, cx| sweep_step(l, view_id, t, w, cx));
        } else {
            let t = times.borrow();
            let mut dts: Vec<f64> = t
                .windows(2)
                .map(|w| w[1].duration_since(w[0]).as_secs_f64() * 1000.0)
                .collect();
            dts.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let m = dts.len();
            if m == 0 {
                eprintln!("sweep: no frames");
                return;
            }
            let sum: f64 = dts.iter().sum();
            let gt17 = dts.iter().filter(|d| **d > 17.5).count();
            let gt25 = dts.iter().filter(|d| **d > 25.0).count();
            eprintln!(
                "sweep: frames={} px_end={px_off:.0} max={max_off:.0} dt_min={:.2} p50={:.2} p90={:.2} p95={:.2} p99={:.2} dt_max={:.2} mean={:.2} gt17.5={gt17} gt25={gt25}",
                m + 1, dts[0], dts[m / 2], dts[m * 9 / 10], dts[m * 95 / 100],
                dts[(m * 99 / 100).min(m - 1)], dts[m - 1], sum / m as f64,
            );
        }
    }
    let content = this.content.clone();
    let view_id = cx.entity().entity_id();
    cx.spawn_in(window, async move |_, cx| {
        cx.background_executor().timer(Duration::from_millis(2000)).await;
        let _ = cx.update(move |window, cx| {
            let list = content.read(cx).list_state().clone();
            let times = Rc::new(std::cell::RefCell::new(Vec::new()));
            sweep_step(list, view_id, times, window, cx);
        });
    })
    .detach();
}
```

Run, inside the rig (`. ~/rig/env.sh`), then focus and fullscreen the window by
address with `rigctl dispatch focuswindow` / `fullscreen 0`; the sweep starts 2 s
after the window opens and prints one `sweep:` line to stderr:

```
tessera --vault ~/dev/spikes/tessera/corpus --index-dir /tmp/tessera-idx \
        --note Dev/Areas/ok-player/_index.md --sweep
```

### B. Injected wheel stream + compositor frame callbacks (no code change)

Measures the shipped binary as a user drives it. 500 wheel detents at 15 ms
(66.7 Hz) — the spike's "500-tick wheel sweep" — through the rig's virtual
pointer, and frame timing from the client's own `wl_surface.frame` callbacks
under `WAYLAND_DEBUG=1`: one `wl_callback.done` per presented frame, timestamped
by libwayland at the client. This is the frame rate the compositor actually
paced the app at, so it saturates at the 60 Hz output (16.7 ms floor).

```
env WAYLAND_DEBUG=1 tessera --vault ~/dev/spikes/tessera/corpus \
    --index-dir /tmp/tessera-idx --note Dev/Areas/ok-player/_index.md 2>run.log &
# focus + fullscreen by address, wait 3 s
rigctl dispatch movecursor 1200 600      # see trap below
~/rig/ptr.sh move 1200 600
python3 ~/dev/spikes/tessera/scripts/wheel-stream.py 500 15
```

Then keep only the `wl_callback#N.done` lines whose `N` was created by a
`wl_surface#S.frame(new id wl_callback#N)` request (the app also uses
`wl_display.sync` callbacks, which are not frames), restrict to the window
between the last log timestamp before the stream and the first after it, and
take percentiles of consecutive deltas. Count `wl_pointer.axis_discrete` in the
same window as a control: it must be 500, or the stream did not reach the app.

**Trap (cost 20 minutes here):** a fresh `rigptr` virtual pointer does not give
the window pointer focus. `ptr.sh move` updates `hyprctl cursorpos` but the app
never receives `wl_pointer.enter`, and every wheel event goes nowhere with no
error — the axis-event count reads 0 and the frame count stays at the idle 3.
One `hyprctl -i "$RIG_SIG" dispatch movecursor X Y` first makes the compositor
send `enter`; after that `rigptr` motion and wheel are delivered normally. The
spike scripts never hit this because they ran on a rig whose real pointer had
already entered the window.

### Rig discipline

Rig only when `loginctl list-sessions` shows only `greeter` on seat0 and
`pgrep -c Hyprland` is 0. `systemctl --user start hypr-desktop`, `~/rig/start.sh`,
run, then `~/rig/stop.sh` and `systemctl --user stop hypr-desktop`. Release build
before the compositor is up (2 cores / 8 GB). GPU error counter —
`journalctl -k | grep -cE 'GPU HANG|GPU reset|Resetting|NULL pointer dereference'`
— before, after every graphical step, and after teardown. This session: 0 → 0
throughout, 20 app launches.

## Results — current vendor (`main` @ `735d4b5`, gpui-kit 928c3eb, gpui-pre 0.3.3)

Frame intervals in ms. `>17.5` / `>25` are counts of frames over those bounds.

### A. Self-driven sweep, 132 KB note (spike-parity instrument)

| Run | Frames | p50 | p90 | p95 | p99 | max | mean | >17.5 | >25 |
|---|---|---|---|---|---|---|---|---|---|
| 1 | 178 | 36.35 | 63.55 | 67.73 | 82.42 | 87.01 | 39.69 | 176 | 154 |
| 2 | 178 | 36.67 | 63.24 | 68.44 | 83.93 | 85.82 | 39.56 | 176 | 154 |
| 3 | 178 | 36.83 | 63.33 | 68.83 | 80.21 | 86.90 | 39.71 | 176 | 156 |
| historical — spike Probe G, 2026-09-01, linux-reference-host (Iris Xe), gpui-component 51505e9 | 178 | 16.88 | 27.99 | — | 40.88 | 42.32 | 19.45 | 64 | 29 |
| historical — spike Probe 2, 2026-09-02, legacy-gpu-host (R7 360), unpatched / patched | 178 | 16.74 / 16.74 | 31.44 / 31.94 | — | 43.32 / 44.10 | — | — | — | 32 / 32 |

Control on this host, same instrument, 64 KB note
(`HomeLab/Resources/Runbooks/cliproxyapi-maestro.md`): 122 frames, p50 21.43,
p95 24.44, p99 36.95, max 55.37. The 132 KB note is roughly twice as expensive
per frame as the 64 KB one on this GPU, which is what a per-frame cost
proportional to visible layout work looks like.

### B. Injected wheel, 500 detents at 66.7 Hz, 132 KB note

| Run | Frames | axis events | p50 | p90 | p95 | p99 | max | mean | >17.5 | >25 |
|---|---|---|---|---|---|---|---|---|---|---|
| 1 | 279 | 500 | 20.03 | 43.05 | 58.37 | 71.00 | 89.17 | 27.98 | 262 | 119 |
| 2 | 276 | 500 | 20.12 | 43.67 | 57.93 | 75.45 | 86.63 | 28.24 | 263 | 122 |
| 3 | 276 | 500 | 20.00 | 44.07 | 57.58 | 80.53 | 99.94 | 28.28 | 261 | 119 |

## Same host, pre-vendor-bump binaries (comparison only, not rebuilt)

Two release binaries from 2026-09-04 were still on linux-test-host in `/tmp`, built from
`main` before #39/#45 moved the vendor from gpui-component `a10352a` + gpui
0.2.2 (zed git `f66ed39`) to gpui-kit + `gpui-pre`. Instrument B only (no code
change possible on a prebuilt binary). Same rig, same note, same session.

| Binary | Run | Frames | p50 | p90 | p95 | p99 | max | mean |
|---|---|---|---|---|---|---|---|---|
| `tessera-pre27` — main before #27 (fonts/heading scale), built 11:07 | 1 | 319 | 20.90 | 37.20 | 45.05 | 55.20 | 78.07 | 24.45 |
| | 2 | 317 | 20.94 | 37.11 | 45.16 | 56.92 | 77.69 | 24.64 |
| | 3 | 316 | 20.85 | 37.62 | 44.88 | 57.08 | 78.55 | 24.69 |
| `tessera-main` — main @ ~`561ef44` (with #27), built 11:45 | 1 | 267 | 31.00 | 44.19 | 49.34 | 56.45 | 57.23 | 29.40 |
| | 2 | 270 | 27.59 | 45.04 | 48.21 | 54.87 | 57.83 | 29.07 |
| | 3 | 274 | 26.89 | 43.58 | 47.03 | 55.56 | 57.70 | 28.67 |

## Interpretation

Vsync on a 60 Hz output is 16.7 ms per frame. p50 at 20 ms under a real wheel
stream means the reader misses roughly every fourth vblank while scrolling this
note on this GPU — smooth enough that daily use on linux-reference-host was judged excellent,
but not 60 fps. What p99 says is different: at 71–80 ms, one scroll frame in a
hundred (three to four per 500-detent flick) is held for four to five vblanks,
which reads as a visible hitch, and the maxima near 90–100 ms are single
frames where the page appears to stall. The self-driven sweep is harsher (p50
36 ms, p99 80+) because it pushes 120 px of new layout every frame; it is the
stress case, not the user experience, and it is the only instrument comparable
to the spike's row. The spike numbers were on a much stronger iGPU, so the gap
between 16.9 and 36 ms is mostly HD 520 vs Iris Xe, not a regression; the
same-host comparison is the one that carries information: against the
2026-09-04 binaries, the current build has the same p50 as `pre27` (~20 ms),
a better p50 than `main` with #27 (27–31 → 20), and a **worse tail** (p99 55–57
→ 71–80 ms, max 58–78 → 87–100 ms). That delta bundles the vendor bump with
everything the shell gained since (callouts, find, highlights), so it is not
attributed here. Upstream's `fps: Sustain frames by default` (gpui-kit #2944)
touches only `crates/fps` and the shell's FPS widget — the on-screen rate
monitor, not the frame loop — so it cannot have moved these numbers either way.
If the tail is ever worth chasing, the next step is instrument B on the current
tree with #45 reverted alone, which isolates the vendor from the shell.

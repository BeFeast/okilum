//! Isolated native #216 fixture. No BrainView, RPC, persistence or live-note access.
#[path = "../src/source_presentation.rs"]
mod adapter;
#[path = "../src/platform/exact_wayland_clipboard.rs"]
mod exact_wayland_clipboard;

use std::{borrow::Cow, sync::Arc, time::Duration};

use adapter::{CachedProvider, BODY_FONT};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use gpui::{prelude::FluentBuilder as _, *};
use gpui_component::{
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{
        attribution::{self, Identity as TraceIdentity, Span as TraceSpan},
        clipboard::{
            ClipboardPasteEvent, ClipboardReadError, ClipboardReadRequest, ExactClipboardProvider,
        },
        projection::{ProjectionProvider, SourceMutation, SourceSnapshot},
        Editor, EditorState, InputEvent, WrappingIndent,
    },
    v_flex, Root,
};
use serde_json::json;
use sha2::{Digest, Sha256};

const FIXTURE: &str = include_str!("decoration784/fixture.md");
const FIXTURE_HASH: &str = include_str!("decoration784/fixture.sha256");

actions!(
    projection_fixture,
    [SnapshotEvidence, ToggleMode, DumpAttribution, ResetFixture]
);

// This opt-in evidence clock matches the Wayland helper on the Linux x86_64 rig.
// Keep the binding local to the experiment; the product has no timing dependency.
#[cfg(all(target_os = "linux", target_pointer_width = "64"))]
fn monotonic_ns() -> Option<u64> {
    #[repr(C)]
    struct Timespec {
        seconds: std::ffi::c_long,
        nanoseconds: std::ffi::c_long,
    }
    unsafe extern "C" {
        fn clock_gettime(clock: std::ffi::c_int, time: *mut Timespec) -> std::ffi::c_int;
    }
    let mut time = Timespec {
        seconds: 0,
        nanoseconds: 0,
    };
    // SAFETY: Linux 64-bit timespec has two C longs; the writable value outlives
    // the call. CLOCK_MONOTONIC is 1 on Linux. No pointer escapes this function.
    if unsafe { clock_gettime(1, &mut time) } != 0
        || time.seconds < 0
        || !(0..1_000_000_000).contains(&time.nanoseconds)
    {
        return None;
    }
    (time.seconds as u64)
        .checked_mul(1_000_000_000)?
        .checked_add(time.nanoseconds as u64)
}

#[cfg(not(all(target_os = "linux", target_pointer_width = "64")))]
fn monotonic_ns() -> Option<u64> {
    None
}

fn timing_clock_sample() {
    let before = monotonic_ns();
    let wall = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok();
    let after = monotonic_ns();
    println!(
        "{}",
        json!({"event":"timing_clock_sample","monotonic_before_ns":before,
        "wall_time_ns":wall.map(|value| value.as_nanos() as u64),"monotonic_after_ns":after})
    );
}

struct Fixture {
    editor: Entity<EditorState>,
    _subscriptions: Vec<Subscription>,
    accepted: Option<Arc<CachedProvider>>,
    first_result: Option<Arc<CachedProvider>>,
    live: bool,
    timing: bool,
    legacy_marker: bool,
    readonly: bool,
    delayed: bool,
    in_flight: bool,
    queued: bool,
    changes: u64,
    mutations: u64,
    rejected: u64,
    status: String,
}

impl Fixture {
    fn new(
        window: &mut Window,
        cx: &mut Context<Self>,
        clipboard: Arc<dyn ExactClipboardProvider>,
    ) -> Self {
        let timing = std::env::var("OKILUM_NATIVE216_TIMING").as_deref() == Ok("1");
        let spans = std::env::var("OKILUM_NATIVE221_SPANS").as_deref() == Ok("1");
        let legacy_marker =
            timing && std::env::var("OKILUM_NATIVE221_LEGACY_MARKER").as_deref() != Ok("0");
        attribution::init(spans);
        println!(
            "{}",
            json!({"event":"attribution_configuration", "spans":spans,
            "legacy_marker":legacy_marker,"timing":timing,"capacity":attribution::CAPACITY,
            "overflow_policy":"invalidate_entire_dump_interval"})
        );
        if timing {
            monotonic_ns().expect("native216 timing requires Linux 64-bit CLOCK_MONOTONIC");
            timing_clock_sample();
        }
        let editor = cx.new(|cx| {
            let mut editor = EditorState::new(window, cx)
                .line_number(false)
                .folding(false)
                .searchable(false)
                .replaceable(false)
                .soft_wrap(true)
                .wrapping_indent(WrappingIndent::None);
            editor.set_value(FIXTURE, window, cx);
            editor.set_exact_clipboard_provider(Some(clipboard), cx);
            editor
        });
        let change_sub = cx.subscribe(&editor, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.changes += 1;
                this.record("input_change", cx);
                cx.notify();
            }
        });
        let mutation_sub = cx.subscribe_in(
            &editor,
            window,
            |this, _, event: &SourceMutation, window, cx| {
                if this.timing {
                    let observed_ns = monotonic_ns();
                    println!("{}", json!({"event":"timing_source_mutation_observed",
                        "monotonic_ns":observed_ns,"document":event.stamp.document,
                        "generation":event.stamp.generation,"reason":format!("{:?}",event.reason),
                        "live":this.live}));
                    timing_clock_sample();
                }
                this.mutations += 1;
                println!("{}", json!({"event":"mutation_notice","document":event.stamp.document,"generation":event.stamp.generation,"reason":format!("{:?}",event.reason)}));
                this.record("source_mutation", cx);
                this.schedule(window, cx);
                cx.notify();
            },
        );
        let clipboard_sub = cx.subscribe(&editor, |this, _, event: &ClipboardPasteEvent, cx| {
            this.status = format!("Exact clipboard: {event:?}");
            println!(
                "{}",
                json!({"event":"exact_clipboard", "outcome":format!("{event:?}")})
            );
            cx.notify();
        });
        let mut fixture = Self {
            editor,
            _subscriptions: vec![change_sub, mutation_sub, clipboard_sub],
            accepted: None,
            first_result: None,
            live: true,
            timing,
            legacy_marker,
            readonly: false,
            delayed: false,
            in_flight: false,
            queued: false,
            changes: 0,
            mutations: 0,
            rejected: 0,
            status: "Classifying synthetic source…".into(),
        };
        fixture.schedule(window, cx);
        fixture.refocus(window, cx);
        fixture
    }

    fn refocus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.editor.read(cx).focus_handle(cx).focus(window, cx);
    }

    fn snapshot(&self, cx: &App) -> Option<SourceSnapshot> {
        let editor = self.editor.read(cx);
        if editor.text().len() > okilum_core::source_classifier::MAX_BYTES {
            return None;
        }
        Some(SourceSnapshot {
            stamp: editor.source_stamp(),
            text: Arc::from(editor.value().as_ref()),
        })
    }

    fn publish(&mut self, provider: Arc<CachedProvider>, cx: &mut Context<Self>) -> bool {
        let mut trace = TraceSpan::new(
            "fixture_publish",
            TraceIdentity::source(provider.source().stamp, 0),
        );
        let Some(current) = self.snapshot(cx) else {
            self.rejected += 1;
            return false;
        };
        if current.stamp != provider.source().stamp || current.text != provider.source().text {
            self.rejected += 1;
            println!(
                "{}",
                json!({"event":"stale_classification_rejected","current_generation":current.stamp.generation,"result_generation":provider.source().stamp.generation})
            );
            return false;
        }
        if self.first_result.is_none() {
            self.first_result = Some(provider.clone());
        }
        self.status = format!(
            "Current classification; Source reasons {}",
            provider.reasons()
        );
        self.accepted = Some(provider.clone());
        if self.live {
            self.editor.update(cx, |editor, cx| {
                editor.set_projection_provider(Some(provider as Arc<dyn ProjectionProvider>), cx)
            });
        }
        self.record("classification_accepted", cx);
        let editor = self.editor.read(cx);
        trace.current(TraceIdentity::source(
            editor.source_stamp(),
            editor.presentation_epoch(),
        ));
        true
    }

    fn schedule(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.in_flight {
            self.queued = true;
            return;
        }
        let Some(source) = self.snapshot(cx) else {
            self.accepted = None;
            self.status = "Exact Source fallback: over 64 KiB; full buffer retained".into();
            self.editor
                .update(cx, |editor, cx| editor.set_projection_provider(None, cx));
            self.record("size_fallback", cx);
            return;
        };
        let identity =
            TraceIdentity::source(source.stamp, self.editor.read(cx).presentation_epoch());
        let enqueue = TraceSpan::new("classifier_enqueue", identity);
        let job = enqueue.id();
        drop(enqueue);
        self.in_flight = true;
        self.status = "Classifying in background; current source remains editable".into();
        let delay = if self.delayed { 500 } else { 0 };
        cx.spawn_in(window, async move |this, cx| {
            let provider = cx
                .background_executor()
                .spawn(async move {
                    if delay != 0 {
                        std::thread::sleep(Duration::from_millis(delay));
                    }
                    let mut trace = TraceSpan::new("classifier_work", identity);
                    trace.counts([job, 0, 0, 0]);
                    Arc::new(CachedProvider::classify(source))
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                let mut trace = TraceSpan::new("classifier_ui_resume", identity);
                trace.counts([job, 0, 0, 0]);
                this.in_flight = false;
                this.publish(provider, cx);
                if std::mem::take(&mut this.queued) {
                    this.schedule(window, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn toggle(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.live = !self.live;
        let provider = if self.live {
            self.accepted
                .clone()
                .map(|p| p as Arc<dyn ProjectionProvider>)
        } else {
            None
        };
        self.editor.update(cx, |editor, cx| {
            editor.set_projection_provider(provider, cx)
        });
        self.refocus(window, cx);
        self.record("mode_changed", cx);
        cx.notify();
    }

    fn reset(&mut self, large: bool, window: &mut Window, cx: &mut Context<Self>) {
        let text = if large {
            "x".repeat(okilum_core::source_classifier::MAX_BYTES + 1)
        } else {
            FIXTURE.to_owned()
        };
        self.editor
            .update(cx, |editor, cx| editor.set_value(text, window, cx));
        self.refocus(window, cx);
        self.record("explicit_history_reset", cx);
    }

    fn record(&self, event: &str, cx: &App) {
        let editor = self.editor.read(cx);
        let source = editor.value();
        let stamp = editor.source_stamp();
        let selected = editor.selected_range();
        let anchor = source
            .find("ANCHOR")
            .and_then(|offset| editor.range_to_bounds(&(offset..offset + 6)));
        println!(
            "{}",
            json!({"event":"anchor_geometry", "live":self.live,
            "bounds":anchor.map(|b| [f32::from(b.origin.x), f32::from(b.origin.y), f32::from(b.size.width), f32::from(b.size.height)]),
            "scroll_y":f32::from(editor.scroll_offset().y)})
        );
        println!(
            "{}",
            json!({
                "event":event,"fixture_sha256":FIXTURE_HASH.split_whitespace().next(),
                "entity":format!("{:?}",self.editor.entity_id()),"document":stamp.document,"generation":stamp.generation,
                "presentation_epoch":editor.presentation_epoch(),"live":self.live,"readonly":self.readonly,
                "source_bytes":source.len(),"source_sha256":format!("{:x}",Sha256::digest(source.as_bytes())),
                "source_base64":STANDARD.encode(source.as_bytes()),"selection":[selected.start,selected.end],"cursor":editor.cursor(),
                "input_changes":self.changes,"source_mutations":self.mutations,"rejected_results":self.rejected,
                "classification_in_flight":self.in_flight,"classification_queued":self.queued
            })
        );
    }
}

impl Render for Fixture {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.editor.read(cx);
        let stamp = state.source_stamp();
        let presentation_epoch = state.presentation_epoch();
        let live = self.live;
        let paint_editor = self.editor.clone();
        let summary = format!(
            "source {}:{} · presentation {} · bytes {} · Change {} · mutation {} · stale {}",
            stamp.document,
            stamp.generation,
            state.presentation_epoch(),
            state.text().len(),
            self.changes,
            self.mutations,
            self.rejected
        );
        v_flex().size_full().p_4().gap_3().key_context("ProjectionFixture")
            .on_action(cx.listener(|this, _: &SnapshotEvidence, _, cx| this.record("keyboard_snapshot", cx)))
            .on_action(cx.listener(|_, _: &DumpAttribution, _, _| {
                println!("{}", json!({"event":"attribution_dump", "trace":attribution::dump()}));
                println!("{}", json!({"event":"attribution_dump_complete", "monotonic_ns":monotonic_ns()}));
            }))
            .on_action(cx.listener(|this, _: &ResetFixture, window, cx| this.reset(false, window, cx)))
            .on_action(cx.listener(|this, _: &ToggleMode, window, cx| this.toggle(window, cx)))
            .child(div().text_lg().child("Native source projection · isolated #216 fixture"))
            .child(h_flex().gap_2().flex_wrap()
                .child(Button::new("projection-mode").label(if self.live { "Live Preview (F7)" } else { "Source (F7)" }).primary().on_click(cx.listener(|this, _, window, cx| this.toggle(window, cx))))
                .child(Button::new("projection-reset").label("Reset fixture + history").on_click(cx.listener(|this, _, window, cx| this.reset(false, window, cx))))
                .child(Button::new("projection-large").label("Reset to 64 KiB + 1").on_click(cx.listener(|this, _, window, cx| this.reset(true, window, cx))))
                .child(Button::new("projection-readonly").label(if self.readonly { "Readonly: on" } else { "Readonly: off" }).on_click(cx.listener(|this, _, window, cx| {
                    this.readonly = !this.readonly;
                    this.editor.update(cx, |editor, cx| editor.set_readonly(this.readonly, cx));
                    this.refocus(window, cx); this.record("readonly_changed", cx); cx.notify();
                })))
                .child(Button::new("projection-delay").label(if self.delayed { "Parse delay: 500 ms" } else { "Parse delay: off" }).on_click(cx.listener(|this, _, window, cx| {
                    this.delayed = !this.delayed; this.refocus(window, cx); cx.notify();
                })))
                .child(Button::new("projection-stale").label("Inject first result").on_click(cx.listener(|this, _, window, cx| {
                    if let Some(provider) = this.first_result.clone() { let accepted = this.publish(provider, cx); println!("{}",json!({"event":"injected_first_result","accepted":accepted})); }
                    this.refocus(window, cx); cx.notify();
                })))
                .child(Button::new("projection-snapshot").label("Record snapshot (F8)").on_click(cx.listener(|this, _, window, cx| { this.record("button_snapshot", cx); this.refocus(window, cx); }))))
            .child(div().text_sm().child(summary))
            .child(div().text_sm().child(self.status.clone()))
            .child(div().text_sm().child("One editable buffer. F7 changes presentation; F8 records exact source. Reset buttons intentionally clear history. No files or backend are written."))
            .child(div().flex_1().min_h_0().child(Editor::new(&self.editor).readonly(self.readonly).font_family(BODY_FONT).text_size(px(16.)).h(relative(1.0))))
            .when(self.legacy_marker, |view| view.child(
                // Paint last without drawing pixels, taking layout space, or receiving input.
                canvas(|_, _, _| (), move |_, (), _, cx| {
                    let painted_ns = monotonic_ns();
                    let current = paint_editor.read(cx);
                    let current_stamp = current.source_stamp();
                    let current_epoch = current.presentation_epoch();
                    println!("{}", json!({"event":"timing_cpu_paint_marker",
                        "monotonic_ns":painted_ns,"document":stamp.document,
                        "generation":stamp.generation,"presentation_epoch":presentation_epoch,
                        "current_document":current_stamp.document,"current_generation":current_stamp.generation,
                        "current_presentation_epoch":current_epoch,
                        "coherent":current_stamp == stamp && current_epoch == presentation_epoch,
                        "live":live,"stage":"trailing_cpu_paint_not_presentation"}));
                }).absolute().top_0().left_0().size(px(1.0))
            ))
    }
}

// Deterministic native race testing only; normal runs have no delivery delay.
struct FixtureClipboardDelay {
    inner: Arc<dyn ExactClipboardProvider>,
    delay: Duration,
}
impl ExactClipboardProvider for FixtureClipboardDelay {
    fn read_text(
        &self,
        request: ClipboardReadRequest,
        cx: &App,
    ) -> Task<Result<Option<String>, ClipboardReadError>> {
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        let task = self.inner.read_text(request.clone(), cx);
        let delay = self.delay;
        cx.background_executor().spawn(async move {
            let result = task.await?;
            let release = std::time::Instant::now() + delay;
            loop {
                if request.is_cancelled() {
                    return Err(ClipboardReadError::Cancelled);
                }
                let now = std::time::Instant::now();
                if now >= deadline {
                    return Err(ClipboardReadError::Timeout);
                }
                if now >= release {
                    return Ok(result);
                }
                std::thread::sleep((release - now).min(Duration::from_millis(10)));
            }
        })
    }
}

fn main() {
    let clipboard: Arc<dyn ExactClipboardProvider> =
        Arc::new(exact_wayland_clipboard::WaylandClipboard::new(
            exact_wayland_clipboard::bind_compositor(),
            std::env::var("OKILUM_NATIVE216_SEAT").ok(),
        ));
    let delay_ms: u64 = std::env::var("OKILUM_NATIVE216_CLIPBOARD_DELAY_MS")
        .ok()
        .map(|value| value.parse().expect("clipboard delay must be milliseconds"))
        .unwrap_or(0);
    assert!(
        delay_ms <= 1500,
        "clipboard test delay must be at most 1500 ms"
    );
    let clipboard: Arc<dyn ExactClipboardProvider> = if delay_ms == 0 {
        clipboard
    } else {
        println!(
            "{}",
            json!({"event":"exact_clipboard_test_delay", "milliseconds":delay_ms})
        );
        Arc::new(FixtureClipboardDelay {
            inner: clipboard,
            delay: Duration::from_millis(delay_ms),
        })
    };
    assert_eq!(
        format!("{:x}", Sha256::digest(FIXTURE.as_bytes())),
        FIXTURE_HASH.split_whitespace().next().unwrap()
    );
    gpui_platform::application()
        .with_assets(gpui_kit_assets::Assets)
        .run(move |cx| {
            gpui_component::init(cx);
            let mode = if std::env::var("OKILUM_MARKER_DARK").as_deref() == Ok("1") {
                gpui_component::ThemeMode::Dark
            } else {
                gpui_component::ThemeMode::Light
            };
            gpui_component::Theme::change(mode, None, cx);
            cx.text_system()
                .add_fonts(vec![
                    Cow::Borrowed(include_bytes!("../assets/brand/fonts/noto-sans-400.ttf")),
                    Cow::Borrowed(include_bytes!("../assets/brand/fonts/noto-sans-700.ttf")),
                    Cow::Borrowed(include_bytes!(
                        "../assets/brand/fonts/cascadia-code-400.ttf"
                    )),
                    Cow::Borrowed(include_bytes!(
                        "../assets/brand/fonts/cascadia-code-700.ttf"
                    )),
                ])
                .expect("fixture font registration failed");
            cx.bind_keys([
                KeyBinding::new("f7", ToggleMode, Some("ProjectionFixture")),
                KeyBinding::new("f8", SnapshotEvidence, Some("ProjectionFixture")),
                KeyBinding::new("f9", DumpAttribution, Some("ProjectionFixture")),
                KeyBinding::new("f6", ResetFixture, Some("ProjectionFixture")),
            ]);
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
            let bounds = Bounds::centered(None, size(px(1100.), px(850.)), cx);
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    app_id: Some("okilum-native-projection216".into()),
                    ..Default::default()
                },
                move |window, cx| {
                    window.set_window_title("Okilum marker paint #784 — synthetic fixture");
                    let fixture = cx.new(|cx| Fixture::new(window, cx, clipboard.clone()));
                    cx.new(|cx| Root::new(fixture, window, cx))
                },
            )
            .expect("fixture window failed");
            cx.activate(true);
        });
}

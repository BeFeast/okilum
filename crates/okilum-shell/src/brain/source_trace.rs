//! Explicit, untimed evidence dumps for the managed native acceptance surface.
//! Source mutation and actual component paint spans come from the shared recorder.
use super::*;
use gpui_component::input::attribution;
use sha2::{Digest, Sha256};

actions!(brain, [DumpManagedSourceTrace]);

struct TraceSettings {
    enabled: bool,
}
impl Global for TraceSettings {}

pub(crate) fn install(cx: &mut App) {
    let requested = std::env::var("OKILUM_MANAGED220_TRACE").as_deref() == Ok("1");
    install_requested(requested, cx);
}

fn install_requested(requested: bool, cx: &mut App) {
    let enabled = requested && cfg!(all(target_os = "linux", target_pointer_width = "64"));
    cx.set_global(TraceSettings { enabled });
    attribution::init(enabled);
    if requested {
        println!(
            "{}",
            json!({"event":"managed_trace_configuration", "enabled":enabled,
                "capacity":attribution::CAPACITY, "schema":"managed-source-trace/v1"})
        );
    }
    if enabled {
        cx.bind_keys([KeyBinding::new(
            "f8",
            DumpManagedSourceTrace,
            Some("OkilumWorkspace"),
        )]);
        clock_sample();
    }
}

fn enabled(cx: &App) -> bool {
    cx.try_global::<TraceSettings>()
        .is_some_and(|settings| settings.enabled)
}

fn clock_sample() {
    let before = attribution::now();
    let realtime = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock after epoch")
        .as_nanos();
    let after = attribution::now();
    println!(
        "{}",
        json!({"event":"managed_trace_clock_sample", "monotonic_before_ns":before,
            "realtime_ns":realtime, "monotonic_after_ns":after})
    );
}

impl BrainView {
    fn source_trace_snapshot(&self, cx: &App) -> Option<Value> {
        if !enabled(cx) {
            return None;
        }
        let state = self.source.managed()?.read(cx);
        let source = self.source_snapshot.as_ref()?;
        let value = state.value();
        let stamp = state.source_stamp();
        let selected = state.selected_range();
        let (in_flight, queued) = self.source_projection.pending();
        Some(
            json!({"event":"managed_source_snapshot", "schema":"managed-source-trace/v1",
            "entity":self.source.entity_id().as_u64(), "document":stamp.document,
            "generation":stamp.generation, "presentation_epoch":state.presentation_epoch(),
            "workspace":self.expected_workspace, "path":source["path"],
            "source_bytes":value.len(), "source_sha256":format!("{:x}",Sha256::digest(value.as_bytes())),
            "selection_utf8":[selected.start,selected.end], "cursor_utf8":state.cursor(),
            "live":self.source_projection.live, "readonly":self.source_readonly(),
            "classification_in_flight":in_flight, "classification_queued":queued}),
        )
    }

    pub(super) fn dump_source_trace(&self, cx: &App) {
        let Some(snapshot) = self.source_trace_snapshot(cx) else {
            return;
        };
        // Explicit action only: formatting, hashing, stdout and buffer draining
        // are outside the sparse-input measurement interval.
        clock_sample();
        println!("{snapshot}");
        println!(
            "{}",
            json!({"event":"attribution_dump", "trace":attribution::dump()})
        );
        clock_sample();
        println!(
            "{}",
            json!({"event":"managed_trace_dump_complete", "monotonic_ns":attribution::now()})
        );
    }
}

#[cfg(all(test, target_os = "linux", target_pointer_width = "64"))]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    use gpui::EntityInputHandler;

    #[gpui::test]
    fn explicit_dump_preserves_managed_source_selection_and_undo_redo(cx: &mut TestAppContext) {
        let dir = std::env::temp_dir().join(format!("okilum-trace-ui-{}", uuid()));
        let workspace = json!({"brain_id":uuid(),"root":"/isolated/trace-ui","records_dir":"records","managed":true});
        let store = editor_recovery::EditorRecovery::at(dir.clone(), &workspace).unwrap();
        let original = "# Original\r\n";
        let base = json!({"schema":SCHEMA,"brain_id":workspace["brain_id"],"path":"source.md","revision":format!("sha256:{:x}",Sha256::digest(original.as_bytes())),"content_base64":STANDARD.encode(original),"media_type":"text/markdown"});
        cx.update(|cx| {
            gpui_component::init(cx);
            install_requested(false, cx);
        });
        let (view, cx) = cx.add_window_view(|window, cx| {
            BrainView::new_managed_test(&workspace, &store, window, cx)
        });
        cx.run_until_parked();
        let entity = view.update_in(cx, |v, window, cx| {
            v.load_source(base, window, cx);
            v.surface = Surface::Source;
            v.busy = false;
            v.sync_source_policy(cx);
            assert!(v.source_trace_snapshot(cx).is_none());
            v.dump_source_trace(cx); // Disabled action returns before clocks/output/dump.
            install_requested(true, cx);
            window.activate_window();
            v.source.focus(window, cx);
            v.source.managed().unwrap().update(cx, |s, cx| {
                s.replace_text_in_range(Some(2..2), "Edited ", window, cx);
            });
            v.source.entity_id()
        });
        cx.run_until_parked();
        let before = view.update_in(cx, |v, _, cx| v.source_trace_snapshot(cx));
        assert!(before.is_some());
        drop(attribution::Span::new(
            "managed_dump_positive_control",
            attribution::Identity::default(),
        ));
        cx.simulate_keystrokes("f8");
        let after_dump = attribution::dump();
        assert!(
            after_dump["events"]
                .as_array()
                .unwrap()
                .iter()
                .all(|event| event["name"] != "managed_dump_positive_control"),
            "F8 must drain the explicit positive control"
        );
        view.update_in(cx, |v, _, cx| {
            assert_eq!(v.source.entity_id(), entity);
            assert_eq!(v.source_trace_snapshot(cx), before);
        });
        #[cfg(not(target_os = "macos"))]
        cx.simulate_keystrokes("ctrl-z");
        #[cfg(target_os = "macos")]
        cx.simulate_keystrokes("cmd-z");
        view.update_in(cx, |v, _, cx| {
            assert_eq!(v.source.value(cx).as_ref(), original)
        });
        cx.simulate_keystrokes("f8");
        #[cfg(not(target_os = "macos"))]
        cx.simulate_keystrokes("ctrl-y");
        #[cfg(target_os = "macos")]
        cx.simulate_keystrokes("cmd-shift-z");
        view.update_in(cx, |v, _, cx| {
            assert_eq!(v.source.entity_id(), entity);
            assert_eq!(v.source.value(cx).as_ref(), "# Edited Original\r\n");
            install_requested(false, cx);
            assert!(v.source_trace_snapshot(cx).is_none());
        });
        cx.run_until_parked();
        let _ = std::fs::remove_dir_all(dir);
    }
}

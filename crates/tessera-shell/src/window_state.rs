//! Durable native window geometry, separate from vaults and rebuildable caches.
use gpui::{
    point, px, size, App, Bounds, Entity, Global, Pixels, Window, WindowBounds, WindowOptions,
};
use gpui_component::Root;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Frame {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    display: Option<String>,
    fullscreen: bool,
    maximized: bool,
}
impl Frame {
    fn valid(&self) -> bool {
        [self.x, self.y, self.width, self.height]
            .iter()
            .all(|x| x.is_finite())
            && self.width > 0.
            && self.height > 0.
    }
    fn fitted(&self, visible: Bounds<Pixels>) -> Bounds<Pixels> {
        let width = self.width.max(600.).min(f32::from(visible.size.width));
        let height = self.height.max(400.).min(f32::from(visible.size.height));
        Bounds::new(
            point(
                px(self.x.clamp(
                    f32::from(visible.left()),
                    f32::from(visible.right()) - width,
                )),
                px(self.y.clamp(
                    f32::from(visible.top()),
                    f32::from(visible.bottom()) - height,
                )),
            ),
            size(px(width), px(height)),
        )
    }
}
#[derive(Default, Serialize, Deserialize)]
struct Saved {
    frames: BTreeMap<String, Frame>,
}
struct State {
    path: PathBuf,
    saved: Saved,
    next: BTreeMap<String, usize>,
    pending: Option<gpui::Task<()>>,
    dirty: bool,
}
impl Global for State {}

pub(crate) fn install(directory: PathBuf, cx: &mut App) {
    let path = directory.join("window-frames.json");
    let saved = std::fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    cx.set_global(State {
        path,
        saved,
        next: BTreeMap::new(),
        pending: None,
        dirty: false,
    });
    cx.on_app_quit(|cx| {
        flush(cx);
        async {}
    })
    .detach();
}

pub(crate) fn reader_key(opts: &super::Opts) -> String {
    let path = opts.vault.clone().or_else(|| {
        opts.open_path.as_ref().map(|path| {
            let path = path.canonicalize().unwrap_or_else(|_| path.clone());
            if path.is_dir() {
                path
            } else if let Some(root) = opts
                .reusable_roots
                .iter()
                .find(|root| path.starts_with(root))
            {
                root.clone()
            } else {
                let parent = path.parent().unwrap_or(&path);
                parent
                    .ancestors()
                    .find(|root| root.join(".obsidian").is_dir())
                    .unwrap_or(parent)
                    .to_path_buf()
            }
        })
    });
    path.map(|p| {
        let p = p.canonicalize().unwrap_or(p);
        format!("reader:{}", p.to_string_lossy())
    })
    .unwrap_or_else(|| "reader".into())
}

/// Duplicate windows of one root use separate creation-order slots.
pub(crate) fn prepare(
    mut options: WindowOptions,
    key: &str,
    cx: &mut App,
) -> (WindowOptions, Option<String>) {
    let Some(state) = cx.try_global::<State>() else {
        return (options, None);
    };
    let slot = *state.next.get(key).unwrap_or(&0);
    let key_with_slot = format!("{key}#{slot}");
    let saved = super::reader_ui_state::window_frame(&key_with_slot, cx)
        .or_else(|| state.saved.frames.get(&key_with_slot).cloned())
        .filter(Frame::valid);
    cx.global_mut::<State>().next.insert(key.into(), slot + 1);
    if let Some(frame) = saved {
        let displays = cx.displays();
        let original = displays.iter().find(|d| {
            frame
                .display
                .as_ref()
                .is_some_and(|saved| d.uuid().ok().is_some_and(|u| u.to_string() == *saved))
        });
        // GPUI macOS uses screen-relative coordinates for BOTH window bounds
        // and visible_bounds (including the dock/menu inset). Keep that native
        // coordinate space paired with its display UUID; do not add an origin.
        // A missing display or an off-screen frame returns to the primary display.
        let target = original
            .filter(|d| {
                let b = d.visible_bounds();
                frame.x >= f32::from(b.left())
                    && frame.y >= f32::from(b.top())
                    && frame.x + frame.width <= f32::from(b.right())
                    && frame.y + frame.height <= f32::from(b.bottom())
            })
            .cloned()
            .or_else(|| cx.primary_display());
        if let Some(display) = target {
            let bounds = frame.fitted(display.visible_bounds());
            options.display_id = Some(display.id());
            options.window_bounds = Some(if frame.fullscreen {
                WindowBounds::Fullscreen(bounds)
            } else if frame.maximized {
                WindowBounds::Maximized(bounds)
            } else {
                WindowBounds::Windowed(bounds)
            });
        }
    }
    (options, Some(key_with_slot))
}

fn record(key: &str, window: &Window, cx: &mut App) {
    let bounds = window.window_bounds();
    let frame = bounds.get_bounds();
    let saved = Frame {
        x: frame.origin.x.into(),
        y: frame.origin.y.into(),
        width: frame.size.width.into(),
        height: frame.size.height.into(),
        display: window
            .display(cx)
            .and_then(|d| d.uuid().ok())
            .map(|u| u.to_string()),
        fullscreen: matches!(bounds, WindowBounds::Fullscreen(_)),
        maximized: matches!(bounds, WindowBounds::Maximized(_)),
    };
    if !saved.valid() {
        return;
    }
    if super::reader_ui_state::record_frame(key, saved.clone(), window.is_window_active(), cx) {
        return;
    }
    let state = cx.global_mut::<State>();
    state.saved.frames.insert(key.into(), saved);
    state.dirty = true;
    // Dropping the previous task cancels its timer. Only the latest frame is
    // written after the user pauses, while quit always flushes immediately.
    state.pending.take();
    let task = cx.spawn(async move |cx| {
        cx.background_executor()
            .timer(std::time::Duration::from_millis(250))
            .await;
        cx.update(flush);
    });
    cx.global_mut::<State>().pending = Some(task);
}

fn flush(cx: &mut App) {
    if super::reader_ui_state::installed(cx) {
        super::reader_ui_state::flush(cx);
        return;
    }
    let Some(state) = cx.try_global::<State>() else {
        return;
    };
    if !state.dirty {
        return;
    }
    let state = cx.global_mut::<State>();
    let result = (|| -> anyhow::Result<()> {
        let parent = state
            .path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("No state directory"))?;
        std::fs::create_dir_all(parent)?;
        let temp = parent.join(format!(".window-frames-{}.json", uuid::Uuid::new_v4()));
        std::fs::write(&temp, serde_json::to_vec(&state.saved)?)?;
        std::fs::rename(&temp, &state.path).inspect_err(|_| {
            let _ = std::fs::remove_file(&temp);
        })?;
        state.dirty = false;
        Ok(())
    })();
    if let Err(error) = result {
        eprintln!("Cannot save window geometry: {error:#}");
    }
}

pub(crate) fn track(root: &Entity<Root>, key: Option<String>, window: &mut Window, cx: &mut App) {
    let Some(key) = key else {
        return;
    };
    record(&key, window, cx);
    root.update(cx, |_, cx| {
        let active_key = key.clone();
        cx.observe_window_activation(window, move |_, window, cx| {
            if window.is_window_active() {
                record(&active_key, window, cx);
            }
        })
        .detach();
        cx.observe_window_bounds(window, move |_, window, cx| record(&key, window, cx))
            .detach();
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[gpui::test]
    fn restores_saved_display_slots_and_flushes_latest_state(cx: &mut gpui::TestAppContext) {
        let directory =
            std::env::temp_dir().join(format!("tessera-window-state-{}", uuid::Uuid::new_v4()));
        cx.update(|cx| {
            install(directory.clone(), cx);
            let display = cx.primary_display().expect("test display positive control");
            let visible = display.visible_bounds();
            let frame = Frame {
                x: visible.origin.x.into(),
                y: visible.origin.y.into(),
                width: 600.,
                height: 400.,
                display: Some(display.uuid().unwrap().to_string()),
                fullscreen: true,
                maximized: false,
            };
            let state = cx.global_mut::<State>();
            state.saved.frames.insert("root#0".into(), frame.clone());
            state.saved.frames.insert(
                "root#1".into(),
                Frame {
                    x: -10000.,
                    display: Some("disconnected".into()),
                    fullscreen: false,
                    ..frame
                },
            );
            state.dirty = true;
            let (first, key) = prepare(WindowOptions::default(), "root", cx);
            assert_eq!(key.as_deref(), Some("root#0"));
            assert_eq!(first.display_id, Some(display.id()));
            assert!(matches!(
                first.window_bounds,
                Some(WindowBounds::Fullscreen(_))
            ));
            let (second, key) = prepare(WindowOptions::default(), "root", cx);
            assert_eq!(key.as_deref(), Some("root#1"));
            assert_eq!(second.display_id, Some(display.id()));
            assert_eq!(
                second.window_bounds.unwrap().get_bounds().left(),
                visible.left()
            );
            assert!(!directory.join("window-frames.json").exists());
            flush(cx);
            let saved: Saved = serde_json::from_slice(
                &std::fs::read(directory.join("window-frames.json")).unwrap(),
            )
            .unwrap();
            assert_eq!(saved.frames.len(), 2);
            assert!(saved.frames["root#0"].fullscreen);
            assert!(!cx.global::<State>().dirty);
        });
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn frame_clamps_to_visible_primary_and_respects_minimum() {
        let screen = Bounds::new(point(px(10.), px(30.)), size(px(1200.), px(800.)));
        let f = Frame {
            x: -2000.,
            y: 1200.,
            width: 1800.,
            height: 20.,
            display: None,
            fullscreen: true,
            maximized: false,
        };
        let fitted = f.fitted(screen);
        assert_eq!(
            fitted,
            Bounds::new(point(px(10.), px(430.)), size(px(1200.), px(400.)))
        );
        let mut saved = Saved::default();
        saved.frames.insert("root#0".into(), f.clone());
        saved
            .frames
            .insert("root#1".into(), Frame { width: 700., ..f });
        let restored: Saved = serde_json::from_slice(&serde_json::to_vec(&saved).unwrap()).unwrap();
        assert!(restored.frames["root#0"].fullscreen);
        assert_eq!(restored.frames["root#1"].width, 700.);
    }
}

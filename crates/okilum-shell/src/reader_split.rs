//! A list above a detail pane with a drag handle between them (#1156). The
//! pane height follows the handle, is remembered per kind of view, and a
//! double-click on the handle collapses the pane to its header or restores it.
use super::*;

/// Height of the handle's hit area; a 1 px line is drawn in its middle.
pub(super) const HANDLE_HEIGHT: f32 = 7.;

#[derive(Clone)]
struct SplitDrag(&'static str);

impl Render for SplitDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

pub(super) struct Split {
    /// UI state key and debug selector prefix.
    key: &'static str,
    default: f32,
    /// The pane never gets smaller than its header.
    min_pane: f32,
    /// The list keeps room for a few rows.
    min_list: f32,
    /// Dragged in this view; otherwise the remembered or default height.
    height: Option<f32>,
    collapsed: bool,
}

impl Split {
    pub(super) fn new(key: &'static str, default: f32, min_pane: f32, min_list: f32) -> Self {
        Self {
            key,
            default,
            min_pane,
            min_list,
            height: None,
            collapsed: false,
        }
    }

    /// The pane height to lay out; the flex layout still shrinks it down to
    /// `min_pane` when the window is too small for the list's minimum.
    pub(super) fn height(&self, cx: &App) -> f32 {
        if self.collapsed {
            return self.min_pane;
        }
        self.height
            .or_else(|| reader_ui_state::split_size(self.key, cx))
            .unwrap_or(self.default)
            .max(self.min_pane)
    }

    /// `requested` pane height inside a split `available` px tall.
    pub(super) fn drag_to(&mut self, requested: f32, available: f32) {
        let max = (available - HANDLE_HEIGHT - self.min_list).max(self.min_pane);
        self.height = Some(requested.clamp(self.min_pane, max));
        self.collapsed = false;
    }

    pub(super) fn toggle_collapsed(&mut self) {
        self.collapsed = !self.collapsed;
    }

    fn save(&self, cx: &mut App) {
        if let Some(height) = self.height {
            reader_ui_state::set_split_size(self.key, height, cx);
        }
    }
}

/// `list` above `pane`, split by a handle. `project` finds the split in the
/// owning view so the handle's listeners can update it.
pub(super) fn vertical<V: 'static>(
    split: &Split,
    project: fn(&mut V) -> &mut Split,
    list: impl IntoElement,
    pane: impl IntoElement,
    cx: &mut Context<V>,
) -> impl IntoElement {
    let key = split.key;
    let line = brand::palette(cx).border_subtle;
    let handle = div()
        .id(SharedString::from(format!("{key}-handle")))
        .debug_selector(move || format!("{key}-handle"))
        .flex_none()
        .w_full()
        .h(px(HANDLE_HEIGHT))
        .flex()
        .items_center()
        .cursor_row_resize()
        .child(div().w_full().h(px(1.)).bg(line))
        .on_drag(SplitDrag(key), |drag, _, _, cx| cx.new(|_| drag.clone()))
        .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
            if event.click_count() == 2 {
                project(this).toggle_collapsed();
                cx.notify();
            }
        }));
    v_flex()
        .id(SharedString::from(format!("{key}-split")))
        .flex_1()
        .min_h_0()
        .w_full()
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_h(px(split.min_list))
                .w_full()
                .child(list),
        )
        .child(handle)
        .child(
            div()
                .debug_selector(move || format!("{key}-pane"))
                .flex()
                .flex_col()
                .flex_shrink(1.)
                .h(px(split.height(cx)))
                .min_h(px(split.min_pane))
                .w_full()
                .overflow_hidden()
                .child(pane),
        )
        .on_drag_move(
            cx.listener(move |this, event: &DragMoveEvent<SplitDrag>, _, cx| {
                if event.drag(cx).0 != key {
                    return;
                }
                let requested = event.bounds.bottom() - event.event.position.y;
                project(this).drag_to(
                    f32::from(requested) - HANDLE_HEIGHT / 2.,
                    f32::from(event.bounds.size.height),
                );
                cx.notify();
            }),
        )
        .on_drop(cx.listener(move |this, drag: &SplitDrag, _, cx| {
            if drag.0 == key {
                project(this).save(cx);
            }
        }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[test]
    fn dragging_keeps_the_list_rows_and_the_pane_header() {
        let mut split = Split::new("test", 240., 36., 120.);
        split.drag_to(540., 900.);
        assert_eq!(split.height, Some(540.));
        // Not past the list's minimum, not below the header.
        split.drag_to(880., 900.);
        assert_eq!(split.height, Some(900. - HANDLE_HEIGHT - 120.));
        split.drag_to(5., 900.);
        assert_eq!(split.height, Some(36.));
        // A window smaller than both minimums still keeps the header.
        split.drag_to(100., 100.);
        assert_eq!(split.height, Some(36.));
        // Dragging restores a collapsed pane.
        split.toggle_collapsed();
        assert!(split.collapsed);
        split.drag_to(300., 900.);
        assert!(!split.collapsed);
    }
}

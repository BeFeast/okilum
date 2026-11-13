//! Pinned files row above the document (#2789). A pinned file stays one
//! click away while other notes open, like a pinned tab.
//!
//! Pins are the vault's existing Pinned list (`reader_sidebar::State::pinned`,
//! the store behind the title-bar Pinned menu, #1517), so a file pinned here,
//! from the tree or from the Pinned menu is one pin. It is saved with the
//! per-vault sidebar state and survives a relaunch. The row lists pinned
//! files; pinned folders stay in the Pinned menu only.
//!
//! Opening a pin is ordinary navigation (`open_note`): unsaved edits follow
//! today's save and draft rules, and pinning itself never saves anything.
//! A move or rename inside Okilum carries its pin along
//! (`remap_move_sidebar`). A pinned file that a complete inventory no longer
//! has was moved or deleted elsewhere: it stays in the row, marked missing,
//! until the user removes it.
use super::*;
use gpui_component::menu::{ContextMenuExt as _, PopupMenuItem};

/// Height of the row; it appears only while a pinned file exists.
pub(crate) const PIN_ROW_HEIGHT: f32 = 34.;
/// A long name is cut so several pins share the row.
const PIN_MAX_WIDTH: f32 = 220.;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PinnedFile {
    pub path: String,
    pub label: String,
    /// Moved or deleted outside Okilum: not in the complete inventory.
    pub missing: bool,
}

/// The menu label for `rel`'s pin.
pub(crate) fn pin_label(pinned: bool) -> &'static str {
    if pinned {
        "Unpin"
    } else {
        "Pin"
    }
}

/// «Pin» or «Unpin» for `rel`, shared by every file menu. None where pins do
/// not exist: a single file opened without its vault, or no file.
pub(crate) fn pin_menu_item(
    reader: &WeakEntity<Reader>,
    rel: &str,
    cx: &App,
) -> Option<PopupMenuItem> {
    let target = reader.upgrade()?;
    let this = target.read(cx);
    if !this.pins_available() || rel.is_empty() {
        return None;
    }
    let pinned = this.sidebar.is_pinned(rel);
    let (root, generation) = (this.vault_root.clone(), this.watcher_generation);
    let reader = reader.clone();
    let rel = rel.to_owned();
    Some(
        PopupMenuItem::new(pin_label(pinned))
            .icon(Icon::default().path(brand::READER_PIN_ICON))
            .on_click(move |_, _, cx| {
                let _ = reader.update(cx, |this, cx| {
                    // A menu built for another vault or inventory acts on nothing.
                    if this.vault_root == root && this.watcher_generation == generation {
                        this.toggle_pin(&rel, cx);
                    }
                });
            }),
    )
}

impl Reader {
    /// Pins belong to a vault; a single file opened on its own has none.
    pub(crate) fn pins_available(&self) -> bool {
        !self.single_file && !self.vault_root.as_os_str().is_empty()
    }

    /// The pin's file is not in a complete inventory: it was moved or deleted
    /// outside Okilum. An incomplete inventory proves nothing.
    pub(crate) fn pin_missing(&self, path: &str) -> bool {
        self.vault.inventory_complete && !self.tree.contains(path)
    }

    /// The row's entries, in pin order. Existing folders are left to the
    /// Pinned menu; a missing pin is listed whatever it was.
    pub(crate) fn pinned_files(&self) -> Vec<PinnedFile> {
        if !self.pins_available() {
            return Vec::new();
        }
        self.sidebar
            .pinned
            .iter()
            .filter(|path| !self.tree.is_directory(path))
            .map(|path| PinnedFile {
                path: path.clone(),
                label: self.note_label(path),
                missing: self.pin_missing(path),
            })
            .collect()
    }

    pub(crate) fn unpin(&mut self, path: &str, cx: &mut Context<Self>) {
        if self.sidebar.is_pinned(path) {
            self.toggle_pin(path, cx);
        }
    }

    /// Opens a pinned file the way any link does. The open file is left as
    /// it is; a missing one says so instead of failing to load.
    pub(crate) fn open_pinned(&mut self, path: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.pin_missing(path) {
            reader_toast::error(
                format!("{path} was moved or deleted outside Okilum. Remove the pin with ×."),
                window,
                cx,
            );
            return;
        }
        if path == self.selected_file() {
            return;
        }
        self.open_note(path, None, window, cx);
    }

    /// The row of pinned files, or nothing when no file is pinned.
    #[inline(never)]
    pub(crate) fn render_pin_row(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let pins = self.pinned_files();
        if pins.is_empty() {
            return None;
        }
        let p = brand::palette(cx);
        let reader_palette = brand::reader_palette(cx);
        let current = self.selected_file().to_owned();
        let entity = cx.entity().downgrade();
        let mut list = h_flex()
            .id("reader-pin-row-scroll")
            .flex_1()
            .min_w_0()
            .h_full()
            .gap_1()
            .overflow_x_scroll();
        for (index, pin) in pins.into_iter().enumerate() {
            let is_current = pin.path == current;
            let group = SharedString::from(format!("reader-pin-group-{index}"));
            let selector = format!("reader-pin-{}", pin.path);
            let close_selector = format!("reader-unpin-{}", pin.path);
            let (open_path, close_path, menu_path) =
                (pin.path.clone(), pin.path.clone(), pin.path.clone());
            let tooltip: SharedString = if pin.missing {
                format!("{} — not found: moved or deleted outside Okilum", pin.path).into()
            } else {
                pin.path.clone().into()
            };
            let menu_reader = entity.clone();
            let missing = pin.missing;
            let close = div()
                .id(SharedString::from(format!("reader-unpin-{index}")))
                .debug_selector(move || close_selector.clone())
                .flex_none()
                .rounded(px(4.))
                .opacity(if is_current || missing { 1. } else { 0. })
                .group_hover(group.clone(), |s| s.opacity(1.))
                .hover(move |s| s.bg(reader_palette.hover))
                .child(
                    Icon::new(IconName::Close)
                        .xsmall()
                        .text_color(reader_palette.text_faint),
                )
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.unpin(&close_path, cx);
                }))
                .tooltip(move |window, cx| {
                    gpui_component::tooltip::Tooltip::new(if missing {
                        "Remove pin"
                    } else {
                        "Unpin"
                    })
                    .build(window, cx)
                });
            let label = div()
                .min_w_0()
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .child(pin.label)
                .when(missing, |label| label.line_through());
            list = list.child(
                h_flex()
                    .id(SharedString::from(format!("reader-pin-{index}")))
                    .debug_selector(move || selector.clone())
                    .group(group)
                    .flex_none()
                    .max_w(px(PIN_MAX_WIDTH))
                    .h(px(26.))
                    .px_2()
                    .gap_1()
                    .rounded(px(6.))
                    .text_size(px(13.))
                    .cursor_pointer()
                    .text_color(if missing {
                        p.text_muted
                    } else if is_current {
                        p.text
                    } else {
                        p.text_muted
                    })
                    .when(is_current, |chip| chip.bg(p.selected))
                    .hover(move |chip| chip.bg(reader_palette.hover).text_color(p.text))
                    .child(
                        Icon::default()
                            .path(brand::READER_PIN_ICON)
                            .xsmall()
                            .text_color(if is_current {
                                p.accent
                            } else {
                                reader_palette.text_faint
                            }),
                    )
                    .child(label)
                    .child(close)
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_pinned(&open_path, window, cx)
                    }))
                    .tooltip(move |window, cx| {
                        gpui_component::tooltip::Tooltip::new(tooltip.clone()).build(window, cx)
                    })
                    .context_menu(move |menu, window, cx| {
                        pin_row_menu(menu, &menu_reader, &menu_path, missing, window, cx)
                    }),
            );
        }
        Some(
            h_flex()
                .id("reader-pin-row")
                .debug_selector(|| "reader-pin-row".into())
                .flex_none()
                .w_full()
                .min_w_0()
                .h(px(PIN_ROW_HEIGHT))
                .px_4()
                .gap_1()
                .border_b_1()
                .border_color(p.border_subtle)
                .child(list)
                .into_any_element(),
        )
    }
}

/// A pin's own menu: Unpin, then the file's actions while it exists.
fn pin_row_menu(
    menu: gpui_component::menu::PopupMenu,
    reader: &WeakEntity<Reader>,
    path: &str,
    missing: bool,
    window: &mut Window,
    cx: &mut Context<gpui_component::menu::PopupMenu>,
) -> gpui_component::menu::PopupMenu {
    let unpin = {
        let reader = reader.clone();
        let path = path.to_owned();
        PopupMenuItem::new(if missing { "Remove pin" } else { "Unpin" }).on_click(
            move |_, _, cx| {
                let _ = reader.update(cx, |this, cx| this.unpin(&path, cx));
            },
        )
    };
    let menu = menu.item(unpin);
    let Some(target) = reader.upgrade().filter(|_| !missing) else {
        return menu;
    };
    let root = target.read(cx).vault_root.clone();
    reader_files::menu(menu.separator(), root, path.to_owned(), window, cx)
}

#[cfg(test)]
mod tests;

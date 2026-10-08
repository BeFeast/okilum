//! Application menu for builds without the managed workspace.
use gpui::{App, KeyBinding, Menu, MenuItem};

gpui::actions!(tessera, [Quit]);

pub(crate) fn install(cx: &mut App) {
    cx.on_action(|_: &Quit, cx| {
        // Defer until the dispatching Reader releases its entity borrow.
        cx.defer(|cx| {
            if crate::reader_editor::save_all(cx) {
                cx.quit();
            }
        });
    });
    cx.bind_keys([KeyBinding::new("secondary-q", Quit, None)]);
    set_menus(cx);
}
pub(crate) fn set_menus(cx: &mut App) {
    cx.set_menus(vec![
        Menu {
            name: "Tessera".into(),
            items: crate::updater::menu_items(MenuItem::action("Quit Tessera", Quit)),
            disabled: false,
        },
        crate::reader_open::file_menu(),
    ]);
}

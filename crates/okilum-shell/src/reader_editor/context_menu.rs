//! Editing-only context actions. Read-mode menus remain independent (#2025).
use super::*;
use gpui_component::native_menu::NativeMenu;

fn formatting_menu(markdown: bool, editable: bool) -> Option<Menu> {
    // Use the app menu's actions unchanged: formatting, selection and Undo have
    // one implementation for keyboard, app menu and right-click.
    (markdown && editable).then(format::format_menu)
}

pub(super) fn editor_menu(
    menu: NativeMenu,
    facts: &reader_reminder::MenuFacts,
    markdown: bool,
    cx: &App,
) -> NativeMenu {
    let menu = reader_reminder::editor_menu(menu, facts, cx);
    match formatting_menu(markdown, facts.editable()) {
        Some(format) => menu.separator().submenu("Format", NativeMenu::from(format)),
        None => menu,
    }
}

#[cfg(test)]
mod tests;

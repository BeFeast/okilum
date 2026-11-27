//! App-registered callbacks for brain modules that need app-level services
//! (menu bar contributions, quit/save guards, window-close diagnostics,
//! Settings → Features navigation, editor text context menu, and SecondBrain
//! module gating) without depending on `Reader` (#3236, epic #3241).
use gpui::{AnyWindowHandle, App, Global, Menu, MenuItem, Window};
use gpui_component::native_menu::NativeMenu;
use okilum_t3::NetworkAccess;

pub type BuildMenusFn = fn(&mut App) -> Vec<Menu>;
pub type ConfirmDeletedForQuitFn = fn(fn(&mut App), &mut App) -> bool;
pub type SaveAllForQuitFn = fn(&mut App) -> bool;
pub type SaveWindowFn = fn(AnyWindowHandle, &mut App) -> bool;
pub type WindowCloseRequestedFn = fn(&Window, &mut App, &'static str);
pub type ShowFeaturesFn = fn(&mut App);
pub type TextMenuFn = fn(NativeMenu, bool) -> NativeMenu;
pub type SecondBrainEnabledFn = fn(&App) -> bool;
pub type SecondBrainAccessFn = fn(&App) -> NetworkAccess;

pub struct BrainHooks {
    pub build_menus: BuildMenusFn,
    pub confirm_deleted_for_quit: ConfirmDeletedForQuitFn,
    pub save_all_for_quit: SaveAllForQuitFn,
    pub save_window: SaveWindowFn,
    pub window_close_requested: WindowCloseRequestedFn,
    pub show_features: ShowFeaturesFn,
    pub text_menu: TextMenuFn,
    pub second_brain_enabled: SecondBrainEnabledFn,
    pub second_brain_access: SecondBrainAccessFn,
}

impl Global for BrainHooks {}

/// Register app-level hooks once at startup (and in `test_init`).
pub fn install(hooks: BrainHooks, cx: &mut App) {
    cx.set_global(hooks);
}

pub(crate) fn build_menus(cx: &mut App) -> Vec<Menu> {
    if let Some(hooks) = cx.try_global::<BrainHooks>() {
        let f = hooks.build_menus;
        return f(cx);
    }
    vec![Menu {
        name: "Okilum".into(),
        items: vec![MenuItem::action(
            "Quit Okilum",
            crate::brain::app_quit::Quit,
        )],
        disabled: false,
    }]
}

pub(crate) fn confirm_deleted_for_quit(retry: fn(&mut App), cx: &mut App) -> bool {
    if let Some(hooks) = cx.try_global::<BrainHooks>() {
        let f = hooks.confirm_deleted_for_quit;
        return f(retry, cx);
    }
    true
}

pub(crate) fn save_all_for_quit(cx: &mut App) -> bool {
    if let Some(hooks) = cx.try_global::<BrainHooks>() {
        let f = hooks.save_all_for_quit;
        return f(cx);
    }
    true
}

pub(crate) fn save_window(window: AnyWindowHandle, cx: &mut App) -> bool {
    if let Some(hooks) = cx.try_global::<BrainHooks>() {
        let f = hooks.save_window;
        return f(window, cx);
    }
    true
}

pub(crate) fn window_close_requested(window: &Window, cx: &mut App, reason: &'static str) {
    if let Some(hooks) = cx.try_global::<BrainHooks>() {
        let f = hooks.window_close_requested;
        f(window, cx, reason);
    }
}

pub(crate) fn show_features(cx: &mut App) {
    if let Some(hooks) = cx.try_global::<BrainHooks>() {
        let f = hooks.show_features;
        f(cx);
    }
}

pub(crate) fn text_menu(menu: NativeMenu, editable: bool, cx: &App) -> NativeMenu {
    if let Some(hooks) = cx.try_global::<BrainHooks>() {
        let f = hooks.text_menu;
        return f(menu, editable);
    }
    menu
}

pub(crate) fn second_brain_enabled(cx: &App) -> bool {
    if let Some(hooks) = cx.try_global::<BrainHooks>() {
        let f = hooks.second_brain_enabled;
        return f(cx);
    }
    true
}

pub(crate) fn second_brain_access(cx: &App) -> NetworkAccess {
    if let Some(hooks) = cx.try_global::<BrainHooks>() {
        let f = hooks.second_brain_access;
        return f(cx);
    }
    NetworkAccess::new(true)
}

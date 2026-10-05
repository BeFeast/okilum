//! Application-owned Quit waits for all managed editors, before calling App::quit.
//! OS termination and Dock Quit bypass this action and are not covered.
use super::*;

gpui::actions!(tessera, [Quit]);

#[derive(Clone)]
struct Participant {
    view: WeakEntity<BrainView>,
    window: AnyWindowHandle,
}
#[derive(Clone)]
struct Intent {
    token: u64,
    participants: Vec<Participant>,
    windows: Vec<AnyWindowHandle>,
}
#[derive(Default)]
struct Coordinator {
    registered: Vec<Participant>,
    pending: Option<Intent>,
    next_token: u64,
    quit_issued: bool,
}
impl Global for Coordinator {}

pub(crate) fn install(cx: &mut App) {
    cx.default_global::<Coordinator>();
    cx.on_action(|_: &Quit, cx| cx.defer(request));
    cx.bind_keys([KeyBinding::new("secondary-q", Quit, None)]);
    set_menus(cx);
}

/// Also called again when an updater menu item changes its checked state.
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

pub(super) fn register(view: WeakEntity<BrainView>, window: AnyWindowHandle, cx: &mut App) {
    cx.default_global::<Coordinator>();
    let state = cx.global_mut::<Coordinator>();
    state.registered.push(Participant { view, window });
    if let Some(intent) = &state.pending {
        let token = intent.token;
        cx.defer(move |cx| cancel(token, cx));
    }
}

fn request(cx: &mut App) {
    if !crate::reader_editor::save_all(cx) {
        return;
    }
    let state = cx.default_global::<Coordinator>();
    if state.pending.is_some() || state.quit_issued {
        return;
    }
    let windows = cx.windows();
    let registered = cx.global::<Coordinator>().registered.clone();
    let participants = registered
        .into_iter()
        .filter(|p| windows.contains(&p.window) && p.view.upgrade().is_some())
        .collect::<Vec<_>>();
    let state = cx.global_mut::<Coordinator>();
    state.next_token += 1;
    let token = state.next_token;
    state.registered = participants.clone();
    state.pending = Some(Intent {
        token,
        participants: participants.clone(),
        windows,
    });
    for participant in participants {
        let result = participant.window.update(cx, |_, window, cx| {
            participant.view.update(cx, |view, cx| {
                view.editor_request_app_quit(token, window, cx)
            })
        });
        if !matches!(result, Ok(Ok(()))) {
            cancel(token, cx);
            return;
        }
    }
    finish(token, cx);
}

pub(super) fn queue_finish(token: u64, cx: &mut App) {
    cx.defer(move |cx| finish(token, cx));
}

fn finish(token: u64, cx: &mut App) {
    let Some(intent) = cx
        .try_global::<Coordinator>()
        .and_then(|s| s.pending.clone())
        .filter(|i| i.token == token)
    else {
        return;
    };
    let windows = cx.windows();
    if windows.len() != intent.windows.len() || !intent.windows.iter().all(|w| windows.contains(w))
    {
        cancel(token, cx);
        return;
    }
    for participant in intent.participants {
        let Some(view) = participant.view.upgrade() else {
            cancel(token, cx);
            return;
        };
        if !view.read(cx).editor_app_quit_ready(token, cx) {
            return;
        }
    }
    let state = cx.global_mut::<Coordinator>();
    state.pending = None;
    state.quit_issued = true;
    cx.quit();
}

pub(super) fn queue_cancel(token: u64, cx: &mut App) {
    cx.defer(move |cx| cancel(token, cx));
}

fn cancel(token: u64, cx: &mut App) {
    let Some(intent) = cx
        .try_global::<Coordinator>()
        .and_then(|s| s.pending.clone())
        .filter(|i| i.token == token)
    else {
        return;
    };
    cx.global_mut::<Coordinator>().pending = None;
    for participant in intent.participants {
        let _ = participant.view.update(cx, |view, cx| {
            view.editor_cancel_app_quit(token, cx);
        });
    }
}

#[cfg(test)]
pub(in crate::brain) fn test_request(cx: &mut App) {
    request(cx);
}
#[cfg(test)]
pub(in crate::brain) fn test_state(cx: &App) -> (Option<u64>, bool) {
    let state = cx.global::<Coordinator>();
    (state.pending.as_ref().map(|i| i.token), state.quit_issued)
}

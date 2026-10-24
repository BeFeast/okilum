//! Non-modal tools belong to the window that opened them (#1582).
use super::*;
use std::collections::{HashMap, HashSet};

#[derive(Default)]
struct Owners(HashMap<WindowId, (AnyWindowHandle, AnyWindowHandle)>);
impl Global for Owners {}

/// Commands invoked in Settings/Feedback still belong to their main window.
pub(crate) fn owner(window: AnyWindowHandle, cx: &App) -> AnyWindowHandle {
    cx.try_global::<Owners>()
        .and_then(|owners| owners.0.get(&window.window_id()))
        .map_or(window, |(owner, _)| *owner)
}

pub(crate) fn options(
    owner: Option<AnyWindowHandle>,
    preferred: Size<Pixels>,
    minimum: Size<Pixels>,
    cx: &mut App,
) -> WindowOptions {
    let placement = owner.and_then(|owner| {
        owner
            .update(cx, |_, window, cx| {
                let display = window.display(cx);
                let mut bounds = Bounds::new(
                    window.bounds().center() - point(preferred.width / 2., preferred.height / 2.),
                    preferred,
                );
                if let Some(display) = &display {
                    let visible = display.visible_bounds();
                    bounds.size.width = bounds.size.width.min(visible.size.width);
                    bounds.size.height = bounds.size.height.min(visible.size.height);
                    bounds.origin.x = bounds
                        .origin
                        .x
                        .max(visible.left())
                        .min(visible.right() - bounds.size.width);
                    bounds.origin.y = bounds
                        .origin
                        .y
                        .max(visible.top())
                        .min(visible.bottom() - bounds.size.height);
                }
                (bounds, display.map(|display| display.id()))
            })
            .ok()
    });
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(placement.as_ref().map_or_else(
            || window_state::default_bounds(preferred, cx),
            |(bounds, _)| *bounds,
        ))),
        display_id: placement.and_then(|(_, display)| display),
        window_min_size: Some(minimum),
        kind: gpui::WindowKind::Floating,
        is_minimizable: false,
        // Set the HWND owner/styles before the first show (no taskbar flash).
        show: !cfg!(windows) || owner.is_none(),
        ..TitleBar::window_options()
    }
}

/// Register only after opening succeeds. Closing a tool never closes its owner;
/// closing an owner also removes its tools from GPUI, not just the native WM.
pub(crate) fn register(owner: Option<AnyWindowHandle>, tool: AnyWindowHandle, cx: &mut App) {
    let Some(owner) = owner else { return };
    if !cx.has_global::<Owners>() {
        cx.set_global(Owners::default());
        cx.on_window_closed(|cx, _| cx.defer(close_orphans))
            .detach();
    }
    cx.global_mut::<Owners>()
        .0
        .insert(tool.window_id(), (owner, tool));
}

fn close_orphans(cx: &mut App) {
    let alive: HashSet<_> = cx.windows().into_iter().map(|w| w.window_id()).collect();
    let mut close = Vec::new();
    cx.global_mut::<Owners>().0.retain(|id, (owner, tool)| {
        if !alive.contains(id) {
            return false;
        }
        if !alive.contains(&owner.window_id()) {
            close.push(*tool);
            return false;
        }
        true
    });
    for tool in close {
        let _ = tool.update(cx, |_, window, _| window.remove_window());
    }
}

/// Windows needs an explicit non-modal HWND owner; macOS needs a child-window
/// relationship rather than GPUI Floating's application-wide floating level.
/// Linux Floating already requests a transient parent from the compositor.
pub(crate) fn attach(owner: Option<AnyWindowHandle>, tool: &Window, cx: &mut App) {
    let Some(owner) = owner else { return };
    if let Err(error) = owner
        .update(cx, |_, owner, _| native::attach(owner, tool))
        .and_then(|r| r)
    {
        eprintln!("Could not attach tool window to its owner: {error:#}");
    }
    #[cfg(windows)]
    // GPUI 0.3.3 WindowsWindow::activate first applies the initial placement
    // saved for show:false, then activates it. The native test exercises the
    // underlying hidden-window placement contract; focus alone is not enough.
    tool.activate_window();
}

#[cfg(not(any(windows, target_os = "macos")))]
mod native {
    pub(super) fn attach(_: &gpui::Window, _: &gpui::Window) -> anyhow::Result<()> {
        Ok(())
    }
}

#[cfg(windows)]
mod native {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows_sys::Win32::{
        Foundation::{GetLastError, SetLastError, HWND},
        UI::WindowsAndMessaging::*,
    };

    pub(super) fn attach(owner: &gpui::Window, tool: &gpui::Window) -> anyhow::Result<()> {
        // TestPlatform has no native HWND; production Win32 windows always do.
        let (Ok(owner), Ok(tool)) = (
            HasWindowHandle::window_handle(owner),
            HasWindowHandle::window_handle(tool),
        ) else {
            return Ok(());
        };
        let (RawWindowHandle::Win32(owner), RawWindowHandle::Win32(tool)) =
            (owner.as_raw(), tool.as_raw())
        else {
            return Ok(());
        };
        // SAFETY: live handles borrowed on the GPUI UI thread; neither is retained.
        unsafe { attach_hwnd(owner.hwnd.get() as HWND, tool.hwnd.get() as HWND) }
    }

    unsafe fn set_long(window: HWND, index: i32, value: isize) -> anyhow::Result<()> {
        // A zero previous value is valid, so clear/check LastError explicitly.
        unsafe {
            SetLastError(0);
        }
        let previous = unsafe { SetWindowLongPtrW(window, index, value) };
        let error = unsafe { GetLastError() };
        anyhow::ensure!(
            previous != 0 || error == 0,
            "SetWindowLongPtrW({index}): {error}"
        );
        Ok(())
    }

    unsafe fn attach_hwnd(owner: HWND, tool: HWND) -> anyhow::Result<()> {
        unsafe {
            set_long(tool, GWLP_HWNDPARENT, owner as isize)?;
        }
        let style = unsafe { GetWindowLongPtrW(tool, GWL_EXSTYLE) };
        unsafe {
            set_long(
                tool,
                GWL_EXSTYLE,
                (style & !(WS_EX_APPWINDOW as isize)) | WS_EX_TOOLWINDOW as isize,
            )?;
        }
        // Recompute the non-client frame after changing taskbar styles. Ownership
        // maintains z-order and minimize/restore without disabling the main window.
        anyhow::ensure!(
            unsafe {
                SetWindowPos(
                    tool,
                    std::ptr::null_mut(),
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOZORDER | SWP_FRAMECHANGED,
                )
            } != 0,
            "Could not refresh owned window frame"
        );
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use ::core::prelude::v1::test;
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::IsWindowEnabled;

        struct NativeWindow(HWND);
        impl Drop for NativeWindow {
            fn drop(&mut self) {
                // SAFETY: handles are created/owned by this test thread only.
                unsafe {
                    if IsWindow(self.0) != 0 {
                        DestroyWindow(self.0);
                    }
                }
            }
        }
        fn native_window() -> NativeWindow {
            let class: Vec<u16> = "STATIC\0".encode_utf16().collect();
            // SAFETY: built-in window class, no callback pointers or borrowed
            // state. RAII destroys each test HWND even if an assertion panics.
            let handle = unsafe {
                CreateWindowExW(
                    WS_EX_APPWINDOW,
                    class.as_ptr(),
                    class.as_ptr(),
                    WS_OVERLAPPEDWINDOW,
                    0,
                    0,
                    300,
                    200,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null(),
                )
            };
            assert!(!handle.is_null());
            NativeWindow(handle)
        }
        #[test]
        fn windows_owned_tool_keeps_owner_enabled_and_follows_minimize_restore_destroy() {
            let owner = native_window();
            let tool = native_window();
            unsafe {
                assert_eq!(
                    IsWindowVisible(tool.0),
                    0,
                    "starts hidden, like GPUI show:false"
                );
                // GPUI retrieve_window_placement caches GetWindowPlacement for
                // show:false; activate later applies it with SetWindowPlacement.
                let mut initial = WINDOWPLACEMENT {
                    length: std::mem::size_of::<WINDOWPLACEMENT>() as u32,
                    ..Default::default()
                };
                assert_ne!(GetWindowPlacement(tool.0, &mut initial), 0);
                attach_hwnd(owner.0, tool.0).unwrap();
                assert_eq!(GetWindow(tool.0, GW_OWNER), owner.0);
                assert_ne!(IsWindowEnabled(owner.0), 0, "non-modal positive control");
                let style = GetWindowLongPtrW(tool.0, GWL_EXSTYLE) as u32;
                assert_eq!(style & WS_EX_APPWINDOW, 0);
                assert_ne!(style & WS_EX_TOOLWINDOW, 0);
                ShowWindow(owner.0, SW_SHOW);
                assert_ne!(SetWindowPlacement(tool.0, &initial), 0);
                assert_ne!(
                    IsWindowVisible(tool.0),
                    0,
                    "deferred placement must show the tool"
                );
                ShowWindow(owner.0, SW_MINIMIZE);
                assert_eq!(IsWindowVisible(tool.0), 0);
                ShowWindow(owner.0, SW_RESTORE);
                assert_ne!(IsWindowVisible(tool.0), 0);
                assert_ne!(DestroyWindow(owner.0), 0);
                assert_eq!(IsWindow(tool.0), 0);
            }
        }
    }
}

#[cfg(target_os = "macos")]
mod native {
    use objc2::{
        msg_send,
        runtime::{AnyObject, Bool},
    };
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    fn ns_window(window: &gpui::Window) -> Option<*mut AnyObject> {
        let RawWindowHandle::AppKit(handle) = HasWindowHandle::window_handle(window).ok()?.as_raw()
        else {
            return None;
        };
        // SAFETY: synchronous UI-thread access to the live GPUI NSView.
        let window: *mut AnyObject =
            unsafe { msg_send![&*handle.ns_view.as_ptr().cast::<AnyObject>(), window] };
        (!window.is_null()).then_some(window)
    }

    pub(super) fn attach(owner: &gpui::Window, tool: &gpui::Window) -> anyhow::Result<()> {
        let (Some(owner), Some(tool)) = (ns_window(owner), ns_window(tool)) else {
            return Ok(());
        };
        // SAFETY: both NSWindows are borrowed from live handles on the UI thread.
        // Child ordering preserves main-window interaction; no modal sheet/session.
        unsafe {
            let level: isize = msg_send![&*owner, level];
            let _: () = msg_send![&*tool, setLevel: level];
            let _: () = msg_send![&*tool, setExcludedFromWindowsMenu: Bool::YES];
            let _: () = msg_send![&*owner, addChildWindow: &*tool, ordered: 1_isize];
        }
        Ok(())
    }
}

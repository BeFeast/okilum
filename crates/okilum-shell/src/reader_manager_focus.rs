//! UI-thread-local lifetime of the AppKit Manager responder bridge.
use std::{ffi::c_void, marker::PhantomData, ptr::NonNull, rc::Rc};

extern "C" {
    fn okilum_manager_focus_attach(webview: *mut c_void, parent: *mut c_void) -> *mut c_void;
    fn okilum_manager_focus_is_focused(handle: *mut c_void) -> bool;
    fn okilum_manager_focus_refresh(handle: *mut c_void);
    fn okilum_manager_focus_detach(handle: *mut c_void);
}

pub(super) struct FocusRouting {
    handle: NonNull<c_void>,
    _main_thread: PhantomData<Rc<()>>,
}
impl FocusRouting {
    /// Both views belong to the current GPUI window and remain owned by it/Wry.
    /// The Objective-C bridge keeps weak references and removes its monitors on drop.
    pub(super) fn new(view: &wry::WebView, window: &gpui::Window) -> anyhow::Result<Self> {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        use wry::WebViewExtMacOS;
        let RawWindowHandle::AppKit(parent) = window.window_handle()?.as_raw() else {
            anyhow::bail!("Manager requires a native AppKit window");
        };
        let webview = view.webview();
        // SAFETY: this constructor runs on the UI thread with live native views.
        let handle = unsafe {
            okilum_manager_focus_attach(
                objc2::rc::Retained::as_ptr(&webview).cast_mut().cast(),
                parent.ns_view.as_ptr(),
            )
        };
        Ok(Self {
            handle: NonNull::new(handle)
                .ok_or_else(|| anyhow::anyhow!("Cannot attach Manager input"))?,
            _main_thread: PhantomData,
        })
    }
    pub(super) fn focused(&self) -> bool {
        // SAFETY: handle is retained until Drop and this type cannot leave the UI thread.
        unsafe { okilum_manager_focus_is_focused(self.handle.as_ptr()) }
    }
    pub(super) fn refresh(&self) {
        // SAFETY: same UI-thread-owned handle.
        unsafe { okilum_manager_focus_refresh(self.handle.as_ptr()) }
    }
}
impl Drop for FocusRouting {
    fn drop(&mut self) {
        // SAFETY: detach removes callbacks and releases exactly one retained handle.
        unsafe { okilum_manager_focus_detach(self.handle.as_ptr()) }
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::{c_char, CStr, CString};
    extern "C" {
        fn okilum_manager_edit_action(
            key: *const c_char,
            command: bool,
            shift: bool,
            control: bool,
            option: bool,
        ) -> *const c_char;
    }
    fn action(
        key: &str,
        command: bool,
        shift: bool,
        control: bool,
        option: bool,
    ) -> Option<String> {
        let key = CString::new(key).unwrap();
        // SAFETY: a live C string is passed; native results are static selector literals.
        let result =
            unsafe { okilum_manager_edit_action(key.as_ptr(), command, shift, control, option) };
        if result.is_null() {
            None
        } else {
            Some(
                unsafe { CStr::from_ptr(result) }
                    .to_str()
                    .unwrap()
                    .to_owned(),
            )
        }
    }
    #[test]
    fn native_edit_commands_select_responder_actions_without_reader_dispatch() {
        for (key, shift, expected) in [
            ("v", false, "paste:"),
            ("c", false, "copy:"),
            ("x", false, "cut:"),
            ("a", false, "selectAll:"),
            ("z", false, "undo:"),
            ("z", true, "redo:"),
        ] {
            assert_eq!(
                action(key, true, shift, false, false).as_deref(),
                Some(expected)
            );
        }
        assert_eq!(
            action("v", false, false, false, false),
            None,
            "ordinary text goes to native keyDown"
        );
        assert_eq!(action("v", true, false, true, false), None);
        assert_eq!(
            action("c", true, false, false, true),
            None,
            "modified T3 shortcuts go to native keyDown"
        );
        assert_eq!(
            action("k", true, false, false, false),
            None,
            "T3 search goes to native keyDown"
        );
    }
}

//! UI-thread-local lifetime of the AppKit Manager responder bridge.
use std::{ffi::c_void, marker::PhantomData, ptr::NonNull, rc::Rc};

extern "C" {
    fn okilum_manager_focus_attach(
        webview: *mut c_void,
        parent: *mut c_void,
        focus_changed: extern "C" fn(*mut c_void, i32),
        context: *mut c_void,
    ) -> *mut c_void;
    fn okilum_manager_focus_is_focused(handle: *mut c_void) -> bool;
    fn okilum_manager_focus_refresh(handle: *mut c_void);
    fn okilum_manager_focus_document_loaded(handle: *mut c_void);
    fn okilum_manager_focus_detach(handle: *mut c_void);
}

pub(super) struct FocusRouting {
    handle: NonNull<c_void>,
    _main_thread: PhantomData<Rc<()>>,
    _events: Box<async_channel::Sender<String>>,
}
impl FocusRouting {
    /// Both views belong to the current GPUI window and remain owned by it/Wry.
    /// The Objective-C bridge keeps weak references and removes its monitors on drop.
    pub(super) fn new(
        view: &wry::WebView,
        window: &gpui::Window,
        events: async_channel::Sender<String>,
    ) -> anyhow::Result<Self> {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        use wry::WebViewExtMacOS;
        let RawWindowHandle::AppKit(parent) = HasWindowHandle::window_handle(window)?.as_raw()
        else {
            anyhow::bail!("Manager requires a native AppKit window");
        };
        let webview = view.webview();
        let mut events = Box::new(events);
        // SAFETY: this constructor runs on the UI thread with live native views.
        let handle = unsafe {
            okilum_manager_focus_attach(
                objc2::rc::Retained::as_ptr(&webview).cast_mut().cast(),
                parent.ns_view.as_ptr(),
                focus_changed,
                (&mut *events as *mut async_channel::Sender<String>).cast(),
            )
        };
        Ok(Self {
            handle: NonNull::new(handle)
                .ok_or_else(|| anyhow::anyhow!("Cannot attach Manager input"))?,
            _main_thread: PhantomData,
            _events: events,
        })
    }
    pub(super) fn focused(&self) -> bool {
        // SAFETY: handle is retained until Drop and this type cannot leave the UI thread.
        unsafe { okilum_manager_focus_is_focused(self.handle.as_ptr()) }
    }
    pub(super) fn document_loaded(&self) {
        // SAFETY: the retained bridge and document belong to this UI thread.
        unsafe { okilum_manager_focus_document_loaded(self.handle.as_ptr()) }
    }
    pub(super) fn refresh(&self) {
        // SAFETY: same UI-thread-owned handle.
        unsafe { okilum_manager_focus_refresh(self.handle.as_ptr()) }
    }
}
extern "C" fn focus_changed(context: *mut c_void, kind: i32) {
    // SAFETY: the boxed sender outlives the native monitor; Drop detaches first.
    let events = unsafe { &*context.cast::<async_channel::Sender<String>>() };
    let event = match kind {
        1 => "native-focus",
        2 => "escape",
        _ => "blur",
    };
    let _ = events.try_send(event.into());
}

impl Drop for FocusRouting {
    fn drop(&mut self) {
        // SAFETY: detach removes callbacks and releases exactly one retained handle.
        unsafe { okilum_manager_focus_detach(self.handle.as_ptr()) }
    }
}

#[cfg(test)]
mod tests {
    use ::core::prelude::v1::test;
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
    #[test]
    fn russian_layout_uses_edit_key_positions_without_losing_modifier_guards() {
        extern "C" {
            fn okilum_manager_edit_action_for_keycode(
                key: *const c_char,
                code: u16,
                command: bool,
                shift: bool,
                control: bool,
                option: bool,
            ) -> *const c_char;
        }
        for (key, code, expected) in [
            ("м", 9, "paste:"),
            ("с", 8, "copy:"),
            ("ч", 7, "cut:"),
            ("ф", 0, "selectAll:"),
            ("я", 6, "undo:"),
            ("c", 9, "copy:"),
        ] {
            let key = CString::new(key).unwrap();
            // SAFETY: pure selector mapping; inputs live and results are static literals.
            let action = unsafe {
                okilum_manager_edit_action_for_keycode(
                    key.as_ptr(),
                    code,
                    true,
                    false,
                    false,
                    false,
                )
            };
            assert_eq!(
                unsafe { CStr::from_ptr(action) }.to_str().unwrap(),
                expected
            );
            assert!(unsafe {
                okilum_manager_edit_action_for_keycode(key.as_ptr(), code, true, false, true, false)
            }
            .is_null());
        }
    }
    #[test]
    fn webview_editor_webview_rearms_each_entry() {
        extern "C" {
            fn okilum_manager_focus_cycle_regression() -> bool;
        }
        // SAFETY: pure native state-machine regression; no app or GUI required.
        assert!(unsafe { okilum_manager_focus_cycle_regression() });
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
            action("v", true, true, false, false),
            None,
            "Cmd+Shift+V reaches WebKit unchanged"
        );
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
    extern "C" {
        fn okilum_manager_command_queue_regression(undo: bool) -> bool;
    }
    #[test]
    fn rapid_paste_paste_keeps_both_commands_in_fifo_order() {
        // SAFETY: the regression runs the production Foundation-only scheduler;
        // it creates no window, browser, clipboard or GUI session.
        assert!(unsafe { okilum_manager_command_queue_regression(false) });
    }
    #[test]
    fn rapid_paste_undo_keeps_order_and_rejects_obsolete_completion() {
        assert!(unsafe { okilum_manager_command_queue_regression(true) });
    }

    #[test]
    #[ignore = "requires an active macOS GUI session and OKILUM_NATIVE_GUI=1"]
    fn real_nsapp_webview_paste_manual() {
        assert_eq!(std::env::var("OKILUM_NATIVE_GUI").as_deref(), Ok("1"));
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let status = std::process::Command::new("bash")
            .arg("scripts/ci/check-manager-webview-paste.sh")
            .current_dir(root)
            .status()
            .expect("cannot start native GUI fixture");
        assert!(status.success(), "native GUI fixture failed");
    }
}

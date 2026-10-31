//! Finder Services feed the same queued, validated folder dispatcher as OS opens.
use std::{
    ffi::{c_char, CStr},
    sync::OnceLock,
};

static OPENS: OnceLock<async_channel::Sender<Vec<String>>> = OnceLock::new();

extern "C" {
    fn okilum_folder_service_install(callback: extern "C" fn(*const c_char));
}

extern "C" fn open(url: *const c_char) {
    // The bridge supplies a live, NUL-terminated NSURL.absoluteString for this call.
    if url.is_null() {
        return;
    }
    if let (Some(sender), Ok(url)) = (OPENS.get(), unsafe { CStr::from_ptr(url) }.to_str()) {
        let _ = sender.try_send(vec![url.to_owned()]);
    }
}

pub(crate) fn install(sender: async_channel::Sender<Vec<String>>) {
    if OPENS.set(sender).is_ok() {
        unsafe { okilum_folder_service_install(open) };
    }
}

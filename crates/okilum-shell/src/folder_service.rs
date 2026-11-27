//! Finder Services feed the same queued, validated folder dispatcher as OS opens.
use std::{
    ffi::{c_char, CStr},
    sync::OnceLock,
};

static OPENS: OnceLock<async_channel::Sender<Vec<String>>> = OnceLock::new();

extern "C" {
    fn okilum_folder_service_install(
        vault_callback: extern "C" fn(*const c_char),
        single_file_callback: extern "C" fn(*const c_char),
    );
}

extern "C" fn open(url: *const c_char) {
    // The bridge supplies a live, NUL-terminated NSURL.absoluteString for this call.
    if url.is_null() {
        return;
    }
    if let (Some(sender), Ok(url)) = (OPENS.get(), unsafe { CStr::from_ptr(url) }.to_str()) {
        // This Service asks for a vault; a plain folder open is a folder view (#3098).
        let _ = sender.try_send(vec![vault_request(url)]);
    }
}

extern "C" fn open_single_file(url: *const c_char) {
    if url.is_null() {
        return;
    }
    if let (Some(sender), Ok(url)) = (OPENS.get(), unsafe { CStr::from_ptr(url) }.to_str()) {
        // This Service asks for standalone single-file mode even inside an open vault (#3264).
        let _ = sender.try_send(vec![single_file_request(url)]);
    }
}

/// The queued form of a Service folder URL: marked as an explicit vault request.
fn vault_request(url: &str) -> String {
    format!("{}{url}", crate::reader_open::VAULT_URL_PREFIX)
}

/// The queued form of a Service file URL: marked as an explicit single-file request.
fn single_file_request(url: &str) -> String {
    format!("{}{url}", crate::reader_open::SINGLE_FILE_URL_PREFIX)
}

pub(crate) fn install(sender: async_channel::Sender<Vec<String>>) {
    if OPENS.set(sender).is_ok() {
        unsafe { okilum_folder_service_install(open, open_single_file) };
    }
}

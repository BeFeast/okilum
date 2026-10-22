use super::*;
unsafe extern "C" {
    fn okilum_capture_own_window(length: *mut usize) -> *mut u8;
    fn okilum_capture_free(bytes: *mut u8);
}
pub(super) fn capture() -> Result<PngBytes, Unsupported> {
    // The bridge enforces main-thread + NSApp ownership before requesting only
    // that window ID. It never requests Screen Recording permission.
    unsafe {
        let mut length = 0;
        let bytes = okilum_capture_own_window(&mut length);
        if bytes.is_null() {
            return Err(Unsupported::PermissionOrUnavailable);
        }
        let result = if length <= MAX_PNG_BYTES {
            Ok(std::slice::from_raw_parts(bytes, length).to_vec())
        } else {
            Err(Unsupported::TooLarge)
        };
        okilum_capture_free(bytes);
        result
    }
}

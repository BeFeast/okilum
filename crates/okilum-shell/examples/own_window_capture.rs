//! Hosted macOS native control; runs on the real main thread, without GPUI.
#[cfg(target_os = "macos")]
fn main() {
    use okilum_shell::own_window_capture::{capture_own_window, Unsupported};
    unsafe extern "C" {
        fn okilum_capture_probe_open() -> i32;
        fn okilum_capture_probe_close();
    }
    assert_eq!(
        capture_own_window(),
        Err(Unsupported::PermissionOrUnavailable)
    );
    unsafe {
        assert_eq!(
            okilum_capture_probe_open(),
            1,
            "Metal control must actually render"
        );
    }
    let result = capture_own_window();
    unsafe {
        okilum_capture_probe_close();
    }
    match result {
        Ok(png) => {
            let image = image::load_from_memory(&png).unwrap().to_rgba8();
            let center = image.get_pixel(image.width() / 2, image.height() / 2).0;
            assert!(
                center[0] > 200 && center[1] < 70 && center[2] < 90,
                "Metal frame missing: {center:?}"
            );
            println!("own-window macOS Metal pixel control PASS");
        }
        Err(Unsupported::PermissionOrUnavailable) => {
            println!("own-window macOS UNSUPPORTED: API unavailable or Screen Recording required; no permission requested. Native capture NOT VERIFIED; attach-only.");
        }
        other => panic!("Unexpected own-window outcome: {other:?}"),
    }
}
#[cfg(not(target_os = "macos"))]
fn main() {}

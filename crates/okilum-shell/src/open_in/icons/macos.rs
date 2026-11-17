//! macOS: the `.app` bundle's icon from NSWorkspace, rasterized for the menu.
use super::raster;
use gpui::RenderImage;
use objc2::{class, msg_send, runtime::AnyObject};
use objc2_foundation::NSString;
use std::{path::Path, sync::Arc};

/// The `.app` bundle containing `path` (or `path` itself), following the
/// symlinks a `PATH` launcher such as `/usr/local/bin/code` goes through.
pub(super) fn bundle(path: &Path) -> Option<std::path::PathBuf> {
    let resolved = std::fs::canonicalize(path).ok()?;
    resolved
        .ancestors()
        .find(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("app")) && p.is_dir())
        .map(Path::to_owned)
}

pub(super) fn load(path: &Path) -> Option<Arc<RenderImage>> {
    // NSWorkspace answers a generic icon for any path, even a missing one;
    // without a bundle the menu's own neutral glyph is clearer.
    let bundle = bundle(path)?;
    let bundle = NSString::from_str(bundle.to_str()?);
    objc2::rc::autoreleasepool(|_| {
        // SAFETY: `sharedWorkspace` and `iconForFile:` are documented as
        // thread-safe; every returned object is autoreleased into this pool
        // and the borrowed TIFF bytes are copied by `raster` before it drains.
        unsafe {
            let workspace: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
            if workspace.is_null() {
                return None;
            }
            let icon: *mut AnyObject = msg_send![workspace, iconForFile: &*bundle];
            if icon.is_null() {
                return None;
            }
            let data: *mut AnyObject = msg_send![icon, TIFFRepresentation];
            if data.is_null() {
                return None;
            }
            let len: usize = msg_send![data, length];
            let bytes: *const u8 = msg_send![data, bytes];
            if bytes.is_null() || len == 0 {
                return None;
            }
            raster(std::slice::from_raw_parts(bytes, len))
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_system_bundle_has_an_icon_and_a_plain_file_falls_back() {
        // Ships with every macOS; also reached through a path inside it.
        let app = Path::new("/System/Applications/TextEdit.app");
        assert_eq!(bundle(&app.join("Contents/MacOS")).as_deref(), Some(app));
        let icon = load(app).expect("positive control: TextEdit has an icon");
        assert_eq!(u32::from(icon.size(0).width), 64);
        let temp = tempfile::tempdir().unwrap();
        let plain = temp.path().join("code");
        std::fs::write(&plain, "#!/bin/sh\n").unwrap();
        assert!(load(&plain).is_none(), "not in a bundle: neutral fallback");
        assert!(load(&temp.path().join("Missing.app")).is_none());
    }

    #[test]
    fn a_path_launcher_symlink_leads_to_its_bundle() {
        let temp = tempfile::tempdir().unwrap();
        let app = temp.path().join("Editor.app");
        let cli = app.join("Contents/Resources/app/bin/code");
        std::fs::create_dir_all(cli.parent().unwrap()).unwrap();
        std::fs::write(&cli, "#!/bin/sh\n").unwrap();
        let link = temp.path().join("code");
        std::os::unix::fs::symlink(&cli, &link).unwrap();
        let canonical = std::fs::canonicalize(&app).unwrap();
        assert_eq!(bundle(&link), Some(canonical.clone()));
        assert_eq!(bundle(&app), Some(canonical));
    }
}

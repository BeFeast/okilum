//! Installed Windows releases: download in the background, apply on next launch.
//! Installation ID BeFeast.Tessera deliberately differs from Reader state `tessera`.
use std::sync::{
    atomic::{AtomicBool, Ordering},
    OnceLock,
};
use velopack::{sources::HttpSource, HttpOptions, UpdateCheck, UpdateManager, UpdateOptions};

static AVAILABLE: OnceLock<bool> = OnceLock::new();
static BETA: AtomicBool = AtomicBool::new(false);
static BUSY: AtomicBool = AtomicBool::new(false);

fn manager() -> Result<UpdateManager, velopack::Error> {
    let channel = if beta() { "beta" } else { "stable" };
    UpdateManager::new(
        HttpSource::new_with_options(
            format!("https://updates.befeast.com/tessera/windows/{channel}/"),
            HttpOptions {
                TimeoutMilliseconds: 300_000,
                ..Default::default()
            },
        ),
        Some(UpdateOptions {
            ExplicitChannel: Some(channel.into()),
            MaximumDeltasBeforeFallback: 10,
            ..Default::default()
        }),
        None,
    )
}

pub(super) fn start() {
    let installed = manager().is_ok_and(|m| !m.get_is_portable());
    AVAILABLE.get_or_init(|| installed);
    if let Ok(root) = crate::reader_history::state_directory() {
        if let Ok(value) = std::fs::read_to_string(root.join("windows-update-channel")) {
            BETA.store(value.trim() != "stable", Ordering::Relaxed);
        }
    }
    if installed {
        check(false);
    }
}

pub(super) fn available() -> bool {
    AVAILABLE.get().copied().unwrap_or(false)
}

pub(super) fn beta() -> bool {
    BETA.load(Ordering::Relaxed)
}

pub(super) fn set_beta(value: bool) {
    let result = (|| -> anyhow::Result<()> {
        let root = crate::reader_history::state_directory()?;
        std::fs::create_dir_all(&root)?;
        std::fs::write(
            root.join("windows-update-channel"),
            if value { "beta" } else { "stable" },
        )?;
        Ok(())
    })();
    match result {
        Ok(()) => BETA.store(value, Ordering::Relaxed),
        Err(error) => message(&format!("Could not save update channel: {error}")),
    }
}

pub(super) fn check(manual: bool) {
    if !available() {
        return;
    }
    if BUSY.swap(true, Ordering::AcqRel) {
        if manual {
            message("An update check or download is already in progress.");
        }
        return;
    }
    // HTTP and package IO never block the GPUI/UI thread or vault preparation.
    std::thread::spawn(move || {
        let result = check_and_download();
        BUSY.store(false, Ordering::Release);
        if manual {
            match result {
                Ok(status) => message(status),
                Err(error) => message(&format!("Could not check or download updates. Your current version is unchanged.\n\n{error}")),
            }
        }
    });
}

fn check_and_download() -> Result<&'static str, velopack::Error> {
    let manager = manager()?;
    if manager.get_update_pending_restart().is_some() {
        return Ok("An update is ready. Quit Tessera and open it again to apply it.");
    }
    match manager.check_for_updates()? {
        UpdateCheck::UpdateAvailable(update) => {
            manager.download_updates(&update, None)?;
            Ok("Update downloaded. Quit Tessera and open it again to apply it. Your vault and Reader settings will be preserved.")
        }
        UpdateCheck::NoUpdateAvailable => Ok("You are up to date on the selected channel."),
        UpdateCheck::RemoteIsEmpty => {
            Ok("No release has been published on the selected channel yet.")
        }
    }
}

fn message(text: &str) {
    let text: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
    let title: Vec<u16> = "Tessera Updates".encode_utf16().chain(Some(0)).collect();
    // The buffers remain alive throughout the synchronous Win32 call.
    unsafe {
        windows_sys::Win32::UI::WindowsAndMessaging::MessageBoxW(
            std::ptr::null_mut(),
            text.as_ptr(),
            title.as_ptr(),
            windows_sys::Win32::UI::WindowsAndMessaging::MB_OK,
        );
    }
}

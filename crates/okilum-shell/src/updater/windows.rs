//! Installed Windows releases: download in the background, apply on next launch.
//! Installation ID BeFeast.Okilum deliberately differs from Reader state `okilum`.
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Mutex, OnceLock,
};
use velopack::{sources::HttpSource, HttpOptions, UpdateCheck, UpdateManager, UpdateOptions};

static AVAILABLE: OnceLock<bool> = OnceLock::new();
static BETA: AtomicBool = AtomicBool::new(true);
static BUSY: AtomicBool = AtomicBool::new(false);
static READY: OnceLock<Mutex<super::ready::Ready>> = OnceLock::new();
fn state() -> &'static Mutex<super::ready::Ready> {
    READY.get_or_init(|| Mutex::new(super::ready::Ready::default()))
}
fn identity(asset: &velopack::VelopackAsset) -> String {
    format!(
        "{}:{}:{}:{}:{}",
        asset.PackageId, asset.Version, asset.FileName, asset.Size, asset.SHA256
    )
}
pub(super) fn ready_package() -> Option<String> {
    state().lock().unwrap().package().map(str::to_owned)
}
pub(super) fn ready() -> bool {
    ready_package().is_some()
}
pub(super) fn take_announcement() -> Option<String> {
    state().lock().unwrap().take_announcement()
}
pub(super) fn restart(expected: Option<&str>, cx: &mut gpui::App) {
    let result = (|| -> anyhow::Result<()> {
        let manager = manager()?;
        let pending = manager.get_update_pending_restart();
        if pending.is_none() {
            state().lock().unwrap().observe(None);
        }
        let asset = pending.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "The downloaded update is no longer available. Check for updates again."
            )
        })?;
        let id = identity(asset);
        if expected.is_some_and(|expected| expected != id) {
            state().lock().unwrap().observe(Some(id));
            anyhow::bail!("The pending update changed. Check for updates again.");
        }
        {
            let mut state = state().lock().unwrap();
            if state.package() != Some(id.as_str()) {
                state.observe(Some(id));
                anyhow::bail!("The pending update changed. Choose Restart again.");
            }
            if !state.begin_restart(&id) {
                return Ok(());
            }
        }
        use super::ready::{restart_transaction, RestartStep};
        let mut restart_args = Vec::new();
        restart_transaction(|stage| -> anyhow::Result<()> {
            match stage {
                RestartStep::Arguments => {
                    restart_args = crate::reader_open::update_restart_args(cx)?
                }
                RestartStep::ProtectEditors => anyhow::ensure!(
                    crate::reader_editor::protect_all_for_quit(cx),
                    "Your latest edits could not be protected. Okilum will stay open."
                ),
                RestartStep::PersistState => crate::reader_ui_state::prepare_for_restart(cx)?,
                RestartStep::ArmUpdater => manager.wait_exit_then_apply_updates(
                    asset,
                    false,
                    true,
                    std::mem::take(&mut restart_args),
                )?,
                RestartStep::Quit => cx.quit(),
            }
            Ok(())
        })
    })();
    if let Err(error) = result {
        state().lock().unwrap().restart_failed();
        let message = format!("Could not restart to update: {error}");
        cx.defer(move |cx| {
            if let Some(window) = cx.active_window() {
                let _ = window.update(cx, |_, window, cx| {
                    crate::reader_toast::error(message, window, cx)
                });
            }
        });
    }
}

fn manager() -> Result<UpdateManager, velopack::Error> {
    let (url, channel) = super::windows_feed::endpoint(beta());
    UpdateManager::new(
        HttpSource::new_with_options(
            url,
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
    BETA.store(super::windows_feed::default_beta(), Ordering::Relaxed);
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
                Ok(status) if !ready() => message(status),
                Ok(_) => {},
                Err(error) => message(&format!("Could not check or download updates. Your current version is unchanged.\n\n{error}")),
            }
        }
    });
}

fn check_and_download() -> anyhow::Result<&'static str> {
    let manager = manager()?;
    let pending = manager.get_update_pending_restart();
    state()
        .lock()
        .unwrap()
        .observe(pending.as_ref().map(identity));
    if pending.is_some() {
        return Ok("Update ready. Choose Restart to update.");
    }
    match manager.check_for_updates()? {
        UpdateCheck::UpdateAvailable(update) => {
            manager.download_updates(&update, None)?;
            let pending = manager.get_update_pending_restart();
            state()
                .lock()
                .unwrap()
                .observe(pending.as_ref().map(identity));
            anyhow::ensure!(
                pending.is_some(),
                "The downloaded package is no longer available. Check for updates again."
            );
            Ok("Update ready. Choose Restart to update.")
        }
        UpdateCheck::NoUpdateAvailable => Ok("You are up to date on the selected channel."),
        UpdateCheck::RemoteIsEmpty => {
            Ok("No release has been published on the selected channel yet.")
        }
    }
}

fn message(text: &str) {
    let text: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
    let title: Vec<u16> = "Okilum Updates".encode_utf16().chain(Some(0)).collect();
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

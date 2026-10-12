//! Installed Windows releases: download in the background, apply on next launch.
//! Installation ID BeFeast.Okilum deliberately differs from Reader state `okilum`.
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Mutex, OnceLock,
};
use velopack::{sources::HttpSource, HttpOptions, UpdateCheck, UpdateManager, UpdateOptions};

static AVAILABLE: OnceLock<bool> = OnceLock::new();
static BETA: AtomicBool = AtomicBool::new(false);
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

const VERIFIED: &str = "windows-update-verified";

fn verified_path() -> Option<std::path::PathBuf> {
    crate::reader_history::state_directory()
        .ok()
        .map(|root| root.join(VERIFIED))
}

/// Records that the pending package belongs to the current channel's feed.
fn record_verified(asset: &velopack::VelopackAsset) {
    if let Some(path) = verified_path() {
        let _ = path.parent().map(std::fs::create_dir_all);
        let _ = std::fs::write(
            path,
            super::windows_feed::verified_record(beta(), &asset.SHA256),
        );
    }
}

fn forget_verified() {
    if let Some(path) = verified_path() {
        let _ = std::fs::remove_file(path);
    }
}

/// Deletes a downloaded package the current channel does not offer (#1318).
fn discard(asset: &velopack::VelopackAsset) {
    let packages = std::env::current_exe()
        .ok()
        .and_then(|exe| Some(exe.parent()?.parent()?.join("packages")));
    if let Some(packages) = packages {
        let _ = std::fs::remove_file(packages.join(&asset.FileName));
    }
    forget_verified();
}

/// Before `VelopackApp::run`: whether it may apply a pending package now. Only
/// a package checked against the channel the install follows today (#1318).
pub(crate) fn apply_pending_at_start() -> bool {
    let Ok(manager) = manager() else {
        return true;
    };
    let pending = manager.get_update_pending_restart();
    let record = verified_path().and_then(|path| std::fs::read_to_string(path).ok());
    pending.is_none()
        || super::windows_feed::may_apply_at_start(
            pending.as_ref().map(|asset| asset.SHA256.as_str()),
            record.as_deref(),
            beta(),
        )
}

fn manager() -> Result<UpdateManager, velopack::Error> {
    manager_for(beta())
}

fn manager_for(beta: bool) -> Result<UpdateManager, velopack::Error> {
    let (url, channel) = super::windows_feed::endpoint(beta);
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

/// Whether this copy is installed and which channel it follows. Read on first
/// use, so the diagnostic line written before `start` already says how Okilum
/// was installed (#1273).
fn settings() -> bool {
    *AVAILABLE.get_or_init(|| {
        let saved = crate::reader_history::state_directory()
            .ok()
            .and_then(|root| std::fs::read_to_string(root.join("windows-update-channel")).ok());
        let beta = super::windows_feed::follows_beta(saved.as_deref());
        BETA.store(beta, Ordering::Relaxed);
        manager_for(beta).is_ok_and(|m| !m.get_is_portable())
    })
}

pub(super) fn start() {
    if settings() {
        check(false);
    }
}

pub(super) fn available() -> bool {
    settings()
}

pub(super) fn beta() -> bool {
    settings();
    BETA.load(Ordering::Relaxed)
}

pub(super) fn set_beta(value: bool) {
    let previous = beta();
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
        Ok(()) => {
            BETA.store(value, Ordering::Relaxed);
            if previous != value {
                // A package downloaded for the other channel is checked again
                // before it can be applied (#1318).
                forget_verified();
                state().lock().unwrap().observe(None);
            }
        }
        Err(error) => message(&format!("Could not save update channel: {error}")),
    }
}

pub(super) fn check(manual: bool) {
    if !available() {
        return;
    }
    use super::status::{self, CheckStatus};
    if BUSY.swap(true, Ordering::AcqRel) {
        if manual {
            status::set(CheckStatus::Note(IN_PROGRESS.into()));
        }
        return;
    }
    if manual {
        status::set(CheckStatus::Checking);
    }
    // HTTP and package IO never block the GPUI/UI thread or vault preparation.
    // A manual check reports inline, never in a message box (#995).
    std::thread::spawn(move || {
        let result = check_and_download();
        BUSY.store(false, Ordering::Release);
        // A manual click during a background check is answered by its result.
        let waiting = matches!(status::get(), CheckStatus::Note(ref note) if note == IN_PROGRESS);
        if manual || waiting {
            status::set(match result {
                Ok(_) if ready() => CheckStatus::Ready,
                Ok(UP_TO_DATE) => CheckStatus::UpToDate(std::time::SystemTime::now()),
                Ok(note) => CheckStatus::Note(note.into()),
                Err(error) => CheckStatus::Failed(format!(
                    "{}. Your current version is unchanged.",
                    error.to_string().trim_end_matches('.')
                )),
            });
        }
    });
}

const UP_TO_DATE: &str = "You are up to date on the selected channel.";
const IN_PROGRESS: &str = "An update check or download is already in progress.";

fn check_and_download() -> anyhow::Result<&'static str> {
    let manager = manager()?;
    if let Some(asset) = manager.get_update_pending_restart() {
        // Applied only if the current channel's feed lists it (#1318).
        let listed = manager
            .get_release_feed()?
            .Assets
            .iter()
            .any(|offered| offered.SHA256 == asset.SHA256 && offered.Version == asset.Version);
        if listed {
            record_verified(&asset);
            state().lock().unwrap().observe(Some(identity(&asset)));
            return Ok("Update ready. Choose Restart to update.");
        }
        discard(&asset);
        state().lock().unwrap().observe(None);
    }
    match manager.check_for_updates()? {
        UpdateCheck::UpdateAvailable(update) => {
            manager.download_updates(&update, None)?;
            let pending = manager.get_update_pending_restart();
            if let Some(asset) = &pending {
                record_verified(asset);
            }
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
        UpdateCheck::NoUpdateAvailable => Ok(UP_TO_DATE),
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

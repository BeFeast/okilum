//! Installed Windows releases: download in the background, apply on next launch.
//! Installation ID BeFeast.Okilum deliberately differs from Reader state `okilum`.
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Mutex, OnceLock,
};
use velopack::{UpdateCheck, UpdateManager, UpdateOptions};

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
    // Serialize restart against background checks until Velopack is armed.
    if BUSY.swap(true, Ordering::AcqRel) {
        cx.defer(|cx| {
            if let Some(window) = cx.active_window() {
                let _ = window.update(cx, |_, window, cx| {
                    crate::reader_toast::error(
                        "The updater is busy. Wait for it to finish, then choose Restart now again.",
                        window,
                        cx,
                    )
                });
            }
        });
        return;
    }
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
        anyhow::ensure!(
            super::status::begin_restart(),
            "The update is not ready to restart. Check for updates again."
        );
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
                RestartStep::Quit => {
                    cx.quit();
                }
            }
            Ok(())
        })
    })();
    if let Err(error) = result {
        state().lock().unwrap().restart_failed();
        super::status::set(if ready() {
            super::status::CheckStatus::Ready
        } else {
            super::status::CheckStatus::Failed(error.to_string())
        });
        let message = format!("Could not restart to update: {error}");
        cx.defer(move |cx| {
            if let Some(window) = cx.active_window() {
                let _ = window.update(cx, |_, window, cx| {
                    crate::reader_toast::error(message, window, cx)
                });
            }
        });
    }
    BUSY.store(false, Ordering::Release);
}

const VERIFIED: &str = "windows-update-verified";

fn verified_path() -> Option<std::path::PathBuf> {
    crate::reader_history::state_directory()
        .ok()
        .map(|root| root.join(VERIFIED))
}

fn packages_dir() -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    Some(exe.parent()?.parent()?.join("packages"))
}

/// SHA256 of the downloaded package file, upper-case hex like the feed's.
/// Velopack's local pending asset leaves `SHA256` empty, so it is read here.
fn package_sha256(asset: &velopack::VelopackAsset) -> Option<String> {
    use sha2::{Digest, Sha256};
    let mut file = std::fs::File::open(packages_dir()?.join(&asset.FileName)).ok()?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher).ok()?;
    Some(
        hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02X}"))
            .collect(),
    )
}

/// Records that the pending package belongs to the current channel's feed.
fn record_verified(sha256: &str) {
    if let Some(path) = verified_path() {
        let _ = path.parent().map(std::fs::create_dir_all);
        let _ = std::fs::write(path, super::windows_feed::verified_record(beta(), sha256));
    }
}

fn forget_verified() {
    if let Some(path) = verified_path() {
        let _ = std::fs::remove_file(path);
    }
}

/// Deletes a downloaded package the current channel does not offer (#1318).
fn discard(asset: &velopack::VelopackAsset) {
    if let Some(packages) = packages_dir() {
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
    let Some(pending) = manager.get_update_pending_restart() else {
        return true;
    };
    let record = verified_path().and_then(|path| std::fs::read_to_string(path).ok());
    super::windows_feed::may_apply_at_start(
        package_sha256(&pending).as_deref(),
        record.as_deref(),
        beta(),
    )
}

/// The installed package's version from `current/sq.version` (#1319). Read
/// once: an applied update starts a new process.
pub(super) fn package_version() -> Option<String> {
    static PACKAGE: OnceLock<Option<String>> = OnceLock::new();
    PACKAGE
        .get_or_init(|| {
            settings()
                .then(|| manager().ok())
                .flatten()
                .map(|manager| manager.get_current_version_as_string())
        })
        .clone()
}

fn manager() -> Result<UpdateManager, velopack::Error> {
    manager_for(beta())
}

fn manager_for(beta: bool) -> Result<UpdateManager, velopack::Error> {
    let (url, channel) = super::windows_feed::endpoint(beta);
    UpdateManager::new(
        okilum_http::updates::Source::new(url, env!("OKILUM_RELEASE_VERSION"))
            .map_err(|error| velopack::Error::Other(format!("{error:#}")))?,
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

pub(super) fn check(_manual: bool) {
    if !available() {
        return;
    }
    use super::status::{self, CheckStatus};
    if BUSY.swap(true, Ordering::AcqRel) {
        return;
    }
    status::set(CheckStatus::Checking);
    // HTTP and package IO never block the GPUI/UI thread or vault preparation.
    // A manual check reports inline, never in a message box (#995).
    std::thread::spawn(move || {
        let result = check_and_download();
        // Download/preparation failures stay visible even for background checks.
        status::set(match result {
            Ok(_) if ready() => CheckStatus::Ready,
            Ok(UP_TO_DATE) => CheckStatus::UpToDate(std::time::SystemTime::now()),
            Ok(note) => CheckStatus::Note(note.into()),
            Err(error) => {
                crate::reader_diagnostics::network_failure("Windows update", &error);
                CheckStatus::Failed(format!(
                    "{}. Your current version is unchanged.",
                    okilum_http::reason(&error)
                ))
            }
        });
        BUSY.store(false, Ordering::Release);
    });
}

const UP_TO_DATE: &str = "You are up to date on the selected channel.";

fn check_and_download() -> anyhow::Result<&'static str> {
    let manager = manager()?;
    if let Some(asset) = manager.get_update_pending_restart() {
        // Applied only if the current channel's feed lists it (#1318). A feed
        // or a package file that cannot be read (a scan holding it, say) keeps
        // the package, unverified, for the next check.
        let feed = manager.get_release_feed()?;
        let sha256 = package_sha256(&asset).ok_or_else(|| {
            anyhow::anyhow!("The downloaded update could not be read. Okilum will try again later")
        })?;
        let listed = feed.Assets.iter().any(|offered| {
            offered.Version == asset.Version && offered.SHA256.eq_ignore_ascii_case(&sha256)
        });
        if listed {
            record_verified(&sha256);
            state().lock().unwrap().observe(Some(identity(&asset)));
            return Ok("Update ready. Choose Restart to update.");
        }
        discard(&asset);
        state().lock().unwrap().observe(None);
    }
    match manager.check_for_updates()? {
        UpdateCheck::UpdateAvailable(update) => {
            use super::status::{self, CheckStatus};
            status::set(CheckStatus::Downloading(Some(0)));
            let (send, receive) = std::sync::mpsc::channel::<i16>();
            // Join before Ready/Failed so a late progress event cannot replace it.
            std::thread::scope(|scope| {
                let progress = scope.spawn(move || {
                    for percent in receive {
                        status::set(CheckStatus::Downloading(Some(percent.clamp(0, 100) as u8)));
                    }
                });
                let result = manager.download_updates(&update, Some(send));
                let _ = progress.join();
                result
            })?;
            status::set(CheckStatus::Preparing);
            let pending = manager.get_update_pending_restart();
            if let Some(sha256) = pending.as_ref().and_then(package_sha256) {
                record_verified(&sha256);
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

//! Installed Windows releases: policy-controlled download, explicit guarded restart.
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
static RESTART: super::restart_intent::RestartIntent = super::restart_intent::RestartIntent::new();
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
/// Installs the newest pending package, whichever notice was clicked (#2679).
pub(super) fn restart(cx: &mut gpui::App) {
    if !available() {
        super::restart_feedback::refused("Open the installed app to update.");
        return;
    }
    if !RESTART.request() {
        return;
    }
    super::restart_feedback::attempt();
    super::restart_feedback::notify("verifying", "Checking the latest update before restarting…");
    if !check(true) && !BUSY.load(Ordering::Acquire) {
        RESTART.cancel();
        super::restart_feedback::refused(
            "The update could not be checked. Choose Restart now to retry.",
        );
        return;
    }
    finish_requested_restart(cx);
}

pub(super) fn finish_requested_restart(cx: &mut gpui::App) {
    use super::restart_intent::Next;
    match RESTART.poll(BUSY.load(Ordering::Acquire), super::status::get()) {
        Next::Wait => {}
        Next::Download => {
            super::restart_feedback::notify("downloading", "Downloading the latest update…");
            check(true);
        }
        Next::Apply => restart_verified(cx),
        Next::Refuse(reason) => super::restart_feedback::refused(reason),
    }
}

pub(super) fn cancel_requested_restart() {
    let requested = RESTART.requested();
    RESTART.cancel();
    if requested {
        super::restart_feedback::offer("Restart cancelled. Nothing was installed.");
    }
}

fn restart_verified(cx: &mut gpui::App) {
    // Serialize restart against background checks until Velopack is armed.
    // The refusal appears in the single update notice, never a second toast.
    if BUSY.swap(true, Ordering::AcqRel) {
        super::restart_feedback::refused(
            "The updater is busy. Wait for it to finish, then choose Restart now again.",
        );
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
        {
            let mut state = state().lock().unwrap();
            // Only a package verified against the current channel's feed may
            // be applied (#1318). The notice carries no package of its own:
            // Restart always targets the newest verified one (#2679).
            if state.package() != Some(id.as_str()) {
                state.observe(None);
                anyhow::bail!(
                    "The downloaded update must be checked again. Choose Check for Updates"
                );
            }
            if !state.begin_restart(&id) {
                super::restart_feedback::refused("The installer is already waiting for Okilum to close. Save or close any pending dialog, then quit Okilum to finish the update.");
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
                RestartStep::ProtectEditors => crate::reader_update_restart::protect_editors(cx)?,
                RestartStep::PersistState => crate::reader_ui_state::prepare_for_restart(cx)?,
                RestartStep::ArmUpdater => manager.wait_exit_then_apply_updates(
                    asset,
                    false,
                    true,
                    std::mem::take(&mut restart_args),
                )?,
                RestartStep::Quit => {
                    super::restart_feedback::record(
                        "accepted",
                        "Velopack armed after editor protection and state persistence",
                    );
                    cx.quit();
                }
            }
            Ok(())
        })
    })();
    if let Err(error) = result {
        super::restart_feedback::refused(format!("Could not restart to update: {error}"));
        state().lock().unwrap().restart_failed();
        super::status::set(if ready() {
            super::status::CheckStatus::RestartRetry(error.to_string())
        } else {
            super::status::CheckStatus::Failed(error.to_string())
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
fn record_verified(sha256: &str, offered: &velopack::VelopackAsset) {
    if let Some(path) = verified_path() {
        let _ = path.parent().map(std::fs::create_dir_all);
        let _ = std::fs::write(
            path,
            super::windows_feed::verified_download(
                beta(),
                &offered.Version,
                sha256,
                &offered.SHA256,
            ),
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
    if let Some(packages) = packages_dir() {
        let _ = std::fs::remove_file(packages.join(&asset.FileName));
    }
    forget_verified();
}

/// Ordinary startup never authorizes an installation (#2467). Explicit
/// Restart uses wait_exit_then_apply_updates after channel/editor guards.
pub(crate) fn apply_pending_at_start() -> bool {
    false
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
    manager_with_token(beta, Default::default())
}
fn manager_with_token(
    beta: bool,
    token: std::sync::Arc<AtomicBool>,
) -> Result<UpdateManager, velopack::Error> {
    let (url, channel) = super::windows_feed::endpoint(beta);
    UpdateManager::new(
        okilum_http::updates::Source::new(&url, env!("OKILUM_RELEASE_VERSION"))
            .map_err(|error| velopack::Error::Other(format!("{error:#}")))?
            .with_cancellation(token),
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
        super::startup_check();
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
                RESTART.cancel();
                super::restart_feedback::clear_pending();
                // A package downloaded for the other channel is checked again
                // before it can be applied (#1318).
                forget_verified();
                state().lock().unwrap().observe(None);
            }
        }
        Err(error) => message(&format!("Could not save update channel: {error}")),
    }
}

pub(super) fn check(download: bool) -> bool {
    if !available() {
        return false;
    }
    use super::status::{self, CheckStatus};
    if BUSY.swap(true, Ordering::AcqRel) {
        return false;
    }
    let download = download || RESTART.requested();
    status::set(CheckStatus::Checking);
    // HTTP and package IO never block the GPUI/UI thread or vault preparation.
    // A manual check reports inline, never in a message box (#995).
    std::thread::spawn(move || {
        let previous = ready_package();
        let result = check_and_download(download);
        // Download/preparation failures stay visible even for background checks.
        super::policy::DOWNLOAD.finish(|cancelled| {
            if cancelled {
                if let Ok(manager) = manager() {
                    if let Some(asset) = manager.get_update_pending_restart() {
                        discard(&asset);
                    }
                }
                state().lock().unwrap().observe(None);
                status::set(CheckStatus::Note(
                    "Update cancelled. Nothing was installed.".into(),
                ));
                super::restart_feedback::offer("Update cancelled. Nothing was installed.");
                return;
            }
            status::set(match result {
                Ok(status) => {
                    if status == CheckStatus::Ready {
                        if previous.is_some() && ready_package() != previous {
                            super::restart_feedback::latest_ready();
                        } else {
                            super::restart_feedback::clear_pending();
                        }
                    }
                    status
                }
                Err(error) => {
                    crate::reader_diagnostics::network_failure("Windows update", &error);
                    let reason = format!(
                        "{}. Your current version is unchanged.",
                        error
                            .to_string()
                            .lines()
                            .next()
                            .unwrap_or("Update failed")
                            .trim_end_matches('.')
                    );
                    if ready()
                        || matches!(
                            status::get(),
                            CheckStatus::Downloading(_) | CheckStatus::Preparing
                        )
                    {
                        super::restart_feedback::refused(reason.clone());
                    }
                    CheckStatus::Failed(reason)
                }
            });
        });
        BUSY.store(false, Ordering::Release);
    });
    true
}

fn check_and_download(download: bool) -> anyhow::Result<super::status::CheckStatus> {
    use super::status::CheckStatus;
    let manager = manager()?;
    // Selection must precede Ready admission: the previous version can remain
    // listed after a newer beta is published (#2706).
    let pending = manager.get_update_pending_restart();
    match manager.check_for_updates()? {
        UpdateCheck::UpdateAvailable(update) => {
            if let Some(asset) = pending.as_ref() {
                let sha256 = package_sha256(asset).ok_or_else(|| {
                    anyhow::anyhow!(
                        "The downloaded update could not be read. Okilum will try again later"
                    )
                })?;
                let record = verified_path().and_then(|p| std::fs::read_to_string(p).ok());
                let offered = &update.TargetFullRelease;
                if asset.PackageId == offered.PackageId
                    && asset.FileName == offered.FileName
                    && super::windows_feed::pending_matches_offer(
                        (&asset.Version, &sha256),
                        (&offered.Version, &offered.SHA256),
                        record.as_deref(),
                        beta(),
                    )
                    .map_err(anyhow::Error::msg)?
                {
                    record_verified(&sha256, offered);
                    state().lock().unwrap().observe(Some(identity(asset)));
                    return Ok(CheckStatus::Ready);
                }
            }
            let replacing = pending.is_some();
            // Retain the file for the SDK's delta base selection, but revoke
            // its restart admission before starting a replacement transfer.
            state().lock().unwrap().observe(None);
            forget_verified();
            use super::status;
            if !download {
                if let Some(asset) = pending.as_ref() {
                    discard(asset);
                    super::restart_feedback::offer(format!(
                        "An update ({}) is available. Download it to update.",
                        update.TargetFullRelease.Version
                    ));
                }
                return Ok(CheckStatus::Available(
                    update.TargetFullRelease.Version.clone(),
                ));
            }
            if replacing {
                super::restart_feedback::notify("downloading", "Downloading the latest update…");
            }
            let token = super::policy::DOWNLOAD.begin();
            let manager = manager_with_token(beta(), token.clone())?;
            status::set(CheckStatus::Downloading(Some(0)));
            let (send, receive) = std::sync::mpsc::channel::<i16>();
            // Join before Ready/Failed so a late progress event cannot replace it.
            std::thread::scope(|scope| {
                let progress = scope.spawn(move || {
                    for percent in receive {
                        if !token.load(Ordering::Acquire) {
                            status::set(CheckStatus::Downloading(
                                Some(percent.clamp(0, 100) as u8),
                            ));
                        }
                    }
                });
                let result = manager.download_updates(&update, Some(send));
                let _ = progress.join();
                result
            })?;
            status::set(CheckStatus::Preparing);
            let pending = manager.get_update_pending_restart();
            anyhow::ensure!(
                pending.as_ref().is_some_and(|asset| {
                    asset.PackageId == update.TargetFullRelease.PackageId
                        && asset.Version == update.TargetFullRelease.Version
                        && asset.FileName == update.TargetFullRelease.FileName
                }),
                "The verified download does not match the selected update. Check for updates again."
            );
            if let Some(sha256) = pending.as_ref().and_then(package_sha256) {
                record_verified(&sha256, &update.TargetFullRelease);
            }
            state()
                .lock()
                .unwrap()
                .observe(pending.as_ref().map(identity));
            anyhow::ensure!(
                pending.is_some(),
                "The downloaded package is no longer available. Check for updates again."
            );
            Ok(CheckStatus::Ready)
        }
        UpdateCheck::NoUpdateAvailable => {
            if let Some(asset) = pending.as_ref() {
                discard(asset);
                state().lock().unwrap().observe(None);
                super::restart_feedback::offer(
                    "The update offer changed. No newer update is currently available.",
                );
            }
            Ok(CheckStatus::UpToDate(std::time::SystemTime::now()))
        }
        UpdateCheck::RemoteIsEmpty => {
            if let Some(asset) = pending.as_ref() {
                discard(asset);
                state().lock().unwrap().observe(None);
                super::restart_feedback::offer(
                    "No release has been published on the selected channel yet.",
                );
            }
            Ok(CheckStatus::Note(
                "No release has been published on the selected channel yet.".into(),
            ))
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

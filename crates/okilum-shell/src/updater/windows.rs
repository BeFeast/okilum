//! Installed Windows releases: policy-controlled download, explicit guarded restart.
//! Installation ID BeFeast.Okilum deliberately differs from Reader state `okilum`.
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Mutex, OnceLock,
};
use velopack::{UpdateCheck, UpdateManager, UpdateOptions};

/// How this copy can update. Not cached once and for all: a helper that needs
/// repair is re-evaluated in this session (#2855 review).
#[derive(Clone, Debug, PartialEq, Eq)]
enum Install {
    Installed,
    Portable,
    /// Update.exe is missing or unusable while its recovery files remain.
    /// Carries a path-free diagnostic.
    Repair(String),
}
static INSTALL: Mutex<Option<(Install, std::time::Instant)>> = Mutex::new(None);
static REPAIRING: AtomicBool = AtomicBool::new(false);
const REPAIR_RETRY: std::time::Duration = std::time::Duration::from_secs(60);
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
        prepared_status(&manager, asset)?;
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
                RestartStep::ArmUpdater => arm_verified(&manager, asset, || {
                    manager.wait_exit_then_apply_updates(
                        asset,
                        false,
                        true,
                        std::mem::take(&mut restart_args),
                    )?;
                    Ok(())
                })?,
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

/// SHA256 of the downloaded package file, upper-case hex like the feed's.
/// Velopack's local pending asset leaves `SHA256` empty, so it is read here,
/// from the directory the package was downloaded to: `<root>\packages`, or the
/// per-user fallback when the install root is not writable.
fn package_sha256(manager: &UpdateManager, asset: &velopack::VelopackAsset) -> Option<String> {
    use sha2::{Digest, Sha256};
    let packages = manager.get_packages_dir();
    let mut file = std::fs::File::open(packages.join(&asset.FileName)).ok()?;
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
fn discard(manager: &UpdateManager, asset: &velopack::VelopackAsset) {
    let _ = std::fs::remove_file(manager.get_packages_dir().join(&asset.FileName));
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

/// A portable copy has no helper transaction. A helper that cannot be located
/// while its recovery files remain needs repair, which is not "Portable".
fn classify(portable: Option<bool>, helper: Option<&str>) -> Install {
    match (portable, helper) {
        (Some(true), _) | (None, None) => Install::Portable,
        (Some(false), None) => Install::Installed,
        // Installed but the helper is missing or its transaction is unresolved:
        // updating cannot work, so say so instead of "Installed" or "Portable".
        (Some(false), Some(detail)) | (None, Some(detail)) => Install::Repair(detail.to_owned()),
    }
}

/// Where Update.exe can live: the install root (per-user installs) and the
/// per-user fallback Velopack uses when the root is not writable.
fn helper_candidates() -> Vec<std::path::PathBuf> {
    let mut candidates = Vec::new();
    if let Some(root) = std::env::current_exe()
        .ok()
        .and_then(|exe| Some(exe.parent()?.parent()?.to_owned()))
    {
        candidates.push(root.join("Update.exe"));
    }
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        let fallback = std::path::PathBuf::from(local)
            .join("BeFeast.Okilum")
            .join("Update.exe");
        if !candidates.contains(&fallback) {
            candidates.push(fallback);
        }
    }
    candidates
}

/// An installed (not portable) layout: `<root>\current\okilum.exe` next to its
/// `sq.version` manifest.
fn installed_layout(exe: &std::path::Path) -> bool {
    exe.parent().is_some_and(|current| {
        current
            .file_name()
            .is_some_and(|name| name.eq_ignore_ascii_case("current"))
            && current.join("sq.version").is_file()
    })
}

/// When the installation cannot be located and no helper has recovery files:
/// an installed layout lost its helper (repair), a portable copy has none.
fn missing_helper_detail(installed_layout: bool) -> Option<&'static str> {
    installed_layout.then_some("update helper missing; reinstall Okilum to restore updates")
}

/// Every cached state is re-evaluated in the background after this long, so a
/// later unrecoverable helper failure is not hidden behind "Installed".
fn refresh_due(_state: &Install, elapsed: std::time::Duration) -> bool {
    elapsed >= REPAIR_RETRY
}

/// Locating the installation already ran helper recovery. A located install
/// still reports a helper that is missing or unresolved; when locating fails,
/// any candidate with recovery files gets one more attempt and keeps its error.
fn evaluate(beta: bool) -> Install {
    if let Ok(manager) = manager_for(beta) {
        let portable = manager.get_is_portable();
        let helper = if portable {
            None
        } else {
            manager.update_helper_state()
        };
        return classify(Some(portable), helper.as_deref());
    }
    for helper in helper_candidates() {
        let Some(pending) = velopack::helper_replace::inspect(&helper) else {
            continue;
        };
        let detail = match velopack::helper_replace::recover(&helper) {
            Ok(()) => match manager_for(beta) {
                Ok(manager) => {
                    let portable = manager.get_is_portable();
                    let helper = (!portable).then(|| manager.update_helper_state()).flatten();
                    return classify(Some(portable), helper.as_deref());
                }
                Err(_) => format!("{pending}; the update helper could not be located"),
            },
            Err(error) => format!("{pending}; last error: {error}"),
        };
        return classify(None, Some(&detail));
    }
    let layout = std::env::current_exe()
        .map(|exe| installed_layout(&exe))
        .unwrap_or(false);
    classify(None, missing_helper_detail(layout))
}

fn install() -> Install {
    let mut cached = INSTALL.lock().unwrap();
    if let Some((state, since)) = cached.as_ref() {
        if refresh_due(state, since.elapsed()) {
            retry_repair_in_background();
        }
        return state.clone();
    }
    let saved = crate::reader_history::state_directory()
        .ok()
        .and_then(|root| std::fs::read_to_string(root.join("windows-update-channel")).ok());
    let beta = super::windows_feed::follows_beta(saved.as_deref());
    BETA.store(beta, Ordering::Relaxed);
    let state = evaluate(beta);
    *cached = Some((state.clone(), std::time::Instant::now()));
    state
}

fn retry_repair_in_background() {
    if REPAIRING.swap(true, Ordering::AcqRel) {
        return;
    }
    std::thread::spawn(|| {
        retry_repair();
        REPAIRING.store(false, Ordering::Release);
    });
}

/// Settings "Retry now": the same evaluation on a worker thread (recovery hashes
/// files and may wait for the helper lock); `then` runs if updating is
/// available again. Returns at once; a retry already running is not repeated.
pub(super) fn retry_repair_then(then: fn()) -> Option<std::thread::JoinHandle<bool>> {
    if REPAIRING.swap(true, Ordering::AcqRel) {
        return None;
    }
    Some(std::thread::spawn(move || {
        let installed = retry_repair();
        REPAIRING.store(false, Ordering::Release);
        if installed {
            then();
        }
        installed
    }))
}

/// Re-run helper recovery now. Returns whether updating is available again.
pub(super) fn retry_repair() -> bool {
    let state = evaluate(BETA.load(Ordering::Relaxed));
    let installed = state == Install::Installed;
    if let Install::Repair(detail) = &state {
        crate::reader_diagnostics::network_failure(
            "Windows update helper",
            &anyhow::anyhow!("needs repair: {detail}"),
        );
    }
    *INSTALL.lock().unwrap() = Some((state, std::time::Instant::now()));
    installed
}

/// Path-free reason why updating is paused, when the helper needs repair.
pub(super) fn repair_needed() -> Option<String> {
    match install() {
        Install::Repair(detail) => Some(detail),
        _ => None,
    }
}

/// Whether this copy is installed and which channel it follows. Read on first
/// use, so the diagnostic line written before `start` already says how Okilum
/// was installed (#1273).
fn settings() -> bool {
    install() == Install::Installed
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
                        discard(&manager, &asset);
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
                    // A failure may come from the helper: re-evaluate now rather
                    // than keep a cached "Installed".
                    retry_repair_in_background();
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

/// Creating Ready requires a live package/helper byte match, independent of
/// feed receipts and cached package presence. Restart repeats this admission.
fn prepared_status(
    manager: &UpdateManager,
    asset: &velopack::VelopackAsset,
) -> anyhow::Result<super::status::CheckStatus> {
    anyhow::ensure!(
        manager.is_update_helper_prepared(asset)?,
        "The update helper is not prepared for this package. Check for updates again."
    );
    Ok(super::status::CheckStatus::Ready)
}

/// Verify and launch under the helper's cross-process transaction lock: no
/// other Okilum process can replace Update.exe between this re-verification
/// and the launch. The running helper keeps its image afterwards.
fn arm_verified(
    manager: &UpdateManager,
    asset: &velopack::VelopackAsset,
    launch: impl FnOnce() -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    let _helper = manager.lock_update_helper().map_err(|error| {
        anyhow::anyhow!(
            "Another Okilum window is preparing an update. Try Restart now again in a moment ({error})"
        )
    })?;
    reverify(manager, asset)?;
    launch()
}

/// The pending package is still this asset and the helper still matches it.
fn reverify(manager: &UpdateManager, asset: &velopack::VelopackAsset) -> anyhow::Result<()> {
    anyhow::ensure!(
        manager
            .get_update_pending_restart()
            .is_some_and(|pending| identity(&pending) == identity(asset)),
        "The downloaded update changed. Check for updates again."
    );
    prepared_status(manager, asset)?;
    Ok(())
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
                let sha256 = package_sha256(&manager, asset).ok_or_else(|| {
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
                    let ready = prepared_status(&manager, asset)?;
                    record_verified(&sha256, offered);
                    state().lock().unwrap().observe(Some(identity(asset)));
                    return Ok(ready);
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
                    discard(&manager, asset);
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
            let ready = prepared_status(
                &manager,
                pending.as_ref().expect("pending identity checked above"),
            )?;
            if let Some(sha256) = pending
                .as_ref()
                .and_then(|asset| package_sha256(&manager, asset))
            {
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
            Ok(ready)
        }
        UpdateCheck::NoUpdateAvailable => {
            if let Some(asset) = pending.as_ref() {
                discard(&manager, asset);
                state().lock().unwrap().observe(None);
                super::restart_feedback::offer(
                    "The update offer changed. No newer update is currently available.",
                );
            }
            Ok(CheckStatus::UpToDate(std::time::SystemTime::now()))
        }
        UpdateCheck::RemoteIsEmpty => {
            if let Some(asset) = pending.as_ref() {
                discard(&manager, asset);
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        io::{Cursor, Write},
        path::{Path, PathBuf},
        sync::mpsc::Sender,
    };
    use velopack::{
        bundle::Manifest, locator::VelopackLocatorConfig, sources::UpdateSource, VelopackAsset,
        VelopackAssetFeed,
    };

    struct Source;
    impl UpdateSource for Source {
        fn get_release_feed(
            &self,
            _: &str,
            _: &Manifest,
            _: &str,
        ) -> Result<VelopackAssetFeed, velopack::Error> {
            unreachable!()
        }
        fn download_release_entry(
            &self,
            _: &VelopackAsset,
            _: &Path,
            _: Option<Sender<i16>>,
        ) -> Result<(), velopack::Error> {
            unreachable!("cached retry must prepare the existing package")
        }
    }

    struct Fixture {
        _root: tempfile::TempDir,
        helper: PathBuf,
        asset: VelopackAsset,
        manager: UpdateManager,
    }
    /// An installed copy at 1.0.0 with a cached 2.0.0 package and a stale helper.
    fn fixture() -> Fixture {
        let root = tempfile::tempdir().unwrap();
        let current = root.path().join("current");
        let packages = root.path().join("packages");
        fs::create_dir(&current).unwrap();
        fs::create_dir(&packages).unwrap();
        let manifest = current.join("sq.version");
        fs::write(&manifest, "<package><metadata><id>ReadyFixture</id><version>1.0.0</version><title>Fixture</title><authors>QA</authors><description>Test</description><mainExe>okilum.exe</mainExe></metadata></package>").unwrap();
        let helper = root.path().join("Update.exe");
        fs::write(&helper, b"stale helper").unwrap();
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        zip.start_file(
            "ReadyFixture.nuspec",
            zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored),
        )
        .unwrap();
        zip.write_all(b"<package><metadata><id>ReadyFixture</id><version>2.0.0</version><title>Fixture</title><authors>QA</authors><description>Test</description><mainExe>okilum.exe</mainExe></metadata></package>").unwrap();
        zip.start_file(
            "lib/app/Squirrel.exe",
            zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored),
        )
        .unwrap();
        zip.write_all(b"prepared helper").unwrap();
        let asset = VelopackAsset {
            PackageId: "ReadyFixture".into(),
            Version: "2.0.0".into(),
            Type: "Full".into(),
            FileName: "ReadyFixture-2.0.0-full.nupkg".into(),
            ..Default::default()
        };
        fs::write(
            packages.join(&asset.FileName),
            zip.finish().unwrap().into_inner(),
        )
        .unwrap();
        let manager = UpdateManager::new(
            Source,
            None,
            Some(VelopackLocatorConfig {
                RootAppDir: root.path().to_owned(),
                UpdateExePath: helper.clone(),
                PackagesDir: packages,
                ManifestPath: manifest,
                CurrentBinaryDir: current,
                IsPortable: true,
            }),
        )
        .unwrap();
        Fixture {
            _root: root,
            helper,
            asset,
            manager,
        }
    }
    fn prepare(f: &Fixture) {
        f.manager
            .download_updates(
                &velopack::UpdateInfo {
                    TargetFullRelease: f.asset.clone(),
                    BaseRelease: None,
                    DeltasToTarget: vec![],
                    IsDowngrade: false,
                },
                None,
            )
            .unwrap();
    }

    #[test]
    fn ready_requires_helper_bytes_for_this_package() {
        let f = fixture();
        assert!(
            prepared_status(&f.manager, &f.asset).is_err(),
            "stale helper must not be Ready"
        );
        prepare(&f);
        assert_eq!(
            prepared_status(&f.manager, &f.asset).unwrap(),
            super::super::status::CheckStatus::Ready
        );
        fs::write(&f.helper, b"prepared helpeX").unwrap();
        assert!(
            prepared_status(&f.manager, &f.asset).is_err(),
            "same-size changed helper must not be Ready"
        );
    }

    #[test]
    fn helper_launch_is_locked_and_reverified_across_processes() {
        let f = fixture();
        prepare(&f);
        let pending = f.manager.get_update_pending_restart().unwrap();
        // Another process holds the helper transaction: no launch.
        let held = velopack::helper_replace::lock(&f.helper).unwrap();
        let mut launched = 0;
        assert!(
            arm_verified(&f.manager, &pending, || {
                launched += 1;
                Ok(())
            })
            .is_err(),
            "launch must wait for the helper lock"
        );
        assert_eq!(launched, 0, "helper launched without the helper lock");
        drop(held);
        // Another process replaced the helper after the earlier verification.
        fs::write(&f.helper, b"prepared helpeX").unwrap();
        assert!(
            arm_verified(&f.manager, &pending, || {
                launched += 1;
                Ok(())
            })
            .is_err(),
            "launch must re-verify the helper under the lock"
        );
        assert_eq!(launched, 0, "changed helper was launched");
        prepare(&f);
        arm_verified(&f.manager, &pending, || {
            launched += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(launched, 1);
    }

    #[test]
    fn package_hash_reads_the_directory_the_package_was_downloaded_to() {
        use sha2::{Digest, Sha256};
        let f = fixture();
        let bytes = fs::read(f.manager.get_packages_dir().join(&f.asset.FileName)).unwrap();
        let expected: String = Sha256::digest(&bytes)
            .iter()
            .map(|byte| format!("{byte:02X}"))
            .collect();
        assert_eq!(
            package_sha256(&f.manager, &f.asset),
            Some(expected),
            "package must be hashed where it was downloaded"
        );
    }

    #[test]
    fn missing_helper_in_an_installed_layout_and_stale_installed_state_reach_repair() {
        let root = tempfile::tempdir().unwrap();
        let current = root.path().join("current");
        fs::create_dir(&current).unwrap();
        fs::write(current.join("sq.version"), "<package/>").unwrap();
        assert!(installed_layout(&current.join("okilum.exe")));
        assert!(!installed_layout(&root.path().join("okilum.exe")));
        assert!(
            missing_helper_detail(true).is_some(),
            "installed layout without a helper must not be Portable"
        );
        assert_eq!(missing_helper_detail(false), None);
        assert!(
            refresh_due(&Install::Installed, REPAIR_RETRY),
            "a cached Installed state must be re-evaluated"
        );
        assert!(!refresh_due(&Install::Installed, std::time::Duration::ZERO));
    }

    #[test]
    fn helper_needing_repair_is_not_reported_as_portable() {
        assert_eq!(classify(Some(false), None), Install::Installed);
        assert_eq!(classify(Some(true), None), Install::Portable);
        assert_eq!(classify(None, None), Install::Portable);
        let detail = "update helper missing; recovery files kept: staged copy; last error: IO error: sharing violation";
        assert_eq!(
            classify(None, Some(detail)),
            Install::Repair(detail.into()),
            "a helper needing repair must not be called Portable"
        );
        // A located machine install whose fallback helper is unresolved.
        assert_eq!(
            classify(Some(false), Some(detail)),
            Install::Repair(detail.into()),
            "an installed copy whose helper needs repair must not be called Installed"
        );
    }
}

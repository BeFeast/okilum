//! AppImage uses the same Settings/About and protected restart flow as Windows.
use super::status::{self, CheckStatus};
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex, OnceLock,
    },
};

static ANNOUNCE: AtomicBool = AtomicBool::new(false);
static BUSY: AtomicBool = AtomicBool::new(false);
static BETA: AtomicBool = AtomicBool::new(false);
static INITIALIZED: OnceLock<()> = OnceLock::new();
static PENDING: Mutex<Option<okilum_appimage::Staged>> = Mutex::new(None);

fn preference() -> anyhow::Result<PathBuf> {
    Ok(crate::reader_history::state_directory()?.join("appimage-update-channel"))
}
pub(super) fn packaged() -> bool {
    std::env::var_os("APPIMAGE").is_some()
}
pub(super) fn beta() -> bool {
    INITIALIZED.get_or_init(|| {
        let saved = preference()
            .ok()
            .and_then(|p| std::fs::read_to_string(p).ok());
        BETA.store(saved.as_deref() == Some("beta"), Ordering::SeqCst);
    });
    BETA.load(Ordering::SeqCst)
}
pub(super) fn set_beta(enabled: bool) {
    // Serialize channel changes with checks/downloads/restart. A stale channel
    // must never replace a newer user choice or apply its pending payload.
    if BUSY
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        status::set(CheckStatus::Note(
            "Wait for the current update operation before changing channels.".into(),
        ));
        return;
    }
    let result = (|| -> anyhow::Result<()> {
        let path = preference()?;
        std::fs::create_dir_all(path.parent().unwrap())?;
        let mut file = tempfile::NamedTempFile::new_in(path.parent().unwrap())?;
        use std::io::Write;
        file.write_all(if enabled { b"beta" } else { b"stable" })?;
        file.as_file().sync_all()?;
        file.persist(path)?;
        beta();
        BETA.store(enabled, Ordering::SeqCst);
        *PENDING.lock().unwrap() = None;
        status::set(CheckStatus::Idle);
        Ok(())
    })();
    if let Err(error) = result {
        status::set(CheckStatus::Failed(error.to_string()));
    }
    BUSY.store(false, Ordering::SeqCst);
}
fn gpgv() -> anyhow::Result<PathBuf> {
    Ok(std::env::current_exe()?
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Missing AppDir"))?
        .join("gpgv"))
}
pub(super) fn take_announcement() -> bool {
    ready() && ANNOUNCE.swap(false, Ordering::SeqCst)
}
pub(super) fn ready() -> bool {
    PENDING.lock().unwrap().is_some()
}
pub(super) fn start() {
    if packaged() && std::env::var_os("OKILUM_APPIMAGE_UPDATE_RECEIPT").is_none() {
        check();
    }
}
pub(super) fn check() {
    if !packaged()
        || BUSY
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
    {
        return;
    }
    status::set(CheckStatus::Checking);
    std::thread::spawn(|| {
        let result = (|| -> anyhow::Result<()> {
            let target = okilum_appimage::Target::detect()?;
            let beta = beta();
            let current = env!("OKILUM_BUILD_VERSION").parse()?;
            let verifier = gpgv()?;
            match okilum_appimage::check(beta, current, &verifier)? {
                Some(release) => {
                    status::set(CheckStatus::Note(format!(
                        "Downloading Okilum {}…",
                        release.version
                    )));
                    let staged =
                        okilum_appimage::Staged::prepare(target, release, beta, &verifier)?;
                    *PENDING.lock().unwrap() = Some(staged);
                    ANNOUNCE.store(true, Ordering::SeqCst);
                    status::set(CheckStatus::Ready);
                }
                None => {
                    status::set(CheckStatus::UpToDate(std::time::SystemTime::now()));
                }
            }
            Ok(())
        })();
        if let Err(error) = result {
            status::set(CheckStatus::Failed(format!(
                "{error}. The original AppImage has been retained."
            )));
        }
        BUSY.store(false, Ordering::SeqCst);
    });
}
pub(super) fn restart(cx: &mut gpui::App) {
    if BUSY
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return;
    }
    let result = (|| -> anyhow::Result<()> {
        let args = prepare_restart(cx)?;
        let staged = PENDING
            .lock()
            .unwrap()
            .take()
            .ok_or_else(|| anyhow::anyhow!("Check for updates again"))?;
        let recovery = staged.arm(&args)?;
        eprintln!("AppImage update recovery directory: {}", recovery.display());
        cx.quit();
        Ok(())
    })();
    if let Err(error) = result {
        status::set(CheckStatus::Failed(format!(
            "Could not restart to update: {error}"
        )));
    }
    BUSY.store(false, Ordering::SeqCst);
}

/// Called after GPUI has an active window, outside rendering/network I/O.
pub(super) fn confirm_startup() {
    static STARTED: AtomicBool = AtomicBool::new(false);
    if std::env::var_os("OKILUM_APPIMAGE_UPDATE_RECEIPT").is_none()
        || STARTED.swap(true, Ordering::SeqCst)
    {
        return;
    }
    std::thread::spawn(|| {
        if let Err(error) = okilum_appimage::confirm_startup(
            env!("OKILUM_BUILD_VERSION"),
            env!("OKILUM_SOURCE_COMMIT"),
        ) {
            status::set(CheckStatus::Failed(format!("Update startup confirmation failed: {error}. Previous AppImage retained beside this file.")));
        }
    });
}

/// Shared by the production Restart action and real-editor regression tests.
pub(crate) fn prepare_restart(cx: &mut gpui::App) -> anyhow::Result<Vec<std::ffi::OsString>> {
    let args = crate::reader_open::update_restart_args(cx)?;
    anyhow::ensure!(
        crate::reader_editor::protect_all_for_quit(cx),
        "Your latest edits could not be protected. Okilum will stay open."
    );
    crate::reader_ui_state::prepare_for_restart(cx)?;
    Ok(args)
}

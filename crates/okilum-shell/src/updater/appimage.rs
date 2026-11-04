//! AppImage uses the same Settings/About and protected restart flow as Windows.
use super::status::{self, CheckStatus};
use okilum_appimage::channel::{self, Channel};
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
};

static ANNOUNCE: AtomicBool = AtomicBool::new(false);
static BUSY: AtomicBool = AtomicBool::new(false);
static STARTUP_CONFIRMED: AtomicBool = AtomicBool::new(false);
static CHANNEL: Mutex<Option<Channel>> = Mutex::new(None);
static PENDING: Mutex<Option<okilum_appimage::Staged>> = Mutex::new(None);

fn preference() -> anyhow::Result<PathBuf> {
    Ok(crate::reader_history::state_directory()?.join("appimage-update-channel"))
}
pub(super) fn packaged() -> bool {
    crate::linux_sprint::allows_updates() && std::env::var_os("APPIMAGE").is_some()
}
pub(super) fn channel() -> &'static str {
    let mut current = CHANNEL.lock().unwrap();
    if current.is_none() {
        *current = preference()
            .ok()
            .and_then(|p| channel::read(&p).ok().flatten());
    }
    current.map_or("Not determined", Channel::label)
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
        let chosen = if enabled {
            Channel::Beta
        } else {
            Channel::Stable
        };
        channel::save(&preference()?, chosen)?;
        *CHANNEL.lock().unwrap() = Some(chosen);
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
    #[cfg(feature = "appimage-acceptance-harness")]
    if crate::appimage_acceptance::active() {
        return;
    }
    if packaged() && std::env::var_os("OKILUM_APPIMAGE_UPDATE_RECEIPT").is_none() {
        super::startup_check();
    }
}
pub(super) fn check(download: bool) -> bool {
    if ready() || !packaged()
        // A focus event can arrive before the receipt writer. Do not recover
        // an update that this process has not yet confirmed (#2387/#2455).
        || (std::env::var_os("OKILUM_APPIMAGE_UPDATE_RECEIPT").is_some()
            && !STARTUP_CONFIRMED.load(Ordering::SeqCst))
        || BUSY
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
    {
        return false;
    }
    status::set(CheckStatus::Checking);
    std::thread::spawn(move || {
        let result = (|| -> anyhow::Result<()> {
            okilum_appimage::recover_interrupted()?;
            let target = okilum_appimage::Target::detect()?;
            let current = env!("OKILUM_BUILD_VERSION").parse()?;
            let verifier = gpgv()?;
            let chosen = channel::resolve(
                &preference()?,
                current,
                env!("OKILUM_SOURCE_COMMIT"),
                &verifier,
            )?;
            *CHANNEL.lock().unwrap() = Some(chosen);
            // Refresh About/Settings as soon as discovery finishes, even if
            // the subsequent update feed request is slow or unavailable.
            status::set(CheckStatus::Checking);
            let beta = chosen == Channel::Beta;
            match okilum_appimage::check(beta, current, &verifier)? {
                Some(release) => {
                    if !download {
                        status::set(CheckStatus::Available(release.version.clone()));
                        return Ok(());
                    }
                    let token = super::policy::DOWNLOAD.begin();
                    status::set(CheckStatus::Downloading(Some(0)));
                    let staged = okilum_appimage::Staged::prepare_cancellable(
                        target,
                        release,
                        beta,
                        &verifier,
                        &token,
                        |percent| {
                            if !token.load(Ordering::Acquire) {
                                status::set(CheckStatus::Downloading(Some(percent)));
                            }
                        },
                    );
                    super::policy::DOWNLOAD.finish(|cancelled| {
                        if cancelled {
                            // Drop the unarmed stage before releasing BUSY.
                            drop(staged);
                            status::set(CheckStatus::Note(
                                "Update cancelled. Nothing was installed.".into(),
                            ));
                        } else {
                            match staged {
                                Ok(staged) => {
                                    *PENDING.lock().unwrap() = Some(staged);
                                    ANNOUNCE.store(true, Ordering::SeqCst);
                                    status::set(CheckStatus::Ready);
                                }
                                Err(error) => status::set(CheckStatus::Failed(format!(
                                    "{error}. The original AppImage has been retained."
                                ))),
                            }
                        }
                    });
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
    true
}
pub(super) fn restart(cx: &mut gpui::App) {
    super::restart_feedback::attempt();
    if !crate::linux_sprint::allows_updates() {
        super::restart_feedback::refused(
            "Updates are disabled for this Sprint. Open the installed AppImage to update.",
        );
        return;
    }
    if BUSY
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        super::restart_feedback::refused(
            "The updater is busy. Wait for it to finish, then choose Restart now again.",
        );
        return;
    }
    let result = (|| -> anyhow::Result<()> {
        #[allow(unused_mut)]
        let mut args = prepare_restart(cx)?;
        #[cfg(feature = "appimage-acceptance-harness")]
        crate::appimage_acceptance::restart_arguments(&mut args);
        let staged = PENDING
            .lock()
            .unwrap()
            .take()
            .ok_or_else(|| anyhow::anyhow!("Check for updates again"))?;
        let recovery = staged.arm(&args)?;
        eprintln!("AppImage update recovery directory: {}", recovery.display());
        super::restart_feedback::record(
            "accepted",
            "AppImage armed after editor protection and state persistence",
        );
        cx.quit();
        Ok(())
    })();
    if let Err(error) = result {
        super::restart_feedback::refused(format!("Could not restart to update: {error}. Save or close the affected note, then try again."));
        let reason = format!("Could not restart to update: {error}");
        status::set(if ready() {
            CheckStatus::RestartRetry(reason)
        } else {
            CheckStatus::Failed(reason)
        });
    }
    BUSY.store(false, Ordering::SeqCst);
}

/// Called after GPUI has a window, without requiring focus, outside rendering.
pub(super) fn confirm_startup() {
    if !crate::linux_sprint::allows_updates() {
        return;
    }
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
            return;
        }
        // The managed install now holds this build: an older launcher copy
        // must not replace it on the next ordinary launch (#2455).
        if let (Some(home), Some(image), Ok(build)) = (
            std::env::var_os("HOME"),
            std::env::var_os("APPIMAGE"),
            env!("OKILUM_BUILD_VERSION").parse::<u64>(),
        ) {
            if let Err(error) = okilum_appimage::installation::record_running(
                std::path::Path::new(&home),
                std::path::Path::new(&image),
                build,
            ) {
                eprintln!("AppImage installation record: {error:#}");
            }
        }
        STARTUP_CONFIRMED.store(true, Ordering::SeqCst);
    });
}

/// Shared by the production Restart action and real-editor regression tests.
pub(crate) fn prepare_restart(cx: &mut gpui::App) -> anyhow::Result<Vec<std::ffi::OsString>> {
    crate::reader_update_restart::prepare(cx)
}

#[cfg(all(test, not(feature = "linux-sprint")))]
mod restart_tests {
    use super::*;

    #[gpui::test]
    fn busy_restart_reports_a_retry_reason_without_arming_or_quitting(
        cx: &mut gpui::TestAppContext,
    ) {
        let _guard = super::super::restart_feedback::TEST_LOCK.lock().unwrap();
        cx.update(crate::test_init);
        assert!(!BUSY.swap(true, Ordering::SeqCst));
        cx.update(restart);
        BUSY.store(false, Ordering::SeqCst);
        assert!(super::super::restart_feedback::pending_message()
            .is_some_and(|reason| reason.contains("busy") && reason.contains("Restart now again")));
        assert!(!ready());
    }
}

#[cfg(feature = "appimage-acceptance-harness")]
pub(crate) fn acceptance_stage(staged: okilum_appimage::Staged) {
    *PENDING.lock().unwrap() = Some(staged);
    status::set(CheckStatus::Ready);
}

#[cfg(all(test, feature = "linux-sprint"))]
mod sprint_tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[test]
    fn sprint_distribution_refuses_before_engine_or_status_work() {
        let generation = status::generation();
        assert!(!packaged());
        assert!(!crate::updater::available());
        start();
        check(true);
        confirm_startup();
        assert_eq!(status::generation(), generation);
        assert!(!ready());
        assert_eq!(crate::updater::channel(), "Sprint");
    }
}

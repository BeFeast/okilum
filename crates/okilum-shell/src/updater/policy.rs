//! App-wide update preference and the download cancellation/Ready boundary.
//! Durable preference lives beside application state, never in a vault/index.
use std::{
    io::Write as _,
    path::Path,
    sync::{Mutex, OnceLock},
};

#[cfg(any(target_os = "linux", windows, test))]
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Mode {
    #[default]
    Background,
    Ask,
    Off,
}
impl Mode {
    pub(crate) const ALL: [Self; 3] = [Self::Background, Self::Ask, Self::Off];
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Background => "Download in the background, install on click",
            Self::Ask => "Ask before downloading",
            Self::Off => "Off · manual check only",
        }
    }
    fn value(self) -> &'static str {
        match self {
            Self::Background => "background",
            Self::Ask => "ask",
            Self::Off => "off",
        }
    }
    pub(super) fn automatic_checks(self) -> bool {
        self != Self::Off
    }
    pub(super) fn download(self, explicit: bool) -> bool {
        explicit || self == Self::Background
    }
    fn read(path: &Path) -> Self {
        match std::fs::read_to_string(path).ok().as_deref().map(str::trim) {
            Some("ask") => Self::Ask,
            Some("off") => Self::Off,
            // Missing/legacy preferences migrate to the requested default.
            _ => Self::Background,
        }
    }
    fn save(self, path: &Path) -> anyhow::Result<()> {
        let parent = path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("Missing settings directory"))?;
        std::fs::create_dir_all(parent)?;
        let mut file = tempfile::NamedTempFile::new_in(parent)?;
        file.write_all(self.value().as_bytes())?;
        file.as_file().sync_all()?;
        file.persist(path)?;
        Ok(())
    }
}
fn preference() -> &'static Mutex<Mode> {
    static MODE: OnceLock<Mutex<Mode>> = OnceLock::new();
    MODE.get_or_init(|| {
        Mutex::new(
            crate::reader_history::state_directory()
                .ok()
                .map(|p| Mode::read(&p.join("update-mode")))
                .unwrap_or_default(),
        )
    })
}
pub(crate) fn mode() -> Mode {
    *preference().lock().unwrap()
}
pub(super) fn set_mode(mode: Mode) -> anyhow::Result<()> {
    let mut current = preference().lock().unwrap();
    mode.save(&crate::reader_history::state_directory()?.join("update-mode"))?;
    *current = mode;
    Ok(())
}

/// A cancelled transfer keeps its single-flight claim until its worker exits.
/// Final publication and cancellation use the same lock, so a late cancel
/// cannot leave a cancelled package marked Ready (or clobber a completed one).
#[cfg(any(target_os = "linux", windows, test))]
#[derive(Default)]
pub(super) struct Download(Mutex<Option<Arc<AtomicBool>>>);
#[cfg(any(target_os = "linux", windows, test))]
impl Download {
    pub(super) fn begin(&self) -> Arc<AtomicBool> {
        let token = Arc::new(AtomicBool::new(false));
        *self.0.lock().unwrap() = Some(token.clone());
        token
    }
    pub(super) fn active(&self) -> bool {
        self.0
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|t| !t.load(Ordering::Acquire))
    }
    pub(super) fn cancel(&self) {
        if let Some(token) = self.0.lock().unwrap().as_ref() {
            token.store(true, Ordering::Release);
        }
    }
    pub(super) fn finish(&self, publish: impl FnOnce(bool)) {
        let mut active = self.0.lock().unwrap();
        let cancelled = active.as_ref().is_some_and(|t| t.load(Ordering::Acquire));
        publish(cancelled);
        *active = None;
    }
}
#[cfg(any(target_os = "linux", windows))]
pub(super) static DOWNLOAD: Download = Download(Mutex::new(None));

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    #[test]
    fn preference_migrates_and_round_trips_every_mode() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mode");
        assert_eq!(Mode::read(&path), Mode::Background);
        std::fs::write(&path, "legacy").unwrap();
        assert_eq!(Mode::read(&path), Mode::Background);
        for mode in Mode::ALL {
            mode.save(&path).unwrap();
            assert_eq!(Mode::read(&path), mode);
        }
    }
    #[test]
    fn cancellation_cannot_publish_ready_and_next_transfer_starts_clean() {
        let download = Download::default();
        let token = download.begin();
        assert!(download.active());
        download.cancel();
        assert!(token.load(Ordering::Acquire));
        download.finish(|cancelled| assert!(cancelled));
        assert!(!download.active());
        let next = download.begin();
        assert!(!next.load(Ordering::Acquire));
        download.finish(|cancelled| assert!(!cancelled));
        download.cancel(); // completed Ready is retained
        assert!(!next.load(Ordering::Acquire));
    }
    struct FakeFeed {
        checks: usize,
        downloads: usize,
        installs: usize,
        status: super::super::status::CheckStatus,
    }
    impl FakeFeed {
        fn new() -> Self {
            Self {
                checks: 0,
                downloads: 0,
                installs: 0,
                status: super::super::status::CheckStatus::Idle,
            }
        }
        fn check(&mut self, download: bool) -> bool {
            use super::super::status::CheckStatus;
            self.checks += 1;
            if download {
                self.downloads += 1;
                self.status = CheckStatus::Ready;
            } else {
                self.status = CheckStatus::Available("0.1.9999".into());
            }
            true
        }
    }
    fn exercise(mode: Mode) -> FakeFeed {
        use super::super::scheduler::{Schedule, Trigger};
        use std::time::Duration;
        let period = Duration::from_secs(30 * 60);
        let mut schedule = Schedule::new(Duration::ZERO, period);
        let mut feed = FakeFeed::new();
        // Startup/focus and due timer go through exactly the shared policy gate.
        for (now, trigger) in [
            (Duration::ZERO, Trigger::Focus),
            (period, Trigger::Interval),
        ] {
            let started = schedule.run(now, trigger, mode.automatic_checks(), period, || {
                feed.check(mode.download(false))
            });
            assert_eq!(started, mode != Mode::Off);
        }
        assert_eq!(feed.checks, if mode == Mode::Off { 0 } else { 2 });
        // Positive control for Off: a real manual check must reach this feed.
        assert!(schedule.run(period, Trigger::Manual, true, period, || feed
            .check(mode.download(false))));
        assert_eq!(feed.checks, if mode == Mode::Off { 1 } else { 3 });
        assert_eq!(feed.installs, 0, "discovery/download never installs");
        feed
    }
    #[test]
    fn background_mode_checks_and_downloads_to_ready_without_installation() {
        use super::super::status::{self, CheckStatus};
        let feed = exercise(Mode::Background);
        assert_eq!(feed.downloads, 3);
        assert_eq!(feed.status, CheckStatus::Ready);
        assert!(status::progress(&feed.status).unwrap().restart);
    }
    #[test]
    fn ask_mode_checks_and_offers_version_before_downloading() {
        assert_offer_then_click(Mode::Ask);
    }
    #[test]
    fn off_mode_ignores_startup_focus_and_timer_but_manual_download_works() {
        assert_offer_then_click(Mode::Off);
    }
    fn assert_offer_then_click(mode: Mode) {
        use super::super::status::{self, CheckStatus};
        let mut feed = exercise(mode);
        assert_eq!(feed.downloads, 0);
        assert_eq!(feed.status, CheckStatus::Available("0.1.9999".into()));
        assert!(
            status::text(&feed.status, std::time::SystemTime::UNIX_EPOCH)
                .unwrap()
                .contains("download?")
        );
        assert!(status::progress(&feed.status).is_none());
        feed.check(mode.download(true)); // explicit Download, still no install
        assert_eq!(feed.downloads, 1);
        assert_eq!(feed.status, CheckStatus::Ready);
        assert_eq!(feed.installs, 0);
    }
}

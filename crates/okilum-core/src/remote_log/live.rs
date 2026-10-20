//! One owned session, one in-flight request, and full-window PIT reconciliation.
//! A timestamp/search_after cursor is scoped to ONE snapshot, never a live watermark.
use super::{
    discover::TimeRange,
    transport::{ApiKey, History, Incomplete, Kibana, Transient},
    EventWindow, Source,
};
use anyhow::{ensure, Context, Result};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Condvar, Mutex,
    },
    time::{Duration, Instant},
};
use time::OffsetDateTime;

#[derive(Clone, Debug)]
pub enum Mode {
    Fixed(TimeRange),
    Rolling { nanos: i128 },
}
impl Mode {
    pub fn rolling_same_duration(range: &TimeRange) -> Result<Self> {
        let range = range.resolve(OffsetDateTime::now_utc())?;
        let parse =
            |s: &str| OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339);
        let nanos = (parse(&range.to)? - parse(&range.from)?).whole_nanoseconds();
        ensure!(nanos > 0, "The rolling range must have a positive duration");
        Ok(Self::Rolling { nanos })
    }
    pub fn range(&self, now: OffsetDateTime) -> Result<TimeRange> {
        match self {
            Self::Fixed(range) => {
                ensure!(
                    range.absolute(),
                    "Fixed following needs absolute timestamps"
                );
                range.resolve(now)
            }
            Self::Rolling { nanos } => {
                ensure!(
                    *nanos > 0,
                    "The rolling range must have a positive duration"
                );
                let from = now
                    .unix_timestamp_nanos()
                    .checked_sub(*nanos)
                    .context("Rolling range is too large")?;
                Ok(TimeRange {
                    from: OffsetDateTime::from_unix_timestamp_nanos(from)?
                        .format(&time::format_description::well_known::Rfc3339)?,
                    to: now.format(&time::format_description::well_known::Rfc3339)?,
                })
            }
        }
    }
}

pub struct Loaded {
    pub window: Arc<EventWindow>,
    pub source: Source,
    pub range: TimeRange,
    pub more: bool,
}
#[derive(Clone, Debug)]
pub enum Status {
    History {
        complete: bool,
        limited: bool,
        warning: Option<String>,
    },
    CatchingUp {
        range: TimeRange,
        pages: usize,
        scanned: usize,
    },
    Complete {
        range: TimeRange,
        began: String,
        seconds: f64,
        pages: usize,
    },
    Reconnecting {
        attempt: u32,
        after_seconds: u64,
        error: String,
    },
    Incomplete {
        range: Option<TimeRange>,
        scanned: usize,
        reason: String,
    },
    Error(String),
    Stopped {
        warning: Option<String>,
    },
}
pub struct Update {
    pub generation: u64,
    pub following: bool,
    /// Always includes the last successful snapshot, including during catch-up/errors.
    pub loaded: Option<Arc<Loaded>>,
    pub status: Status,
}

enum Command {
    Apply(Box<Source>, ApiKey),
    More,
    Start(Mode, Box<Source>),
    Stop,
}
struct Mailbox {
    generation: u64,
    pending: Option<Command>,
    cancel: Arc<AtomicBool>,
    closing: bool,
}
type Shared = Arc<(Mutex<Mailbox>, Condvar)>;
pub struct Worker {
    shared: Shared,
}
impl Drop for Worker {
    fn drop(&mut self) {
        let (lock, wake) = &*self.shared;
        let mut control = lock.lock().unwrap();
        control.closing = true;
        control.pending = None;
        control.cancel.store(true, Ordering::Release);
        wake.notify_one();
    }
}
#[derive(Clone)]
struct Timing {
    poll: Duration,
    retry: Duration,
    cycle: Duration,
    idle: Duration,
    #[cfg(test)]
    after_page: Option<Arc<dyn Fn(usize) + Send + Sync>>,
}
impl Default for Timing {
    fn default() -> Self {
        Self {
            poll: Duration::from_secs(2),
            retry: Duration::from_secs(2),
            cycle: Duration::from_secs(60),
            idle: Duration::from_secs(45),
            #[cfg(test)]
            after_page: None,
        }
    }
}
impl Worker {
    pub fn start() -> (Self, async_channel::Receiver<Update>) {
        Self::spawn(Timing::default(), None)
    }
    pub fn generation(&self) -> u64 {
        self.shared.0.lock().unwrap().generation
    }
    fn submit(&self, command: Command) -> u64 {
        let (lock, wake) = &*self.shared;
        let mut control = lock.lock().unwrap();
        control.cancel.store(true, Ordering::Release);
        control.cancel = Arc::new(AtomicBool::new(false));
        control.generation += 1;
        control.pending = Some(command); // bounded latest-command mailbox, including rapid Apply.
        wake.notify_one();
        control.generation
    }
    pub fn apply(&self, source: Source, key: ApiKey) -> u64 {
        self.submit(Command::Apply(Box::new(source), key))
    }
    pub fn more(&self) -> u64 {
        self.submit(Command::More)
    }
    pub fn start_live(&self, mode: Mode, expected: &Source) -> u64 {
        self.submit(Command::Start(mode, Box::new(expected.clone())))
    }
    pub fn stop(&self) -> u64 {
        self.submit(Command::Stop)
    }
    fn spawn(
        timing: Timing,
        endpoint: Option<url::Url>,
    ) -> (Self, async_channel::Receiver<Update>) {
        let shared = Arc::new((
            Mutex::new(Mailbox {
                generation: 0,
                pending: None,
                cancel: Arc::new(AtomicBool::new(false)),
                closing: false,
            }),
            Condvar::new(),
        ));
        let (sender, receiver) = async_channel::bounded(1);
        let control = shared.clone();
        std::thread::spawn(move || Engine::new(timing, endpoint).run(control, sender));
        (Self { shared }, receiver)
    }
}
struct Applied {
    source: Source,
    client: Kibana,
    history: Option<History>,
    events: Vec<Arc<serde_json::Value>>,
}
struct Engine {
    timing: Timing,
    endpoint: Option<url::Url>,
    applied: Option<Applied>,
    loaded: Option<Arc<Loaded>>,
    mode: Option<Mode>,
    due: Instant,
    failures: u32,
}
impl Engine {
    fn new(timing: Timing, endpoint: Option<url::Url>) -> Self {
        Self {
            timing,
            endpoint,
            applied: None,
            loaded: None,
            mode: None,
            due: Instant::now(),
            failures: 0,
        }
    }
    fn close_history(&mut self) -> Option<String> {
        let applied = self.applied.as_mut()?;
        let result = applied.history.as_mut()?.close();
        match result {
            Ok(()) => {
                applied.history = None;
                None
            }
            Err(error) => Some(error.to_string()),
        }
    }
    fn emit(
        &self,
        shared: &Shared,
        sender: &async_channel::Sender<Update>,
        generation: u64,
        status: Status,
    ) {
        if shared.0.lock().unwrap().generation == generation {
            // Latest coherent state replaces a pending notification; no UI consumer
            // can cause unbounded status queues or prevent Stop/close cleanup.
            let _ = sender.force_send(Update {
                generation,
                following: self.mode.is_some(),
                loaded: self.loaded.clone(),
                status,
            });
        }
    }
    fn run(mut self, shared: Shared, sender: async_channel::Sender<Update>) {
        loop {
            let (generation, cancel, command) = {
                let (lock, wake) = &*shared;
                let mut control = lock.lock().unwrap();
                loop {
                    if control.closing {
                        drop(control);
                        self.close_history();
                        return;
                    }
                    if let Some(command) = control.pending.take() {
                        break (control.generation, control.cancel.clone(), Some(command));
                    }
                    let now = Instant::now();
                    if now >= self.due {
                        break (control.generation, control.cancel.clone(), None);
                    }
                    control = wake.wait_timeout(control, self.due - now).unwrap().0;
                }
            };
            let status = match command {
                Some(Command::Stop) => {
                    self.mode = None;
                    Status::Stopped {
                        warning: self.close_history(),
                    }
                }
                Some(Command::Apply(source, key)) => {
                    self.mode = None;
                    let cleanup = self.close_history();
                    let result = if let Some(reason) = &cleanup {
                        Err(anyhow::anyhow!("Previous resource cleanup unconfirmed: {reason}; retry Stop before opening another snapshot"))
                    } else {
                        self.apply(*source, key, cancel.clone(), &shared, generation)
                    };
                    match result {
                        Ok(mut status) => {
                            if let Status::History { warning, .. } = &mut status {
                                if warning.is_none() {
                                    *warning = cleanup;
                                }
                            }
                            status
                        }
                        Err(error) => Status::Error(format!("{error:#}")),
                    }
                }
                Some(Command::More) => {
                    self.mode = None;
                    match self.more(cancel.clone(), &shared, generation) {
                        Ok(status) => status,
                        Err(e) => Status::Error(format!("{e:#}")),
                    }
                }
                Some(Command::Start(mode, expected)) => {
                    self.mode = None;
                    if !self.applied.as_ref().is_some_and(|a| {
                        a.source.id == expected.id
                            && a.source.endpoint == expected.endpoint
                            && a.source.discover == expected.discover
                    }) {
                        Status::Error("The applied query changed before Start; Apply it again before following".into())
                    } else {
                        self.mode = Some(mode);
                        self.failures = 0;
                        self.reconcile(&shared, &sender, generation, cancel.clone())
                    }
                }
                None if self.mode.is_some() => {
                    self.reconcile(&shared, &sender, generation, cancel.clone())
                }
                None => Status::Stopped {
                    warning: self.close_history(),
                },
            };
            if self.mode.is_none() {
                self.due = Instant::now() + self.timing.idle;
            }
            if !cancel.load(Ordering::Acquire) {
                self.emit(&shared, &sender, generation, status);
            }
        }
    }
    fn apply(
        &mut self,
        source: Source,
        key: ApiKey,
        cancel: Arc<AtomicBool>,
        shared: &Shared,
        generation: u64,
    ) -> Result<Status> {
        source.validate()?;
        let client = match &self.endpoint {
            Some(endpoint) => Kibana::with_endpoint(endpoint.clone(), key, cancel)?,
            None => Kibana::new(&source, key, cancel)?,
        };
        let mut history = History::open(client.clone(), source.discover.clone())?;
        let page = history.page()?;
        let events: Vec<_> = page.events.into_iter().map(Arc::new).collect();
        let window = Arc::new(EventWindow::new(events.clone(), &source)?);
        let current = shared.0.lock().unwrap();
        ensure!(
            current.generation == generation && !current.closing,
            "Log request cancelled"
        );
        self.loaded = Some(Arc::new(Loaded {
            window,
            source: source.clone(),
            range: page.range,
            more: history.can_page(),
        }));
        self.applied = Some(Applied {
            source,
            client,
            history: Some(history),
            events,
        });
        Ok(Status::History {
            complete: page.complete,
            limited: page.limited,
            warning: page.cleanup_warning,
        })
    }
    fn more(
        &mut self,
        cancel: Arc<AtomicBool>,
        shared: &Shared,
        generation: u64,
    ) -> Result<Status> {
        let a = self.applied.as_mut().context("Apply history first")?;
        let history = a
            .history
            .as_mut()
            .context("Snapshot expired or stopped; Apply to refresh")?;
        history.set_cancellation(cancel);
        let page = history.page()?;
        let mut events = a.events.clone();
        events.extend(page.events.into_iter().map(Arc::new));
        let window = match EventWindow::new(events.clone(), &a.source) {
            Ok(window) => Arc::new(window),
            Err(e) => {
                let _ = history.close();
                return Err(e);
            }
        };
        let current = shared.0.lock().unwrap();
        ensure!(
            current.generation == generation && !current.closing,
            "Log request cancelled"
        );
        a.events = events;
        self.loaded = Some(Arc::new(Loaded {
            window,
            source: a.source.clone(),
            range: page.range,
            more: history.can_page(),
        }));
        Ok(Status::History {
            complete: page.complete,
            limited: page.limited,
            warning: page.cleanup_warning,
        })
    }
    fn reconcile(
        &mut self,
        shared: &Shared,
        sender: &async_channel::Sender<Update>,
        generation: u64,
        cancel: Arc<AtomicBool>,
    ) -> Status {
        let result = self.scan(shared, sender, generation, cancel.clone());
        match result {
            Ok(status @ Status::Complete { .. }) => {
                self.failures = 0;
                self.due = Instant::now() + self.timing.poll;
                status
            }
            Ok(status) => {
                self.mode = None;
                status
            }
            Err(error) if error.is::<Transient>() && !cancel.load(Ordering::Acquire) => {
                self.failures = self.failures.saturating_add(1);
                let delay = self
                    .timing
                    .retry
                    .saturating_mul(1u32 << self.failures.min(4).saturating_sub(1))
                    .min(Duration::from_secs(30));
                self.due = Instant::now() + delay;
                Status::Reconnecting {
                    attempt: self.failures,
                    after_seconds: delay.as_secs(),
                    error: format!("{error:#}"),
                }
            }
            Err(error) => {
                self.mode = None;
                Status::Error(format!("{error:#}"))
            }
        }
    }
    fn abandon_cycle(&mut self, mut history: History, range: TimeRange, scanned: usize) -> Status {
        let cleanup = history.close();
        if history.has_resources() {
            self.applied.as_mut().unwrap().history = Some(history);
        }
        Status::Incomplete { range: Some(range), scanned, reason: format!("Full-window cycle exceeded {:.2}s; previous snapshot retained. Narrow the range. Cleanup: {}", self.timing.cycle.as_secs_f64(), if cleanup.is_ok() { "confirmed" } else { "unconfirmed" }) }
    }
    fn scan(
        &mut self,
        shared: &Shared,
        sender: &async_channel::Sender<Update>,
        generation: u64,
        cancel: Arc<AtomicBool>,
    ) -> Result<Status> {
        if self.close_history().is_some() {
            return Err(anyhow::anyhow!(Transient("Reconnecting: previous PIT cleanup is unconfirmed; retrying cleanup before any new snapshot")));
        }
        let now = OffsetDateTime::now_utc();
        let range = self.mode.as_ref().context("Live is stopped")?.range(now)?;
        let began = now.format(&time::format_description::well_known::Rfc3339)?;
        let clock = Instant::now();
        let applied = self.applied.as_ref().context("Apply history first")?;
        let source = applied.source.clone();
        let client = applied.client.with_cancellation(cancel.clone());
        let mut query = source.discover.clone();
        query.time = range.clone();
        self.emit(
            shared,
            sender,
            generation,
            Status::CatchingUp {
                range: range.clone(),
                pages: 0,
                scanned: 0,
            },
        );
        let mut history = match History::open(client, query) {
            Ok(history) => history,
            Err(error) if error.is::<Incomplete>() => {
                return Ok(Status::Incomplete {
                    range: Some(range),
                    scanned: 0,
                    reason: format!("{error:#}"),
                })
            }
            Err(error) => return Err(error),
        };
        let mut events = Vec::new();
        let mut pages = 0;
        loop {
            if cancel.load(Ordering::Acquire) {
                // The pending Stop/Apply owns cleanup, including retry after a
                // temporary outage; do not lose known handles through Drop.
                self.applied.as_mut().unwrap().history = Some(history);
                anyhow::bail!("Log request cancelled");
            }
            if clock.elapsed() >= self.timing.cycle {
                return Ok(self.abandon_cycle(history, range, events.len()));
            }
            let page = match history.page() {
                Ok(page) => page,
                Err(error) => {
                    if history.has_resources() {
                        self.applied.as_mut().unwrap().history = Some(history);
                    }
                    if error.is::<Incomplete>() {
                        return Ok(Status::Incomplete {
                            range: Some(range),
                            scanned: events.len(),
                            reason: format!("{error:#}; previous snapshot retained"),
                        });
                    }
                    return Err(error);
                }
            };
            pages += 1;
            events.extend(page.events.into_iter().map(Arc::new));
            #[cfg(test)]
            if let Some(hook) = &self.timing.after_page {
                hook(pages);
            }
            if clock.elapsed() >= self.timing.cycle {
                return Ok(self.abandon_cycle(history, range, events.len()));
            }
            if page.limited || page.cleanup_warning.is_some() {
                if history.has_resources() {
                    self.applied.as_mut().unwrap().history = Some(history);
                }
                return Ok(Status::Incomplete { range: Some(range), scanned: events.len(), reason: page.cleanup_warning.unwrap_or_else(|| "Full window exceeds 10,000 events / 16 MiB. Following stopped; older pages are NOT covered. Narrow the range. Previous snapshot retained.".into()) });
            }
            if page.complete {
                ensure!(!cancel.load(Ordering::Acquire), "Log request cancelled");
                let window = Arc::new(EventWindow::new(events.clone(), &source)?);
                if clock.elapsed() >= self.timing.cycle {
                    return Ok(self.abandon_cycle(history, range, events.len()));
                }
                let current = shared.0.lock().unwrap();
                ensure!(
                    current.generation == generation && !current.closing,
                    "Log request cancelled"
                );
                self.loaded = Some(Arc::new(Loaded {
                    window,
                    source,
                    range: range.clone(),
                    more: false,
                }));
                // No accumulation across cycles: current window atomically replaces
                // the old snapshot only after the complete PIT traversal.
                if let Some(a) = self.applied.as_mut() {
                    a.events = events;
                }
                return Ok(Status::Complete {
                    range,
                    began,
                    seconds: clock.elapsed().as_secs_f64(),
                    pages,
                });
            }
            self.emit(
                shared,
                sender,
                generation,
                Status::CatchingUp {
                    range: range.clone(),
                    pages,
                    scanned: events.len(),
                },
            );
        }
    }
}

#[cfg(test)]
#[path = "live_tests.rs"]
mod tests;

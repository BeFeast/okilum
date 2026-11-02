//! One owned session, one in-flight request, and bounded full-window reconciliation.
//! A timestamp/search_after cursor is scoped to ONE traversal, never a live watermark.
use super::{
    discover::TimeRange,
    result::{AuthError, IndexPatternRequired, ResultStatus, SearchTimeout},
    transport::{ApiKey, History, Kibana, Transient},
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
    pub quality: ResultStatus,
}
#[derive(Clone, Debug)]
pub enum Status {
    History {
        quality: ResultStatus,
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
        quality: ResultStatus,
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
    TimedOut,
    AuthError(AuthError),
    IndexPatternRequired,
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
    ReplaceKey(ApiKey),
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
    pub fn replace_key(&self, key: ApiKey) -> u64 {
        self.submit(Command::ReplaceKey(key))
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
    retry_source: Option<Source>,
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
            retry_source: None,
            loaded: None,
            mode: None,
            due: Instant::now(),
            failures: 0,
        }
    }
    fn close_history(&mut self) {
        if let Some(applied) = self.applied.as_mut() {
            applied.history = None;
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
            let _ = sender.force_send(Update {
                generation,
                following: self.mode.is_some(),
                loaded: self.loaded.clone(),
                status,
            });
        }
    }
    fn error(error: anyhow::Error) -> Status {
        if let Some(auth) = error.downcast_ref::<AuthError>() {
            Status::AuthError(auth.clone())
        } else if error.is::<SearchTimeout>() {
            Status::TimedOut
        } else if error.is::<IndexPatternRequired>() {
            Status::IndexPatternRequired
        } else {
            Status::Error(format!("{error:#}"))
        }
    }
    fn run(mut self, shared: Shared, sender: async_channel::Sender<Update>) {
        loop {
            let (generation, cancel, command) = {
                let (lock, wake) = &*shared;
                let mut control = lock.lock().unwrap();
                loop {
                    if control.closing {
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
                    self.close_history();
                    Status::Stopped { warning: None }
                }
                Some(Command::Apply(source, key)) => {
                    self.mode = None;
                    self.close_history();
                    self.apply(*source, key, cancel.clone(), &shared, generation, false)
                        .unwrap_or_else(Self::error)
                }
                Some(Command::ReplaceKey(key)) => {
                    self.mode = None;
                    self.close_history();
                    match self
                        .retry_source
                        .clone()
                        .or_else(|| self.applied.as_ref().map(|a| a.source.clone()))
                    {
                        Some(source) => self
                            .apply(source, key, cancel.clone(), &shared, generation, true)
                            .unwrap_or_else(Self::error),
                        None => Status::Error("Apply a query before replacing its key".into()),
                    }
                }
                Some(Command::More) => {
                    self.mode = None;
                    self.more(cancel.clone(), &shared, generation)
                        .unwrap_or_else(Self::error)
                }
                Some(Command::Start(mode, expected)) => {
                    self.mode = None;
                    if !self.applied.as_ref().is_some_and(|a| {
                        a.source.id == expected.id
                            && a.source.endpoint == expected.endpoint
                            && a.source.discover == expected.discover
                            && a.source.index_pattern == expected.index_pattern
                            && a.source.time_field == expected.time_field
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
                None => {
                    self.close_history();
                    Status::Stopped { warning: None }
                }
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
        replacing_key: bool,
    ) -> Result<Status> {
        source.validate()?;
        self.retry_source = Some(source.clone());
        let client = match &self.endpoint {
            Some(endpoint) => Kibana::with_endpoint(endpoint.clone(), key, cancel)?,
            None => Kibana::new(&source, key, cancel)?,
        };
        let source = client.resolve_source(&source)?;
        self.retry_source = Some(source.clone());
        let retained_count = if replacing_key {
            self.applied
                .as_ref()
                .filter(|a| {
                    // A live cycle freezes a new absolute range for the same
                    // investigation. Its credential retry retains the loaded extent.
                    let mut previous = a.source.discover.clone();
                    previous.time = source.discover.time.clone();
                    a.source.id == source.id
                        && a.source.endpoint == source.endpoint
                        && previous == source.discover
                        && a.source.index_pattern == source.index_pattern
                        && a.source.time_field == source.time_field
                })
                .map_or(0, |a| a.events.len())
        } else {
            0
        };
        let mut history = History::for_source(client.clone(), &source)?;
        let mut page = history.page()?;
        let mut events: Vec<_> = page.events.drain(..).map(Arc::new).collect();
        // Credential replacement refreshes the loaded extent of the same query,
        // rather than silently reducing a multi-page investigation to page one.
        // Keep the old display atomically until that extent has been re-read.
        while events.len() < retained_count && history.can_page() {
            page = history.page()?;
            events.extend(page.events.drain(..).map(Arc::new));
        }
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
            quality: page.status.clone(),
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
            quality: page.status,
        })
    }
    fn more(
        &mut self,
        cancel: Arc<AtomicBool>,
        shared: &Shared,
        generation: u64,
    ) -> Result<Status> {
        let a = self.applied.as_mut().context("Apply history first")?;
        self.retry_source = Some(a.source.clone());
        let history = a
            .history
            .as_mut()
            .context("History stopped; Apply to refresh")?;
        history.set_cancellation(cancel);
        let page = history.page()?;
        let mut events = a.events.clone();
        events.extend(page.events.into_iter().map(Arc::new));
        let window = Arc::new(EventWindow::new(events.clone(), &a.source)?);
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
            quality: page.status.clone(),
        }));
        Ok(Status::History {
            complete: page.complete,
            limited: page.limited,
            warning: page.cleanup_warning,
            quality: page.status,
        })
    }
    fn reconcile(
        &mut self,
        shared: &Shared,
        sender: &async_channel::Sender<Update>,
        generation: u64,
        cancel: Arc<AtomicBool>,
    ) -> Status {
        match self.scan(shared, sender, generation, cancel.clone()) {
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
                Self::error(error)
            }
        }
    }
    fn scan(
        &mut self,
        shared: &Shared,
        sender: &async_channel::Sender<Update>,
        generation: u64,
        cancel: Arc<AtomicBool>,
    ) -> Result<Status> {
        self.close_history();
        let now = OffsetDateTime::now_utc();
        let range = self.mode.as_ref().context("Live is stopped")?.range(now)?;
        let began = now.format(&time::format_description::well_known::Rfc3339)?;
        let clock = Instant::now();
        let applied = self.applied.as_ref().context("Apply history first")?;
        let display_source = applied.source.clone();
        let mut source = display_source.clone();
        source.discover.time = range.clone();
        self.retry_source = Some(source.clone());
        let client = applied.client.with_cancellation(cancel.clone());
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
        let mut history = History::for_source(client, &source)?;
        let mut events = Vec::new();
        let mut pages = 0;
        loop {
            ensure!(!cancel.load(Ordering::Acquire), "Log request cancelled");
            let page = history.page()?;
            pages += 1;
            events.extend(page.events.into_iter().map(Arc::new));
            #[cfg(test)]
            if let Some(hook) = &self.timing.after_page {
                hook(pages);
            }
            let mut quality = page.status;
            if clock.elapsed() >= self.timing.cycle {
                quality.timed_out = true;
            }
            let ended = page.complete || quality.incomplete();
            if ended {
                let window = Arc::new(EventWindow::new(events.clone(), &source)?);
                let current = shared.0.lock().unwrap();
                ensure!(
                    current.generation == generation
                        && !current.closing
                        && !cancel.load(Ordering::Acquire),
                    "Log request cancelled"
                );
                let more = false;
                self.loaded = Some(Arc::new(Loaded {
                    window,
                    source: display_source,
                    range: range.clone(),
                    more,
                    quality: quality.clone(),
                }));
                if let Some(a) = self.applied.as_mut() {
                    a.events = events;
                }
                if quality.incomplete() {
                    return Ok(Status::History {
                        complete: false,
                        limited: quality.truncated,
                        warning: None,
                        quality,
                    });
                }
                return Ok(Status::Complete {
                    range,
                    began,
                    seconds: clock.elapsed().as_secs_f64(),
                    pages,
                    quality,
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

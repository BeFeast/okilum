//! The pinned log dock. It reuses the standalone Kubernetes log worker, the
//! bounded stream assembler and the ordinary LogView (filter, export, marks).
//!
//! Open logs pins context/namespace/Pod name+UID/container/instance. Before
//! `kubectl logs` starts, one fresh read compares the Pod's UID with the pinned
//! one; `kubectl logs` itself addresses the Pod by name, so a replacement after
//! that check is surfaced from the next Pods read, never hidden.
use super::model::{self, LogTarget, UidCheck};
use super::*;
use crate::reader_log_kubernetes::worker;
use okilum_core::kubernetes::{log_request, Cancel};
use okilum_core::log::stream::Snapshot;
use std::time::SystemTime;

pub(crate) const DEFAULT_TAIL: u32 = 1000;

/// Starts one log session. Production: the standalone panel's worker. Tests
/// substitute a worker without a process, so no real kubectl ever runs.
pub(crate) type Starter = Rc<
    dyn Fn(
        okilum_core::log::kubernetes::Request,
        String,
    ) -> (worker::Worker, async_channel::Receiver<worker::Update>),
>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    /// Reading the Pod once to confirm its UID; kubectl logs has not started.
    Verifying,
    Receiving,
    Stopping,
    /// kubectl exited by itself (history read, or the stream closed).
    Ended,
    Stopped,
    /// A stream bound stopped loading; received rows are kept.
    Truncated(String),
    /// A followed stream failed after data arrived (connection, Pod gone).
    Disconnected(String),
    /// The session's own failure (worker/transport text, never kubectl stderr).
    Failed(String),
    /// The UID check or the request before `kubectl logs` failed: nothing
    /// was started. Kept typed, so an export can show only its safe parts;
    /// the UI shows its summary.
    NotStarted(Failure),
    /// The name now refers to another Pod UID: nothing was started.
    Replaced {
        uid: String,
    },
}

impl Phase {
    pub(crate) fn active(&self) -> bool {
        matches!(self, Self::Verifying | Self::Receiving | Self::Stopping)
    }
    pub(crate) fn text(&self) -> String {
        match self {
            Self::Verifying => "Verifying Pod UID…".into(),
            Self::Receiving => "Receiving".into(),
            Self::Stopping => "Stopping…".into(),
            Self::Ended => "Ended".into(),
            Self::Stopped => "Stopped".into(),
            Self::Truncated(_) => "Stopped at a loading limit".into(),
            Self::Disconnected(_) => "Disconnected".into(),
            Self::Failed(_) => "Failed".into(),
            Self::NotStarted(_) => "Not started".into(),
            Self::Replaced { .. } => "Not started: Pod replaced".into(),
        }
    }
    fn tone(&self) -> Tone {
        match self {
            Self::Receiving => Tone::Good,
            Self::Verifying | Self::Stopping | Self::Ended | Self::Stopped => Tone::Neutral,
            Self::Truncated(_) | Self::Disconnected(_) | Self::Replaced { .. } => Tone::Warning,
            Self::Failed(_) | Self::NotStarted(_) => Tone::Danger,
        }
    }
    fn detail(&self) -> Option<String> {
        match self {
            Self::Truncated(e) | Self::Disconnected(e) | Self::Failed(e) => Some(e.clone()),
            Self::NotStarted(failure) => Some(failure.summary()),
            _ => None,
        }
    }
}

/// Where the rows in the log view came from: the session that sent them,
/// kept with them, so a retarget or restart cannot relabel old bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Shown {
    pub target: LogTarget,
    pub generation: u64,
    pub verified_at: Option<SystemTime>,
    pub follow: bool,
    pub tail_lines: u32,
}

#[derive(Clone)]
pub(crate) struct Session {
    pub target: LogTarget,
    pub phase: Phase,
    /// Stream new lines (`kubectl logs -f`) for this session.
    pub follow: bool,
    pub verified_at: Option<SystemTime>,
    pub received: bool,
    pub diagnostic: String,
    pub pending_bytes: usize,
}

pub(crate) struct Dock {
    pub session: Option<Session>,
    /// A different target asked for while a session holds evidence: kept
    /// until the user confirms or cancels the retarget.
    pub pending_retarget: Option<LogTarget>,
    pub message: Option<String>,
    /// The target whose rows the view currently shows.
    pub displayed: Option<Shown>,
    pub follow_stream: bool,
    pub tail: u32,
    pub generation: u64,
    pub starter: Starter,
    pub view: Option<Entity<reader_log::LogView>>,
    pending: Option<Arc<Snapshot>>,
    worker: Option<worker::Worker>,
    verify: Option<Cancel>,
    _task: Option<Task<()>>,
    _view_observer: Option<Subscription>,
}

impl Default for Dock {
    fn default() -> Self {
        Self {
            session: None,
            pending_retarget: None,
            message: None,
            displayed: None,
            follow_stream: true,
            tail: DEFAULT_TAIL,
            generation: 0,
            starter: Rc::new(worker::Worker::start),
            view: None,
            pending: None,
            worker: None,
            verify: None,
            _task: None,
            _view_observer: None,
        }
    }
}

impl Dock {
    /// Rows received but not yet handed to the view, if any.
    pub(crate) fn pending_snapshot(&self) -> Option<Arc<Snapshot>> {
        self.pending.clone()
    }
    pub(crate) fn worker_running(&self) -> bool {
        self.worker.is_some()
    }
    fn halt(&mut self) {
        if let Some(worker) = self.worker.take() {
            worker.cancel();
        }
        if let Some(cancel) = self.verify.take() {
            cancel.cancel();
        }
        self._task = None;
    }
}

/// What the copied command is known to address. It reads the Pod by name;
/// whether that name was confirmed to be the pinned UID depends on this
/// session's own check, which may be pending, failed or never started.
/// The shell a copied command is written for. Each has its own quoting;
/// a command quoted for one is not safe to paste into another.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Shell {
    /// sh, bash, zsh: POSIX quoting.
    Posix,
    /// Windows PowerShell 5.1 and PowerShell 7: single-quoted literals.
    /// Not cmd.exe, which has no safe quoting for every argument.
    PowerShell,
}

impl Shell {
    pub(crate) const NATIVE: Shell = if cfg!(windows) {
        Shell::PowerShell
    } else {
        Shell::Posix
    };
    pub(crate) fn name(self) -> &'static str {
        match self {
            Shell::Posix => "POSIX shell (sh, bash, zsh)",
            Shell::PowerShell => "PowerShell (not cmd.exe)",
        }
    }
}

/// Characters every supported shell passes through unquoted and unchanged.
/// No `,` (a PowerShell array), `@` (splatting), `$`, quotes, spaces or
/// cmd/sh metacharacters.
fn plain(arg: &str) -> bool {
    !arg.is_empty()
        && arg.bytes().all(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'/' | b'=' | b':')
        })
}

/// PowerShell treats these as single quotes too; each is doubled inside a
/// single-quoted literal.
const POWERSHELL_QUOTES: [char; 5] = ['\'', '\u{2018}', '\u{2019}', '\u{201A}', '\u{201B}'];

/// `kubectl` and its arguments for `shell`, or why it cannot be written
/// safely. Windows PowerShell 5.1 passes native arguments containing `"`
/// incorrectly, so such an argument is refused rather than mangled.
pub(crate) fn command_line(args: &[String], shell: Shell) -> Result<String, String> {
    let mut words = vec!["kubectl".to_string()];
    for arg in args {
        if plain(arg) {
            words.push(arg.clone());
            continue;
        }
        words.push(match shell {
            Shell::Posix => shell_words::quote(arg).into_owned(),
            Shell::PowerShell => {
                if arg.contains('"') {
                    return Err(format!(
                        "an argument contains a double quote, which PowerShell 5.1 passes to kubectl incorrectly: {arg}"
                    ));
                }
                let mut quoted = String::from("'");
                for c in arg.chars() {
                    if POWERSHELL_QUOTES.contains(&c) {
                        quoted.push(c);
                    }
                    quoted.push(c);
                }
                quoted.push('\'');
                quoted
            }
        });
    }
    Ok(words.join(" "))
}

pub(crate) fn copy_note(session: &Session, shell: Shell) -> String {
    let target = &session.target;
    let state = match (&session.phase, session.verified_at) {
        (Phase::Replaced { uid }, _) => format!(
            "the name now refers to Pod UID {}, not the pinned {}",
            model::short_uid(uid),
            target.short_uid()
        ),
        (_, Some(at)) => format!(
            "this session verified UID {} at {} before kubectl started; a later replacement is not detected by the command",
            target.short_uid(),
            model::clock(at)
        ),
        (Phase::Verifying, None) => format!(
            "the UID check for {} is still running",
            target.short_uid()
        ),
        (Phase::NotStarted(_), None) => format!(
            "the UID check for {} failed; nothing confirms the name refers to it",
            target.short_uid()
        ),
        (_, None) => format!("UID {} was not verified by this session", target.short_uid()),
    };
    format!(
        "Copied a kubectl command for {}. It reads Pod {} by name; {state}.",
        shell.name(),
        target.pod
    )
}

impl Workspace {
    /// The only way logs start. A different target while a session holds
    /// rows or runs needs an explicit confirmation; selection never calls it.
    pub(crate) fn request_logs(&mut self, target: LogTarget, cx: &mut Context<Self>) {
        self.dock.message = None;
        let busy = self.dock.session.as_ref().is_some_and(|session| {
            session.target != target && (session.phase.active() || session.received)
        });
        if busy {
            self.dock.pending_retarget = Some(target);
            cx.notify();
            return;
        }
        if self
            .dock
            .session
            .as_ref()
            .is_some_and(|s| s.target == target && s.phase.active())
        {
            self.dock.message = Some("This target is already pinned and running.".into());
            cx.notify();
            return;
        }
        self.pin(target, cx);
    }

    pub(crate) fn confirm_retarget(&mut self, cx: &mut Context<Self>) {
        if let Some(target) = self.dock.pending_retarget.take() {
            self.pin(target, cx);
        }
    }

    fn cancel_retarget(&mut self, cx: &mut Context<Self>) {
        self.dock.pending_retarget = None;
        cx.notify();
    }

    /// Pins the target and verifies its UID; the stream starts only after a
    /// matching UID. Loaded rows stay visible until the new session sends.
    pub(crate) fn pin(&mut self, target: LogTarget, cx: &mut Context<Self>) {
        self.dock.halt();
        self.dock.pending_retarget = None;
        self.dock.generation = self.dock.generation.wrapping_add(1);
        let generation = self.dock.generation;
        let cancel = Cancel::new();
        self.dock.verify = Some(cancel.clone());
        self.dock.session = Some(Session {
            target: target.clone(),
            phase: Phase::Verifying,
            follow: self.dock.follow_stream,
            verified_at: None,
            received: false,
            diagnostic: String::new(),
            pending_bytes: 0,
        });
        let receiver = backend::verify(
            self.reader.clone(),
            target.connection(),
            target.namespace.clone(),
            target.pod.clone(),
            cancel,
        );
        let executor = cx.background_executor().clone();
        self.dock._task = Some(cx.spawn(async move |this, cx| {
            if let Some(result) = backend::next_batch(&receiver, &executor)
                .await
                .and_then(|batch| batch.into_iter().next())
            {
                let _ = this.update(cx, |this, cx| this.verified(generation, result, cx));
            }
        }));
        cx.notify();
    }

    pub(crate) fn verified(
        &mut self,
        generation: u64,
        result: Result<okilum_core::kubernetes::Observation<okilum_core::kubernetes::Pod>, Failure>,
        cx: &mut Context<Self>,
    ) {
        if generation != self.dock.generation {
            return;
        }
        self.dock.verify = None;
        let Some(session) = self.dock.session.as_mut() else {
            return;
        };
        let observation = match result {
            Ok(observation) => observation,
            Err(Failure::Cancelled) => {
                session.phase = Phase::Stopped;
                cx.notify();
                return;
            }
            Err(failure) => {
                session.phase = Phase::NotStarted(failure);
                cx.notify();
                return;
            }
        };
        match model::check_fetched(&session.target, &observation) {
            UidCheck::Same => {}
            UidCheck::Replaced { uid } => {
                session.phase = Phase::Replaced { uid };
                cx.notify();
                return;
            }
            UidCheck::Missing | UidCheck::Unknown => {
                session.phase =
                    Phase::Failed("The verification read did not identify the pinned Pod.".into());
                cx.notify();
                return;
            }
        }
        session.verified_at = Some(observation.observed_at);
        let request = match log_request(
            &observation,
            &session.target.container,
            session.target.previous,
            session.follow,
            self.dock.tail,
        ) {
            Ok(request) => request,
            Err(failure) => {
                session.phase = Phase::NotStarted(failure);
                cx.notify();
                return;
            }
        };
        session.phase = Phase::Receiving;
        let scope = format!("kubernetes-workspace:{}:{generation}", self.vault_scope);
        let (worker, receiver) = (self.dock.starter)(request, scope);
        self.dock.worker = Some(worker);
        let executor = cx.background_executor().clone();
        self.dock._task = Some(cx.spawn(async move |this, cx| {
            while let Some(batch) = backend::next_batch(&receiver, &executor).await {
                let applied = this.update(cx, |this, cx| {
                    for update in batch {
                        this.log_update(generation, update, cx);
                    }
                });
                if applied.is_err() {
                    break;
                }
            }
        }));
        cx.notify();
    }

    pub(crate) fn log_update(
        &mut self,
        generation: u64,
        update: worker::Update,
        cx: &mut Context<Self>,
    ) {
        if generation != self.dock.generation {
            return;
        }
        let Some(session) = self.dock.session.as_mut() else {
            return;
        };
        if let Some(snapshot) = update.snapshot {
            self.dock.pending = Some(snapshot);
            self.dock.displayed = Some(Shown {
                target: session.target.clone(),
                generation,
                verified_at: session.verified_at,
                follow: session.follow,
                tail_lines: self.dock.tail,
            });
            session.received = true;
        }
        session.diagnostic = update.diagnostic;
        session.pending_bytes = update.pending;
        session.phase = match update.status {
            worker::Status::Reading => {
                if session.phase == Phase::Stopping {
                    Phase::Stopping
                } else {
                    Phase::Receiving
                }
            }
            worker::Status::Complete => Phase::Ended,
            worker::Status::Stopped => Phase::Stopped,
            worker::Status::Truncated(error) => Phase::Truncated(error),
            worker::Status::Failed(error) if session.follow && session.received => {
                Phase::Disconnected(error)
            }
            worker::Status::Failed(error) => Phase::Failed(error),
        };
        if !session.phase.active() {
            self.dock.worker = None;
        }
        cx.notify();
    }

    /// Stop receiving. Loaded rows, filters and the viewport stay.
    pub(crate) fn stop_logs(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.dock.session.as_mut() else {
            return;
        };
        match session.phase {
            Phase::Verifying => {
                if let Some(cancel) = self.dock.verify.take() {
                    cancel.cancel();
                }
                self.dock._task = None;
                session.phase = Phase::Stopped;
            }
            Phase::Receiving => {
                if let Some(worker) = &self.dock.worker {
                    worker.cancel();
                }
                session.phase = Phase::Stopping;
            }
            _ => return,
        }
        cx.notify();
    }

    /// Terminal handoff: the exact kubectl argv of the pinned target. It
    /// addresses the Pod by name, as the session does.
    pub(crate) fn copy_command(&mut self, cx: &mut Context<Self>) {
        let Some(session) = &self.dock.session else {
            return;
        };
        let target = &session.target;
        let request = okilum_core::log::kubernetes::Request {
            context: Some(target.context.clone()),
            namespace: target.namespace.clone(),
            pod: target.pod.clone(),
            container: target.container.clone(),
            previous: target.previous,
            follow: session.follow,
            tail_lines: self.dock.tail,
        };
        self.dock.message = Some(match request.args() {
            Ok(args) => match command_line(&args, Shell::NATIVE) {
                Ok(line) => {
                    cx.write_to_clipboard(ClipboardItem::new_string(line));
                    copy_note(session, Shell::NATIVE)
                }
                Err(why) => format!("Nothing copied: {why}"),
            },
            Err(error) => format!("No command: {error}"),
        });
        cx.notify();
    }

    /// Start the pinned target again (a fresh UID check first).
    fn restart_logs(&mut self, cx: &mut Context<Self>) {
        if let Some(target) = self.dock.session.as_ref().map(|s| s.target.clone()) {
            self.pin(target, cx);
        }
    }

    pub(crate) fn toggle_follow_bottom(&mut self, cx: &mut Context<Self>) {
        if let Some(view) = &self.dock.view {
            view.update(cx, |view, cx| {
                let follow = view.stream_bottom() != Some(true);
                view.set_stream_bottom(follow, cx);
            });
        }
    }

    /// Moves a received snapshot into the LogView. A new view follows the
    /// bottom; scrolling up pauses that without stopping the session.
    pub(crate) fn sync_dock_view(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(snapshot) = self.dock.pending.take() else {
            return;
        };
        if let Some(view) = &self.dock.view {
            view.update(cx, |view, cx| view.replace_stream(snapshot, cx));
            return;
        }
        let view = cx.new(|cx| {
            let mut view = reader_log::LogView::stream(snapshot, window, cx);
            view.set_stream_bottom(true, cx);
            view
        });
        self.dock._view_observer = Some(cx.observe(&view, |_, _, cx| cx.notify()));
        self.dock.view = Some(view);
    }

    pub(crate) fn uid_note(&self) -> Option<(String, Tone)> {
        let session = self.dock.session.as_ref()?;
        let target = &session.target;
        if let Phase::Replaced { uid } = &session.phase {
            return Some((
                format!(
                    "{} now refers to Pod UID {}, not the pinned {}. Open logs on the new Pod to retarget explicitly.",
                    target.pod,
                    model::short_uid(uid),
                    target.short_uid()
                ),
                Tone::Warning,
            ));
        }
        let data = self.data.as_ref();
        match data.map(|data| model::check_listed(target, data)) {
            Some(UidCheck::Replaced { uid }) => Some((
                format!(
                    "The latest Pods read shows {} as Pod UID {}, not the pinned {}. Rows received after the replacement may come from the new Pod: kubectl logs addresses Pods by name.",
                    target.pod,
                    model::short_uid(&uid),
                    target.short_uid()
                ),
                Tone::Warning,
            )),
            Some(UidCheck::Missing) => Some((
                format!(
                    "{} is no longer listed; the dock keeps what was received.",
                    target.pod
                ),
                Tone::Warning,
            )),
            _ => session.verified_at.map(|at| {
                (
                    format!(
                        "Pod UID {} verified at {} just before kubectl started. kubectl logs addresses the Pod by name, so a replacement after that shows here after the next Refresh.",
                        target.short_uid(),
                        model::clock(at)
                    ),
                    Tone::Neutral,
                )
            }),
        }
    }

    pub(crate) fn render_dock(&self, cx: &mut Context<Self>) -> AnyElement {
        let tokens = brand::palette(cx);
        let session = self.dock.session.as_ref();
        let active = session.is_some_and(|s| s.phase.active());
        let receiving = self.dock.worker_running();
        let follow_bottom = self
            .dock
            .view
            .as_ref()
            .and_then(|view| view.read(cx).stream_bottom())
            == Some(true);
        let mut bar = h_flex()
            .debug_selector(|| "k8s-dock-bar".into())
            .flex_none()
            .flex_wrap()
            .gap_2()
            .px_4()
            .py_2()
            .items_center()
            .border_b_1()
            .border_color(tokens.border)
            .bg(tokens.surface)
            .child(Icon::new(IconName::SquareTerminal).small())
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Logs"),
            );
        bar = match session {
            Some(session) => bar
                .child(
                    div()
                        .debug_selector(|| "k8s-dock-target".into())
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .font_family(brand::MONO_FONT)
                        .child(session.target.label()),
                )
                .child(
                    div()
                        .debug_selector(|| "k8s-dock-phase".into())
                        .flex_none()
                        .text_xs()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(tone_color(session.phase.tone(), cx))
                        .child(session.phase.text()),
                ),
            None => bar.child(
                div()
                    .text_xs()
                    .text_color(tokens.text_muted)
                    .child("Nothing pinned"),
            ),
        };
        bar = bar
            .child(div().flex_1())
            .child(
                Button::new("k8s-dock-stream")
                    .debug_selector(|| "k8s-dock-stream".into())
                    .small()
                    .ghost()
                    .label("Stream new lines")
                    .selected(self.dock.follow_stream)
                    .disabled(active)
                    .tooltip("kubectl logs --follow for the next start; history only when off")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.dock.follow_stream = !this.dock.follow_stream;
                        cx.notify();
                    })),
            )
            .child(
                Button::new("k8s-dock-follow-bottom")
                    .debug_selector(|| "k8s-dock-follow-bottom".into())
                    .small()
                    .ghost()
                    .label("Follow bottom")
                    .selected(follow_bottom)
                    .disabled(self.dock.view.is_none())
                    .tooltip("Keep the newest row in view. Scrolling up pauses this; receiving continues.")
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_follow_bottom(cx))),
            )
            .child(
                Button::new("k8s-dock-copy-command")
                    .debug_selector(|| "k8s-dock-copy-command".into())
                    .small()
                    .ghost()
                    .label("Copy command")
                    .disabled(session.is_none())
                    .tooltip("Copy the exact kubectl logs command of the pinned target for a terminal")
                    .on_click(cx.listener(|this, _, _, cx| this.copy_command(cx))),
            )
            .child(
                Button::new("k8s-dock-start")
                    .debug_selector(|| "k8s-dock-start".into())
                    .small()
                    .label("Start")
                    .disabled(session.is_none() || active)
                    .tooltip("Start the pinned target again after a fresh Pod UID check")
                    .on_click(cx.listener(|this, _, _, cx| this.restart_logs(cx))),
            )
            .child(
                Button::new("k8s-dock-stop")
                    .debug_selector(|| "k8s-dock-stop".into())
                    .small()
                    .label("Stop")
                    .disabled(!(receiving || session.is_some_and(|s| s.phase == Phase::Verifying)))
                    .on_click(cx.listener(|this, _, _, cx| this.stop_logs(cx))),
            );
        let mut notes: Vec<(String, Tone)> = Vec::new();
        if let Some(message) = &self.dock.message {
            notes.push((message.clone(), Tone::Neutral));
        }
        if let Some(session) = session {
            if let Some(detail) = session.phase.detail() {
                notes.push((detail, session.phase.tone()));
            }
            if let Some(note) = self.uid_note() {
                notes.push(note);
            }
            if let Some(data) = &self.data {
                if !session.target.in_scope(&data.scope) {
                    notes.push((
                        format!(
                            "Pinned to {} / {}; the list above shows {}.",
                            session.target.context,
                            session.target.namespace,
                            data.scope.label()
                        ),
                        Tone::Neutral,
                    ));
                }
            }
            if let Some(shown) = &self.dock.displayed {
                if shown.generation != self.dock.generation {
                    notes.push((
                        format!(
                            "Rows below are from an earlier session of {} until the new session sends data.",
                            shown.target.label()
                        ),
                        Tone::Warning,
                    ));
                }
            }
            if session.pending_bytes > 0 {
                notes.push((
                    format!(
                        "{} trailing bytes are waiting for a newline.",
                        session.pending_bytes
                    ),
                    Tone::Neutral,
                ));
            }
            if !session.diagnostic.is_empty() {
                notes.push((
                    format!("kubectl: {}", session.diagnostic.trim()),
                    Tone::Neutral,
                ));
            }
        }
        let retarget = self.dock.pending_retarget.as_ref().map(|target| {
            h_flex()
                .debug_selector(|| "k8s-dock-retarget".into())
                .flex_none()
                .flex_wrap()
                .gap_2()
                .px_4()
                .py_2()
                .items_center()
                .bg(brand::reader_palette(cx).warning_tint)
                .child(div().text_xs().child(format!(
                    "Replace the pinned logs with {}? The current session stops; its rows stay until the new one sends.",
                    target.label()
                )))
                .child(
                    Button::new("k8s-dock-retarget-confirm")
                        .debug_selector(|| "k8s-dock-retarget-confirm".into())
                        .small()
                        .primary()
                        .label("Retarget")
                        .on_click(cx.listener(|this, _, _, cx| this.confirm_retarget(cx))),
                )
                .child(
                    Button::new("k8s-dock-retarget-cancel")
                        .small()
                        .ghost()
                        .label("Keep current")
                        .on_click(cx.listener(|this, _, _, cx| this.cancel_retarget(cx))),
                )
        });
        let body: AnyElement = match &self.dock.view {
            Some(view) => div().flex_1().min_h_0().child(view.clone()).into_any_element(),
            None => div()
                .debug_selector(|| "k8s-dock-empty".into())
                .flex_1()
                .p_4()
                .text_sm()
                .text_color(tokens.text_muted)
                .child("No logs pinned. Select a Pod or container and choose Open logs. Selecting rows never starts kubectl.")
                .into_any_element(),
        };
        v_flex()
            .debug_selector(|| "k8s-dock".into())
            .flex_none()
            .h(relative(if self.lens == Lens::Workspace {
                0.4
            } else {
                0.32
            }))
            .min_h(px(220.))
            .w_full()
            .border_t_1()
            .border_color(tokens.border)
            .child(bar)
            .children(retarget)
            .when(!notes.is_empty(), |dock| {
                dock.child(
                    v_flex()
                        .debug_selector(|| "k8s-dock-notes".into())
                        .flex_none()
                        .px_4()
                        .py_1()
                        .gap(px(2.))
                        .max_h(px(96.))
                        .children(notes.into_iter().map(|(text, tone)| {
                            let color = match tone {
                                Tone::Neutral => tokens.text_muted,
                                tone => tone_color(tone, cx),
                            };
                            div().text_xs().text_color(color).child(text)
                        })),
                )
            })
            .child(body)
            .into_any_element()
    }
}

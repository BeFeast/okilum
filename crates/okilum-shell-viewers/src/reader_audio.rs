//! Streaming pure-Rust audio playback (MP3/WAV/M4A/FLAC/OGG).
//! Duration probing and decoding/output live off the UI thread.
use crate::prelude::*;
use gpui_component::slider::{Slider, SliderEvent, SliderState};
use rodio::{Decoder, Sink, Source};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Mutex, OnceLock};

const AUDIO_EXTENSIONS: &[&str] = &["mp3", "wav", "m4a", "flac", "ogg"];
const SEEK_STEP_SECS: f32 = 5.0;

pub fn eligible(path: &Path) -> bool {
    path.extension()
        .and_then(|s| s.to_str())
        .is_some_and(|ext| {
            AUDIO_EXTENSIONS
                .iter()
                .any(|candidate| ext.eq_ignore_ascii_case(candidate))
        })
}

/// Probe total duration from container/codec headers without opening an audio device.
fn probe_duration(root: &Path, rel: &str) -> anyhow::Result<Option<Duration>> {
    let path = reader_files::checked_path(root, rel)?;
    let decoder = Decoder::try_from(std::fs::File::open(path)?)?;
    Ok(decoder.total_duration())
}

#[derive(Clone, Default)]
struct Snapshot {
    playing: bool,
    position: Duration,
    duration: Option<Duration>,
    error: Option<String>,
}

enum Command {
    Play,
    Pause,
    #[cfg(any(test, feature = "test-support"))]
    #[allow(dead_code)]
    Toggle,
    Seek(f32),
    SeekRelative(f32),
    Volume(f32),
    Mute(bool),
}

struct ActiveSlot {
    id: u64,
    pause: mpsc::Sender<Command>,
}

/// Coordinates single-active-player playback across standalone and inline audio views.
struct ActivePlayerCoordinator {
    next_id: AtomicU64,
    active: Mutex<Option<ActiveSlot>>,
}

impl ActivePlayerCoordinator {
    fn new() -> Self {
        Self {
            next_id: AtomicU64::new(1),
            active: Mutex::new(None),
        }
    }

    fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    fn claim(&self, id: u64, sender: mpsc::Sender<Command>) {
        let previous = {
            let mut guard = self.active.lock().unwrap();
            guard.replace(ActiveSlot { id, pause: sender })
        };
        if let Some(prev) = previous {
            if prev.id != id {
                let _ = prev.pause.send(Command::Pause);
            }
        }
    }

    fn release(&self, id: u64) {
        let mut guard = self.active.lock().unwrap();
        if guard.as_ref().is_some_and(|slot| slot.id == id) {
            *guard = None;
        }
    }
}

fn shared_coordinator() -> Arc<ActivePlayerCoordinator> {
    static COORDINATOR: OnceLock<Arc<ActivePlayerCoordinator>> = OnceLock::new();
    COORDINATOR
        .get_or_init(|| Arc::new(ActivePlayerCoordinator::new()))
        .clone()
}

/// The same sink/decoder path is exercised offline with Rodio's real mixer.
struct Playback {
    root: PathBuf,
    rel: String,
    mixer: rodio::mixer::Mixer,
    sink: Sink,
    base: Duration,
    duration: Option<Duration>,
    volume: f32,
    muted: bool,
}
impl Playback {
    fn new(root: PathBuf, rel: String, mixer: rodio::mixer::Mixer) -> anyhow::Result<Self> {
        let sink = Sink::connect_new(&mixer);
        let mut playback = Self {
            root,
            rel,
            mixer,
            sink,
            base: Duration::ZERO,
            duration: None,
            volume: 0.8,
            muted: false,
        };
        playback.load(Duration::ZERO, true)?;
        Ok(playback)
    }

    fn apply_volume(&self) {
        self.sink
            .set_volume(if self.muted { 0.0 } else { self.volume });
    }

    fn current_position(&self) -> Duration {
        if self.sink.empty() {
            self.duration.unwrap_or(self.base)
        } else {
            self.base + self.sink.get_pos()
        }
    }

    fn load(&mut self, position: Duration, paused: bool) -> anyhow::Result<()> {
        let path = reader_files::checked_path(&self.root, &self.rel)?;
        let mut decoder = Decoder::try_from(std::fs::File::open(path)?)?;
        self.duration = decoder.total_duration();
        let position = self.duration.map_or(position, |d| position.min(d));
        if !position.is_zero() {
            decoder
                .try_seek(position)
                .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        }
        self.sink.stop();
        self.sink = Sink::connect_new(&self.mixer);
        self.apply_volume();
        if paused {
            self.sink.pause();
        }
        self.sink.append(decoder);
        self.base = position;
        Ok(())
    }

    fn apply(&mut self, command: Command) -> anyhow::Result<()> {
        match command {
            Command::Play => {
                if self.sink.empty() {
                    self.load(Duration::ZERO, false)?;
                } else {
                    self.sink.play();
                }
            }
            Command::Pause => {
                self.sink.pause();
            }
            #[cfg(any(test, feature = "test-support"))]
            Command::Toggle => {
                if self.sink.empty() {
                    self.load(Duration::ZERO, false)?;
                } else if self.sink.is_paused() {
                    self.sink.play();
                } else {
                    self.sink.pause();
                }
            }
            Command::Seek(fraction) => {
                if let Some(duration) = self.duration {
                    // Seek the decoder before enqueueing. Sink::try_seek waits for an
                    // output callback and can hang after device disconnection.
                    self.load(
                        duration.mul_f32(fraction.clamp(0., 1.)),
                        self.sink.is_paused(),
                    )?;
                }
            }
            Command::SeekRelative(delta_secs) => {
                if let Some(duration) = self.duration {
                    let target = (self.current_position().as_secs_f32() + delta_secs)
                        .clamp(0.0, duration.as_secs_f32());
                    self.load(Duration::from_secs_f32(target), self.sink.is_paused())?;
                }
            }
            Command::Volume(volume) => {
                self.volume = volume.clamp(0., 1.);
                self.apply_volume();
            }
            Command::Mute(muted) => {
                self.muted = muted;
                self.apply_volume();
            }
        }
        Ok(())
    }

    fn snapshot(&self) -> Snapshot {
        let ended = self.sink.empty();
        let position = self.current_position();
        Snapshot {
            playing: !ended && !self.sink.is_paused(),
            position,
            duration: self.duration,
            error: None,
        }
    }
}

fn worker(
    root: PathBuf,
    rel: String,
    receive: mpsc::Receiver<Command>,
    state: Arc<Mutex<Snapshot>>,
) {
    let result = (|| -> anyhow::Result<()> {
        // No device is touched until the user presses Play.
        let errors = state.clone();
        let mut stream = rodio::OutputStreamBuilder::from_default_device()?
            .with_error_callback(move |error| {
                let mut state = errors.lock().unwrap();
                state.playing = false;
                state.error = Some(format!("Audio output failed: {error}"));
            })
            .open_stream()?;
        stream.log_on_drop(false);
        let mut playback = Playback::new(root, rel, stream.mixer().clone())?;
        *state.lock().unwrap() = playback.snapshot();
        loop {
            match receive.recv_timeout(Duration::from_millis(100)) {
                Ok(command) => playback.apply(command)?,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
            let mut state = state.lock().unwrap();
            if state.error.is_some() {
                break;
            }
            *state = playback.snapshot();
        }
        Ok(())
    })();
    if let Err(error) = result {
        let mut state = state.lock().unwrap();
        state.playing = false;
        state.error = Some(format!("Cannot play audio: {error}"));
    }
}

pub struct AudioView {
    root: PathBuf,
    rel: String,
    player_id: u64,
    coordinator: Arc<ActivePlayerCoordinator>,
    focus_handle: FocusHandle,
    state: Arc<Mutex<Snapshot>>,
    send: Option<mpsc::Sender<Command>>,
    seek: Entity<SliderState>,
    volume: Entity<SliderState>,
    muted: bool,
    dragging: bool,
    _probe: Option<Task<()>>,
    _refresh: Option<Task<()>>,
}

impl Drop for AudioView {
    fn drop(&mut self) {
        self.coordinator.release(self.player_id);
    }
}

impl AudioView {
    pub fn new(root: PathBuf, rel: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self::new_with_coordinator(root, rel, shared_coordinator(), window, cx)
    }

    fn new_with_coordinator(
        root: PathBuf,
        rel: String,
        coordinator: Arc<ActivePlayerCoordinator>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let player_id = coordinator.next_id();
        let focus_handle = cx.focus_handle();
        let seek = cx.new(|_| SliderState::new().min(0.).max(100.).step(0.1));
        let volume = cx.new(|_| {
            SliderState::new()
                .min(0.)
                .max(100.)
                .step(1.)
                .default_value(80.)
        });
        cx.subscribe_in(
            &seek,
            window,
            |this, _, event: &SliderEvent, window, cx| match event {
                SliderEvent::Change(_) => this.dragging = true,
                SliderEvent::Release(value) => {
                    this.dragging = false;
                    this.command(Command::Seek(value.start() / 100.), window, cx);
                }
            },
        )
        .detach();
        cx.subscribe_in(
            &volume,
            window,
            |this, _, event: &SliderEvent, window, cx| {
                let (SliderEvent::Change(value) | SliderEvent::Release(value)) = event;
                this.command(Command::Volume(value.start() / 100.), window, cx);
            },
        )
        .detach();
        let probe_root = root.clone();
        let probe_rel = rel.clone();
        let _probe = Some(cx.spawn_in(window, async move |this, cx| {
            let probed = cx
                .background_executor()
                .spawn(async move { probe_duration(&probe_root, &probe_rel) })
                .await;
            let _ = this.update(cx, |this, cx| {
                let mut state = this.state.lock().unwrap();
                match probed {
                    Ok(duration) => {
                        if state.duration.is_none() {
                            state.duration = duration;
                        }
                    }
                    Err(error) => {
                        if this.send.is_none() && state.error.is_none() {
                            state.error = Some(format!("Cannot play audio: {error}"));
                        }
                    }
                }
                drop(state);
                cx.notify();
            });
        }));
        Self {
            root,
            rel,
            player_id,
            coordinator,
            focus_handle,
            state: Arc::new(Mutex::new(Snapshot::default())),
            send: None,
            seek,
            volume,
            muted: false,
            dragging: false,
            _probe,
            _refresh: None,
        }
    }

    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus_handle
    }

    fn toggle_mute(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.muted = !self.muted;
        self.command(Command::Mute(self.muted), window, cx);
        cx.notify();
    }

    fn toggle_play(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let playing = self.state.lock().unwrap().playing;
        if playing {
            self.coordinator.release(self.player_id);
            self.command(Command::Pause, window, cx);
        } else {
            self.command(Command::Play, window, cx);
        }
    }

    fn handle_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mods = &event.keystroke.modifiers;
        if mods.control || mods.alt || mods.platform || mods.function {
            return;
        }
        match event.keystroke.key.as_str() {
            "space" => self.toggle_play(window, cx),
            "left" => self.command(Command::SeekRelative(-SEEK_STEP_SECS), window, cx),
            "right" => self.command(Command::SeekRelative(SEEK_STEP_SECS), window, cx),
            "m" => self.toggle_mute(window, cx),
            _ => return,
        }
        cx.stop_propagation();
    }

    fn command(&mut self, command: Command, window: &mut Window, cx: &mut Context<Self>) {
        let starts_playback = matches!(command, Command::Play);
        // Sliders and seek keys are inert before first Play; changing volume/mute does not start playback.
        if self.send.is_none() && !starts_playback {
            return;
        }
        if self.state.lock().unwrap().error.is_some() {
            if !starts_playback {
                return;
            }
            self.send = None;
        }
        if self.send.is_none() {
            {
                let mut state = self.state.lock().unwrap();
                state.playing = false;
                state.error = None;
            }
            let (send, receive) = mpsc::channel();
            let (root, rel, state) = (self.root.clone(), self.rel.clone(), self.state.clone());
            match std::thread::Builder::new()
                .name("okilum-audio".into())
                .spawn(move || worker(root, rel, receive, state))
            {
                Ok(_) => {
                    let _ = send.send(Command::Volume(self.volume.read(cx).value().start() / 100.));
                    let _ = send.send(Command::Mute(self.muted));
                    self.send = Some(send);
                }
                Err(error) => self.state.lock().unwrap().error = Some(error.to_string()),
            }
        }
        if let Some(send) = &self.send {
            if starts_playback {
                self.coordinator.claim(self.player_id, send.clone());
            }
            let _ = send.send(command);
        }
        if self._refresh.is_none() {
            self._refresh = Some(cx.spawn_in(window, async move |this, cx| loop {
                cx.background_executor()
                    .timer(Duration::from_millis(250))
                    .await;
                if this
                    .update_in(cx, |this, window, cx| {
                        let state = this.state.lock().unwrap().clone();
                        if !this.dragging {
                            if let Some(duration) = state.duration.filter(|d| !d.is_zero()) {
                                this.seek.update(cx, |slider, cx| {
                                    slider.set_value(
                                        (state.position.as_secs_f32() / duration.as_secs_f32()
                                            * 100.)
                                            .clamp(0., 100.),
                                        window,
                                        cx,
                                    )
                                });
                            }
                        }
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }));
        }
        cx.notify();
    }
}

fn time(duration: Duration) -> String {
    let seconds = duration.as_secs();
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

impl Render for AudioView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.state.lock().unwrap().clone();
        let open_root = self.root.clone();
        let open_rel = self.rel.clone();
        v_flex()
            .id("audio-player")
            .debug_selector(|| "audio-player".into())
            .track_focus(&self.focus_handle)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    this.focus_handle.focus(window, cx);
                }),
            )
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                this.handle_key_down(event, window, cx);
            }))
            .gap_3()
            .p_4()
            .border_1()
            .border_color(cx.theme().border)
            .rounded_md()
            .child(
                Path::new(&self.rel)
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
            )
            .child(
                h_flex()
                    .gap_3()
                    .child(
                        Button::new("audio-toggle")
                            .label(if state.playing { "Pause" } else { "Play" })
                            .debug_selector(|| "audio-toggle".into())
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.focus_handle.focus(window, cx);
                                this.toggle_play(window, cx);
                            })),
                    )
                    .child(format!(
                        "{} / {}",
                        time(state.position),
                        state.duration.map(time).unwrap_or_else(|| "—".into())
                    )),
            )
            .child(
                div().w_full().child(
                    Slider::new(&self.seek)
                        .horizontal()
                        .disabled(state.duration.is_none()),
                ),
            )
            .child(
                h_flex()
                    .gap_3()
                    .child("Volume")
                    .child(
                        div()
                            .w(px(160.))
                            .child(Slider::new(&self.volume).horizontal()),
                    )
                    .child(
                        Button::new("audio-mute")
                            .label(if self.muted { "Unmute" } else { "Mute" })
                            .debug_selector(|| "audio-mute".into())
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.focus_handle.focus(window, cx);
                                this.toggle_mute(window, cx);
                            })),
                    ),
            )
            .when_some(state.error, |view, error| {
                view.child(
                    v_flex()
                        .gap_2()
                        .child(div().text_sm().text_color(cx.theme().danger).child(error))
                        .child(
                            Button::new("audio-open-system")
                                .label("Open with default app")
                                .debug_selector(|| "audio-open-system".into())
                                .on_click(move |_, window, cx| {
                                    reader_files::run(
                                        reader_files::FileAction::Open,
                                        &open_root,
                                        &open_rel,
                                        window,
                                        cx,
                                    );
                                }),
                        ),
                )
            })
    }
}

pub fn inline(
    root: PathBuf,
    path: PathBuf,
    occurrence: usize,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let rel = match reader_media::relative_file(&root, &path) {
        Ok(relative) => relative,
        Err(error) => {
            return div()
                .child(format!("Cannot open audio: {error}"))
                .into_any_element()
        }
    };
    // Window keyed state is dropped when the embed stops rendering: the sender
    // disconnects, so its worker drops both sink and device. No global player cache.
    window
        .use_keyed_state(
            SharedString::from(format!("audio:{}:{occurrence}", path.display())),
            cx,
            |window, cx| AudioView::new(root, rel, window, cx),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    fn root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../okilum-core/tests/fixtures/viewers/audio")
    }

    #[test]
    fn mp3_and_wav_decode_non_silence_and_seek_without_an_output_device() {
        for file in ["tone.mp3", "tone.wav"] {
            let path = root().join(file);
            let before = std::fs::read(&path).unwrap();
            let mut decoder = Decoder::try_from(std::fs::File::open(&path).unwrap()).unwrap();
            assert_eq!(decoder.channels(), 1);
            assert_eq!(decoder.sample_rate(), 44100);
            let duration = decoder.total_duration().expect("seekable file duration");
            assert!(
                (2.9..3.2).contains(&duration.as_secs_f64()),
                "{file}: {duration:?}"
            );
            decoder.try_seek(Duration::from_secs(2)).unwrap();
            let samples: Vec<_> = decoder.collect();
            assert!(
                (40000..50000).contains(&samples.len()),
                "{file}: {}",
                samples.len()
            );
            assert!(
                samples.iter().any(|s| s.abs() > 0.05),
                "must actually decode audible samples"
            );
            assert_eq!(std::fs::read(path).unwrap(), before);
        }
        assert!(Decoder::try_from(std::io::Cursor::new(b"corrupt WAV".to_vec())).is_err());
        assert!(eligible(Path::new("TONE.MP3")));
        assert!(!eligible(Path::new("clip.mp4")));
    }

    #[test]
    fn expanded_audio_formats_decode_seek_and_probe_duration() {
        for ext in [
            "tone.mp3",
            "tone.wav",
            "tone.flac",
            "tone.ogg",
            "tone.m4a",
            "TONE.FLAC",
            "TONE.OGG",
            "TONE.M4A",
        ] {
            assert!(eligible(Path::new(ext)), "{ext} must be eligible");
        }
        for rejected in ["clip.mp4", "clip.mkv", "note.md", "voice.opus", "audio"] {
            assert!(
                !eligible(Path::new(rejected)),
                "{rejected} must not be eligible"
            );
        }

        for file in ["tone.flac", "tone.ogg", "tone.m4a", "tone-alac.m4a"] {
            let path = root().join(file);
            let before = std::fs::read(&path).unwrap();
            let probed = probe_duration(&root(), file)
                .unwrap_or_else(|e| panic!("{file} probe failed: {e}"))
                .expect("seekable file duration");
            assert!(
                (2.9..3.2).contains(&probed.as_secs_f64()),
                "{file} probed duration: {probed:?}"
            );

            let mut decoder = Decoder::try_from(std::fs::File::open(&path).unwrap()).unwrap();
            assert_eq!(decoder.channels(), 1, "{file}");
            assert_eq!(decoder.sample_rate(), 44100, "{file}");
            decoder.try_seek(Duration::from_secs(2)).unwrap();
            let samples: Vec<_> = decoder.collect();
            assert!(
                (38000..52000).contains(&samples.len()),
                "{file} remaining samples after 2s seek: {}",
                samples.len()
            );
            assert!(
                samples.iter().any(|s| s.abs() > 0.05),
                "{file}: must actually decode audible samples"
            );
            assert_eq!(std::fs::read(path).unwrap(), before);
        }

        // Unsupported codec (Ogg/Opus) must fail cleanly instead of panicking.
        assert!(
            probe_duration(&root(), "unsupported.ogg").is_err(),
            "Ogg/Opus is unsupported in Symphonia 0.5.5 and must return an honest error"
        );
    }

    #[test]
    fn real_sink_controls_pause_seek_volume_end_and_replay() {
        for file in ["tone.mp3", "tone.wav"] {
            let (mixer, mut output) = rodio::mixer::mixer(1, 44100);
            let mut playback = Playback::new(root(), file.into(), mixer).unwrap();
            assert!(!playback.snapshot().playing);
            playback.apply(Command::Volume(0.3)).unwrap();
            assert_eq!(playback.sink.volume(), 0.3);
            playback.apply(Command::Toggle).unwrap();
            assert!(playback.snapshot().playing);
            let played: Vec<_> = output.by_ref().take(22050).collect();
            assert!(played.iter().any(|s| s.abs() > 0.01));
            assert!(playback.snapshot().position >= Duration::from_millis(400));
            playback.apply(Command::Toggle).unwrap();
            assert!(!playback.snapshot().playing);
            playback.apply(Command::Seek(0.5)).unwrap();
            assert!(!playback.snapshot().playing, "seeking preserves pause");
            assert_eq!(playback.sink.volume(), 0.3);
            assert!(playback.snapshot().position >= Duration::from_millis(1450));
            playback.apply(Command::Toggle).unwrap();
            output.by_ref().take(44100 * 4).for_each(drop);
            assert!(playback.sink.empty(), "positive control consumes to EOF");
            assert!(!playback.snapshot().playing);
            playback.apply(Command::Toggle).unwrap();
            assert!(playback.snapshot().playing, "Play restarts after EOF");
            assert!(playback.snapshot().position < Duration::from_millis(100));
        }
    }

    #[test]
    fn real_sink_mute_and_relative_seek_for_expanded_formats() {
        for file in ["tone.flac", "tone.ogg", "tone.m4a", "tone-alac.m4a"] {
            let (mixer, mut output) = rodio::mixer::mixer(1, 44100);
            let mut playback = Playback::new(root(), file.into(), mixer).unwrap();
            playback.apply(Command::Volume(0.6)).unwrap();
            playback.apply(Command::Mute(true)).unwrap();
            assert_eq!(playback.sink.volume(), 0.0, "{file}: mute silences sink");
            playback.apply(Command::Play).unwrap();
            let silent: Vec<_> = output.by_ref().take(4410).collect();
            assert!(
                silent.iter().all(|s| s.abs() < 1e-6),
                "{file}: muted sink must emit silence"
            );
            playback.apply(Command::Mute(false)).unwrap();
            assert_eq!(
                playback.sink.volume(),
                0.6,
                "{file}: unmute restores previous volume"
            );
            let audible: Vec<_> = output.by_ref().take(4410).collect();
            assert!(
                audible.iter().any(|s| s.abs() > 0.01),
                "{file}: unmuted sink must emit audio"
            );
            playback.apply(Command::SeekRelative(1.5)).unwrap();
            assert!(
                playback.snapshot().position >= Duration::from_millis(1500),
                "{file}: relative forward seek moved position"
            );
            playback.apply(Command::SeekRelative(-5.0)).unwrap();
            assert!(
                playback.snapshot().position < Duration::from_millis(100),
                "{file}: relative backward seek clamps to zero"
            );
            playback.apply(Command::Pause).unwrap();
            assert!(!playback.snapshot().playing, "{file}: Pause stops playback");
        }
    }

    #[test]
    fn single_active_player_coordinator_pauses_previous_player() {
        let coordinator = ActivePlayerCoordinator::new();
        let (tx1, rx1) = mpsc::channel();
        let (tx2, rx2) = mpsc::channel();
        let id1 = coordinator.next_id();
        let id2 = coordinator.next_id();

        let (mixer1, _out1) = rodio::mixer::mixer(1, 44100);
        let (mixer2, _out2) = rodio::mixer::mixer(1, 44100);
        let mut p1 = Playback::new(root(), "tone.flac".into(), mixer1).unwrap();
        let mut p2 = Playback::new(root(), "tone.ogg".into(), mixer2).unwrap();

        // Start player 1.
        coordinator.claim(id1, tx1.clone());
        p1.apply(Command::Play).unwrap();
        assert!(p1.snapshot().playing);
        assert!(
            rx1.try_recv().is_err(),
            "claiming own slot never pauses self"
        );

        // Start player 2: coordinator must send Command::Pause to player 1.
        coordinator.claim(id2, tx2.clone());
        p2.apply(Command::Play).unwrap();
        assert!(p2.snapshot().playing);
        let cmd_for_p1 = rx1.try_recv().expect("player 1 must receive Pause");
        assert!(matches!(cmd_for_p1, Command::Pause));
        p1.apply(cmd_for_p1).unwrap();
        assert!(!p1.snapshot().playing, "player 1 is now paused");

        // Resume player 1: coordinator must send Command::Pause to player 2.
        coordinator.claim(id1, tx1);
        p1.apply(Command::Play).unwrap();
        let cmd_for_p2 = rx2.try_recv().expect("player 2 must receive Pause");
        assert!(matches!(cmd_for_p2, Command::Pause));
        p2.apply(cmd_for_p2).unwrap();
        assert!(!p2.snapshot().playing, "player 2 is now paused");
        assert!(p1.snapshot().playing, "player 1 resumed");

        coordinator.release(id1);
    }

    #[gpui::test]
    fn audio_view_probes_duration_in_background_and_handles_keys(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        cx.update(crate::test_init);
        let coordinator = Arc::new(ActivePlayerCoordinator::new());

        // 1. Valid FLAC file: background probe populates duration without touching audio device.
        let mut valid_view = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                AudioView::new_with_coordinator(
                    root(),
                    "tone.flac".into(),
                    coordinator.clone(),
                    window,
                    cx,
                )
            });
            valid_view = Some(view.clone());
            Root::new(view, window, cx)
        });
        let valid_view = valid_view.unwrap();
        visual.run_until_parked();
        let probed = valid_view.read_with(visual, |v, _| {
            let s = v.state.lock().unwrap();
            (s.duration, s.error.clone(), v.send.is_none())
        });
        assert!(
            probed
                .0
                .is_some_and(|d| (2.9..3.2).contains(&d.as_secs_f64())),
            "background probe must populate duration: {probed:?}"
        );
        assert!(probed.1.is_none(), "valid FLAC must have no error");
        assert!(probed.2, "opening audio view must not spawn output worker");

        // Focus AudioView and press 'm' to toggle mute; 'ctrl-m' must be ignored.
        valid_view.update_in(visual, |v, window, cx| {
            v.focus_handle().focus(window, cx);
        });
        visual.run_until_parked();
        visual.simulate_keystrokes("m");
        visual.run_until_parked();
        valid_view.read_with(visual, |v, _| assert!(v.muted, "'m' must mute"));
        visual.simulate_keystrokes("ctrl-m");
        visual.run_until_parked();
        valid_view.read_with(visual, |v, _| {
            assert!(v.muted, "modified key must not toggle mute")
        });
        visual.simulate_keystrokes("m");
        visual.run_until_parked();
        valid_view.read_with(visual, |v, _| assert!(!v.muted, "second 'm' must unmute"));

        // 2. Unsupported Ogg/Opus file: background probe reports honest error + system fallback button.
        let mut unsupported_view = None;
        let (_, visual2) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                AudioView::new_with_coordinator(
                    root(),
                    "unsupported.ogg".into(),
                    coordinator.clone(),
                    window,
                    cx,
                )
            });
            unsupported_view = Some(view.clone());
            Root::new(view, window, cx)
        });
        let unsupported_view = unsupported_view.unwrap();
        visual2.run_until_parked();
        unsupported_view.read_with(visual2, |v, _| {
            assert!(
                v.state.lock().unwrap().error.is_some(),
                "unsupported codec must surface an error"
            );
        });
        assert!(
            visual2.debug_bounds("audio-open-system").is_some(),
            "error state must offer Open with default app fallback"
        );
    }
}

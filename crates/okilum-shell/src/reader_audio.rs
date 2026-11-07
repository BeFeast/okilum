//! Streaming MP3/WAV playback. The output device and decoder live off the UI thread.
use super::*;
use gpui_component::slider::{Slider, SliderEvent, SliderState};
use rodio::{Decoder, Sink, Source};
use std::sync::{mpsc, Mutex};

pub(super) fn eligible(path: &Path) -> bool {
    path.extension()
        .and_then(|s| s.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("mp3") || ext.eq_ignore_ascii_case("wav"))
}

#[derive(Clone, Default)]
struct Snapshot {
    playing: bool,
    position: Duration,
    duration: Option<Duration>,
    error: Option<String>,
}

enum Command {
    Toggle,
    Seek(f32),
    Volume(f32),
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
        };
        playback.load(Duration::ZERO, true)?;
        Ok(playback)
    }

    fn load(&mut self, position: Duration, paused: bool) -> anyhow::Result<()> {
        let path = reader_files::checked_path(&self.root, &self.rel)?;
        let mut decoder = Decoder::try_from(std::fs::File::open(path)?)?;
        self.duration = decoder.total_duration();
        let position = self.duration.map_or(position, |d| position.min(d));
        if !position.is_zero() {
            decoder.try_seek(position)?;
        }
        self.sink.stop();
        self.sink = Sink::connect_new(&self.mixer);
        self.sink.set_volume(self.volume);
        if paused {
            self.sink.pause();
        }
        self.sink.append(decoder);
        self.base = position;
        Ok(())
    }

    fn apply(&mut self, command: Command) -> anyhow::Result<()> {
        match command {
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
            Command::Volume(volume) => {
                self.volume = volume.clamp(0., 1.);
                self.sink.set_volume(self.volume);
            }
        }
        Ok(())
    }

    fn snapshot(&self) -> Snapshot {
        let ended = self.sink.empty();
        let position = if ended {
            self.duration.unwrap_or(self.base)
        } else {
            self.base + self.sink.get_pos()
        };
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

pub(super) struct AudioView {
    root: PathBuf,
    rel: String,
    state: Arc<Mutex<Snapshot>>,
    send: Option<mpsc::Sender<Command>>,
    seek: Entity<SliderState>,
    volume: Entity<SliderState>,
    dragging: bool,
    _refresh: Option<Task<()>>,
}
impl AudioView {
    pub(super) fn new(
        root: PathBuf,
        rel: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
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
        Self {
            root,
            rel,
            state: Arc::new(Mutex::new(Snapshot::default())),
            send: None,
            seek,
            volume,
            dragging: false,
            _refresh: None,
        }
    }

    fn command(&mut self, command: Command, window: &mut Window, cx: &mut Context<Self>) {
        // Sliders are inert before first Play; changing volume does not start playback.
        if self.send.is_none() && !matches!(command, Command::Toggle) {
            return;
        }
        if self.state.lock().unwrap().error.is_some() {
            if !matches!(command, Command::Toggle) {
                return;
            }
            self.send = None;
        }
        if self.send.is_none() {
            *self.state.lock().unwrap() = Snapshot::default();
            let (send, receive) = mpsc::channel();
            let (root, rel, state) = (self.root.clone(), self.rel.clone(), self.state.clone());
            match std::thread::Builder::new()
                .name("okilum-audio".into())
                .spawn(move || worker(root, rel, receive, state))
            {
                Ok(_) => {
                    let _ = send.send(Command::Volume(self.volume.read(cx).value().start() / 100.));
                    self.send = Some(send);
                }
                Err(error) => self.state.lock().unwrap().error = Some(error.to_string()),
            }
        }
        if let Some(send) = &self.send {
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
        v_flex()
            .id("audio-player")
            .debug_selector(|| "audio-player".into())
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
                                this.command(Command::Toggle, window, cx)
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
                h_flex().gap_3().child("Volume").child(
                    div()
                        .w(px(160.))
                        .child(Slider::new(&self.volume).horizontal()),
                ),
            )
            .when_some(state.error, |view, error| {
                view.child(div().text_sm().text_color(cx.theme().danger).child(error))
            })
    }
}

pub(super) fn inline(
    root: PathBuf,
    path: PathBuf,
    occurrence: usize,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let Ok(relative) = path.strip_prefix(&root) else {
        return div()
            .child("Audio is outside this vault")
            .into_any_element();
    };
    let rel = relative.to_string_lossy().into_owned();
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
        assert!(!eligible(Path::new("not-yet.flac")));
        assert!(eligible(Path::new("TONE.MP3")));
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
}

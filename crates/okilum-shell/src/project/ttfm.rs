//! One bounded diagnostic per native panel open, using the process monotonic clock.
use super::*;
use std::{sync::Mutex, time::Instant};

#[derive(Clone, Copy)]
pub(super) enum Phase {
    Auth = 1,
    Connect,
    Shell,
    Thread,
    Paint,
}

#[derive(Clone)]
pub(super) struct Ttfm(Arc<Mutex<Measurement>>);
struct Measurement {
    opened: Instant,
    phases: [Option<Instant>; 6],
    reused: bool,
    finished: bool,
    trace: Option<reader_diagnostics::Trace>,
}
impl Ttfm {
    pub(super) fn open(opened: Instant, reused: bool, cx: &App) -> Self {
        Self(Arc::new(Mutex::new(Measurement {
            opened,
            // Descriptor lookup begins with panel open, before any async work.
            phases: [Some(opened), None, None, None, None, None],
            reused,
            finished: false,
            trace: reader_diagnostics::trace(cx),
        })))
    }
    pub(super) fn cancel(&self) {
        self.0.lock().unwrap().finished = true;
    }
    pub(super) fn needs_paint(&self) -> bool {
        let m = self.0.lock().unwrap();
        !m.finished && m.phases[Phase::Paint as usize].is_none()
    }
    pub(super) fn socket_connected(&self, phases: okilum_t3::stream::ConnectPhases) {
        self.mark(Phase::Auth, phases.auth_done);
        self.mark(Phase::Connect, phases.websocket_open);
    }
    pub(super) fn mark(&self, phase: Phase, at: Instant) {
        let mut m = self.0.lock().unwrap();
        if m.finished || at < m.opened {
            return;
        }
        m.phases[phase as usize].get_or_insert(at);
        let Some(painted) = m.phases[Phase::Paint as usize] else {
            return;
        };
        // Cold subscriptions run independently. Preserve the actual first paint
        // even if the shell reply arrives later; enqueue once both are observed.
        // A warm open can paint retained history before either socket reconnects.
        if !m.reused && m.phases.iter().any(Option::is_none) {
            return;
        }
        m.finished = true;
        if let Some(trace) = &m.trace {
            let mut details = serde_json::Map::new();
            for (name, at) in [
                "connect_start_ms",
                "auth_ms",
                "connect_ms",
                "shell_ms",
                "thread_ms",
                "render_ms",
            ]
            .into_iter()
            .zip(m.phases)
            {
                details.insert(
                    name.into(),
                    at.map(|at| at.duration_since(m.opened).as_secs_f64() * 1000.)
                        .into(),
                );
            }
            details.insert(
                "open_to_first_message_ms".into(),
                (painted.duration_since(m.opened).as_secs_f64() * 1000.).into(),
            );
            details.insert("reused_connection".into(), m.reused.into());
            trace.event("manager_ttfm", serde_json::Value::Object(details));
        }
    }
}

impl ProjectView {
    pub(super) fn ttfm_open(&mut self, opened: Instant, cx: &mut Context<Self>) {
        self.ttfm.cancel();
        self.ttfm = Ttfm::open(opened, self.native.active(), cx);
        self.feed.update(cx, |feed, cx| {
            feed.ttfm = Some(self.ttfm.clone());
            // A warm feed may otherwise reuse a cached frame with the old probe.
            cx.notify();
        });
    }
}

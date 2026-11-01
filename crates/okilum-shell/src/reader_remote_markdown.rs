//! Background controller for explicit SSH quick view. The Reader owns rendering
//! and routing; remote source is never inserted into the local editor/vault.
use okilum_core::{
    log::sftp::{OpenSsh, Transport},
    remote_markdown::{self, Source},
};
use std::{
    io,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

#[derive(Debug)]
pub(crate) enum State {
    Idle,
    Loading { target: String },
    Ready { source: Source, markdown: String },
    Cancelled,
    Disconnected(String),
}
impl State {
    /// Visible status for Reader chrome; errors must not masquerade as empty text.
    pub(crate) fn message(&self) -> String {
        match self {
            Self::Idle => "Choose a remote Markdown file".into(),
            Self::Loading { target } => format!("Opening {target}…"),
            Self::Ready { source, .. } => format!("Read-only · {}", source.url()),
            Self::Cancelled => "Remote read cancelled".into(),
            Self::Disconnected(error) => format!("Disconnected: {error}"),
        }
    }
}

pub(crate) struct Reply {
    generation: u64,
    source: Source,
    result: io::Result<String>,
}

pub(crate) struct Controller {
    generation: u64,
    cancel: Option<Arc<AtomicBool>>,
    pub(crate) state: State,
}
impl Default for Controller {
    fn default() -> Self {
        Self {
            generation: 0,
            cancel: None,
            state: State::Idle,
        }
    }
}
impl Controller {
    /// Call only for an explicit user Open action. Parsing finishes before any
    /// child is spawned. Parent receives replies asynchronously, then notifies UI.
    pub(crate) fn open(&mut self, url: &str) -> io::Result<async_channel::Receiver<Reply>> {
        let source = Source::parse(url)?;
        Ok(self.start(source, OpenSsh::default()))
    }
    fn start(
        &mut self,
        source: Source,
        transport: impl Transport + Send + 'static,
    ) -> async_channel::Receiver<Reply> {
        self.cancel();
        let generation = self.generation;
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel = Some(cancel.clone());
        self.state = State::Loading {
            target: source.url().into(),
        };
        let (sender, receiver) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let mut transport = transport;
            let result =
                remote_markdown::read(&source, &mut transport, || cancel.load(Ordering::Acquire));
            let _ = sender.try_send(Reply {
                generation,
                source,
                result,
            });
        });
        receiver
    }
    /// Returns false for completion of a replaced/cancelled source. No stale
    /// completion can change either the displayed target or its Markdown.
    pub(crate) fn receive(&mut self, reply: Reply) -> bool {
        if reply.generation != self.generation {
            return false;
        }
        self.cancel = None;
        self.state = match reply.result {
            Ok(markdown) => State::Ready {
                source: reply.source,
                markdown,
            },
            Err(error) if error.kind() == io::ErrorKind::Interrupted => State::Cancelled,
            Err(error) => State::Disconnected(error.to_string()),
        };
        true
    }
    /// Cancels publication immediately. An in-flight transport operation retains
    /// its existing bounded deadline and disconnects on the background worker.
    pub(crate) fn cancel(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            cancel.store(true, Ordering::Release);
        }
        self.generation = self
            .generation
            .checked_add(1)
            .expect("remote view generation exhausted");
        self.state = State::Cancelled;
    }
    /// Returns a remote target plus preserved heading intent. The caller must
    /// explicitly handle heading landing; never silently report a note-top hit.
    pub(crate) fn resolve_link(&self, link: &str) -> io::Result<(Source, Option<String>)> {
        match &self.state {
            State::Ready { source, .. } => source.resolve(link),
            _ => Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "No remote Markdown loaded",
            )),
        }
    }
}
impl Drop for Controller {
    fn drop(&mut self) {
        self.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture;
    impl Transport for Fixture {
        fn open(&mut self, _: &okilum_core::log::sftp::Target) -> io::Result<()> {
            Ok(())
        }
        fn read_at(&mut self, offset: u64, _: usize) -> io::Result<Vec<u8>> {
            Ok(if offset == 0 {
                b"# Ready".to_vec()
            } else {
                vec![]
            })
        }
        fn disconnect(&mut self) {}
    }
    #[test]
    fn mocked_worker_publishes_source_and_cancel_rejects_stale_completion() {
        let mut controller = Controller::default();
        assert!(controller.open("ssh://user@host/note.md").is_err());
        assert!(matches!(controller.state, State::Idle));
        let receiver = controller.start(
            Source::parse("ssh://alias/notes/start.md").unwrap(),
            Fixture,
        );
        assert!(controller.receive(receiver.recv_blocking().unwrap()));
        assert!(
            matches!(&controller.state, State::Ready { markdown, .. } if markdown == "# Ready")
        );
        assert!(controller.state.message().contains("Read-only"));
        assert_eq!(
            controller.resolve_link("next.md").unwrap().0.url(),
            "ssh://alias/notes/next.md"
        );
        let receiver = controller.start(Source::parse("ssh://alias/other.md").unwrap(), Fixture);
        controller.cancel();
        assert!(!controller.receive(receiver.recv_blocking().unwrap()));
        assert!(matches!(controller.state, State::Cancelled));
    }
    #[test]
    fn disconnected_reply_is_visible_and_never_ready() {
        let mut controller = Controller::default();
        let reply = Reply {
            generation: 0,
            source: Source::parse("ssh://alias/a.md").unwrap(),
            result: Err(io::Error::new(io::ErrorKind::BrokenPipe, "connection lost")),
        };
        assert!(controller.receive(reply));
        assert_eq!(controller.state.message(), "Disconnected: connection lost");
        assert!(controller.resolve_link("a.md").is_err());
    }
}

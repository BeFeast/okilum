//! Window-owned SSH Markdown view with explicit guarded saves and local recovery.
//! Remote source never enters the local Reader editor, vault or saved history.
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
    fn is_current(&self, origin: &Source, generation: u64) -> bool {
        generation == self.generation
            && matches!(&self.state, State::Ready { source, .. } if source.url() == origin.url())
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

/// Recognize the scheme even for invalid targets, so a rejected SSH link never
/// falls through into local note handling or an external system URL launcher.
pub(crate) fn is_ssh_url(url: &str) -> bool {
    url.split_once(':')
        .is_some_and(|(scheme, _)| scheme.eq_ignore_ascii_case("ssh"))
}

/// Reader rewrites unsupported schemes before the click callback. Recover an
/// SSH target only from the exact current document's captured link identity;
/// never parse a diagnostic string as navigation authority.
pub(crate) fn authored_ssh_url<'a>(
    url: &str,
    identities: impl IntoIterator<Item = (&'a str, bool, &'a str)>,
) -> io::Result<Option<String>> {
    if is_ssh_url(url) {
        return Ok(Some(url.into()));
    }
    if !url.starts_with("okilum://unsupported/") {
        return Ok(None);
    }
    let mut selected: Option<&str> = None;
    for (rendered, wiki, target) in identities {
        if rendered != url || wiki || !is_ssh_url(target) {
            continue;
        }
        if selected.is_some_and(|previous| previous != target) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Ambiguous remote Markdown target",
            ));
        }
        selected = Some(target);
    }
    Ok(selected.map(str::to_owned))
}

fn destination(url: &str) -> io::Result<(Source, Option<String>)> {
    let (path, fragment) = url
        .split_once('#')
        .map_or((url, None), |(p, f)| (p, Some(f)));
    let source = Source::parse(path)?;
    let heading = fragment.map(okilum_core::document_links::decode);
    if heading
        .as_ref()
        .is_some_and(|heading| heading.trim().is_empty())
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "The heading target is empty",
        ));
    }
    Ok((source, heading))
}

enum Location {
    Folder(okilum_core::remote_markdown::folder::Folder),
    Document(Source, Option<String>),
}
fn location(url: &str) -> io::Result<Location> {
    let path = url.split('#').next().unwrap();
    if path.ends_with('/') || !path.to_ascii_lowercase().ends_with(".md") {
        Ok(Location::Folder(
            okilum_core::remote_markdown::folder::Folder::parse(url)?,
        ))
    } else {
        let (source, heading) = destination(url)?;
        Ok(Location::Document(source, heading))
    }
}

fn literal_remote_html(body: &str) -> String {
    let html =
        regex::Regex::new(r"(?is)<(?:/?[a-z][a-z0-9-]*(?:[\s/][^>]*)?|![^>]*|\?[^>]*)>").unwrap();
    okilum_core::prose::replace_in_prose(body, |prose| {
        html.replace_all(prose, |capture: &regex::Captures| {
            capture[0].replace('<', "&lt;").replace('>', "&gt;")
        })
        .into_owned()
    })
}

#[path = "reader_remote_markdown/editing.rs"]
mod editing;
#[path = "reader_remote_markdown/folders.rs"]
mod folders;
pub(crate) use view::show;

// Window-owned GPUI integration. Kept separate from the injectable controller:
// closing the window releases its entity and cancels the transport publication.
mod view {
    use super::*;
    use crate::*;

    pub(super) struct Prepared {
        rendered: String,
        headings: okilum_core::document_links::HeadingInventory,
    }
    fn prepare(markdown: &str) -> Prepared {
        let body = okilum_core::render::without_frontmatter(markdown);
        let body = okilum_core::obsidian::before_links(body);
        // Remote HTML and media have no local filesystem/network authority.
        // Show authored HTML literally, while retaining code examples verbatim.
        let body = literal_remote_html(&body);
        let body = okilum_core::render::rewrite_source_images_with(&body, |_| {
            Some(okilum_core::render::UNAVAILABLE_IMAGE.into())
        });
        let body = okilum_core::render::rewrite_highlights(&body);
        let rendered = okilum_core::obsidian::after_links(&body);
        let headings = okilum_core::document_links::HeadingInventory::new(&rendered);
        Prepared { rendered, headings }
    }

    pub(crate) fn show(url: &str, cx: &mut App) -> io::Result<()> {
        let target = location(url)?;
        // OpenSSH may need a passphrase, password or host trust (#1610).
        crate::reader_ssh_askpass::ensure(cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(window_state::default_bounds(
                size(px(900.), px(700.)),
                cx,
            ))),
            window_min_size: Some(size(px(600.), px(400.))),
            ..TitleBar::window_options()
        };
        cx.open_window(options, move |window, cx| {
            window.set_window_title("Remote Markdown");
            let panel = cx.new(|cx| {
                let mut panel = Panel::new();
                match target {
                    Location::Folder(folder) => panel.browse(folder, cx),
                    Location::Document(source, heading) => panel.open(source, heading, cx),
                }
                panel
            });
            let weak = panel.downgrade();
            window.on_window_should_close(cx, move |window, cx| {
                crate::reader_diagnostics::window_close_requested(
                    window,
                    cx,
                    "native platform request",
                );
                weak.update(cx, |panel, cx| panel.stop(cx)).unwrap_or(true)
            });
            cx.new(|cx| Root::new(panel, window, cx))
        })
        .map(|_| ())
        .map_err(|error| io::Error::other(error.to_string()))
    }

    #[cfg(test)]
    type FixtureLoader = fn(&mut Controller, &str) -> io::Result<async_channel::Receiver<Reply>>;

    pub(super) struct Panel {
        pub(super) browser: Option<folders::Browser>,
        pub(super) folder_serial: u64,
        pub(super) folder_task: Option<Task<()>>,
        pub(super) folder_cancel: Option<Arc<AtomicBool>>,
        pub(super) lister: folders::Lister,
        #[cfg(test)]
        fixture_loader: Option<FixtureLoader>,
        pub(super) controller: Controller,
        pub(super) content: Option<Entity<TextViewState>>,
        pub(super) prepared: Option<Prepared>,
        pub(super) notice: Option<String>,
        pub(super) landing: Option<usize>,
        pub(super) reply_task: Option<Task<()>>,
        pub(super) landing_task: Option<Task<()>>,
        pub(super) landing_serial: u64,
        pub(super) requested_source: Option<Source>,
        pub(super) edit: Option<editing::Editing>,
        pub(super) edit_task: Option<Task<()>>,
        pub(super) save_task: Option<Task<()>>,
        pub(super) write_cancel: Option<Arc<AtomicBool>>,
        pub(super) saver: editing::Saver,
        pub(super) recovery_root: Option<std::path::PathBuf>,
    }
    impl Panel {
        fn new() -> Self {
            Self {
                browser: None,
                folder_serial: 0,
                folder_task: None,
                folder_cancel: None,
                lister: folders::default_lister(),
                #[cfg(test)]
                fixture_loader: None,
                controller: Controller::default(),
                content: None,
                prepared: None,
                notice: None,
                landing: None,
                reply_task: None,
                landing_task: None,
                landing_serial: 0,
                requested_source: None,
                edit: None,
                edit_task: None,
                save_task: None,
                write_cancel: None,
                saver: editing::default_saver(),
                recovery_root: editing::recovery_root(),
            }
        }

        pub(super) fn open(
            &mut self,
            source: Source,
            heading: Option<String>,
            cx: &mut Context<Self>,
        ) {
            #[cfg(test)]
            if let Some(loader) = self.fixture_loader {
                self.open_with(source, heading, loader, cx);
                return;
            }
            self.open_with(source, heading, Controller::open, cx);
        }
        // The same window/worker publication path also accepts an offline
        // fixture loader; production always uses the existing SSH controller.
        fn open_with(
            &mut self,
            source: Source,
            heading: Option<String>,
            load: impl FnOnce(&mut Controller, &str) -> io::Result<async_channel::Receiver<Reply>>,
            cx: &mut Context<Self>,
        ) {
            if !self.protect(cx) {
                return;
            }
            self.revoke_folder();
            self.browser = None;
            if let Some(cancel) = self.write_cancel.take() {
                cancel.store(true, Ordering::Release);
            }
            self.save_task = None;
            self.edit_task = None;
            self.edit = None;
            self.landing_serial = self.landing_serial.wrapping_add(1);
            self.landing_task = None;
            self.landing = None;
            self.content = None;
            self.prepared = None;
            self.notice = None;
            self.requested_source = Some(source.clone());
            let receiver = match load(&mut self.controller, source.url()) {
                Ok(receiver) => receiver,
                Err(error) => {
                    self.notice = Some(error.to_string());
                    cx.notify();
                    return;
                }
            };
            self.reply_task = Some(cx.spawn(async move |this, cx| {
                if let Ok(reply) = receiver.recv().await {
                    let prepared = if let Ok(markdown) = &reply.result {
                        let markdown = markdown.clone();
                        Some(
                            cx.background_executor()
                                .spawn(async move { prepare(&markdown) })
                                .await,
                        )
                    } else {
                        None
                    };
                    let _ = this.update(cx, |this, cx| {
                        if !this.controller.receive(reply) {
                            return;
                        }
                        if let Some(prepared) = prepared {
                            let content = cx.new(|cx| {
                                TextViewState::markdown("", cx)
                                    .scrollable(true)
                                    .selectable(true)
                            });
                            this.prepared = Some(prepared);
                            this.content = Some(content.clone());
                            let (source, original) = match &this.controller.state {
                                State::Ready { source, markdown } => {
                                    (source.clone(), markdown.clone())
                                }
                                _ => return,
                            };
                            let view = this.markdown_view(
                                &content,
                                source,
                                this.controller.generation,
                                cx,
                            );
                            content.update(cx, |state, cx| {
                                view.prepare_state(state, cx);
                                state.set_text_with_source(
                                    &this.prepared.as_ref().unwrap().rendered,
                                    Some(original.into()),
                                    cx,
                                );
                            });
                            if let Some(heading) = heading {
                                this.heading(&heading, cx);
                            }
                        }
                        cx.notify();
                    });
                }
            }));
            cx.notify();
        }
        pub(super) fn stop(&mut self, cx: &mut Context<Self>) -> bool {
            if !self.protect(cx) {
                return false;
            }
            self.revoke_folder();
            if let Some(cancel) = self.write_cancel.take() {
                cancel.store(true, Ordering::Release);
            }
            self.save_task = None;
            self.edit_task = None;
            if let Some(browser) = &mut self.browser {
                if browser.result.is_none() {
                    browser.result = Some(Err(io::Error::new(
                        io::ErrorKind::Interrupted,
                        "Remote folder listing cancelled",
                    )));
                }
            }
            self.controller.cancel();
            self.reply_task = None;
            self.landing_serial = self.landing_serial.wrapping_add(1);
            self.landing_task = None;
            self.landing = None;
            self.notice = None;
            cx.notify();
            true
        }
        fn markdown_view(
            &self,
            content: &Entity<TextViewState>,
            origin: Source,
            generation: u64,
            cx: &Context<Self>,
        ) -> TextView {
            let weak = cx.entity().downgrade();
            markdown_plugins(
                TextView::new(content),
                Arc::new(move |url, _, _, cx| {
                    let _ = weak.update(cx, |this, cx| {
                        this.follow(&origin, generation, url, cx);
                    });
                }),
                Arc::new(|_| Some(MarkdownImage::Unavailable)),
                SelectionFormat::Plain,
            )
            .scrollable(true)
            .selectable(true)
            .style(reader_text_style(cx.theme()))
        }
        fn follow(&mut self, origin: &Source, generation: u64, url: &str, cx: &mut Context<Self>) {
            if !self.controller.is_current(origin, generation) {
                self.notice = Some("This link belongs to an inactive remote document.".into());
                cx.notify();
                return;
            }
            if let Some(landing) = self
                .prepared
                .as_ref()
                .and_then(|prepared| reader_obsidian::footnote_landing(url, &prepared.rendered))
            {
                match landing {
                    Ok(block) => self.land(block, cx),
                    Err(error) => self.notice = Some(error.into()),
                }
                cx.notify();
                return;
            }
            // is_current above proves that this captured origin is still the
            // controller's published source before resolving anything.
            match self.controller.resolve_link(url) {
                Ok((source, heading)) => {
                    let heading =
                        heading.map(|heading| okilum_core::document_links::decode(&heading));
                    if source.url() == origin.url() {
                        if let Some(heading) = heading {
                            self.heading(&heading, cx);
                        } else {
                            self.land(0, cx);
                        }
                    } else {
                        self.open(source, heading, cx);
                    }
                }
                Err(error) => self.notice = Some(error.to_string()),
            }
            cx.notify();
        }
        fn heading(&mut self, heading: &str, cx: &mut Context<Self>) {
            match self.prepared.as_ref().unwrap().headings.locate(heading) {
                Ok(target) => self.land(target.block, cx),
                Err(error) => {
                    self.landing_serial = self.landing_serial.wrapping_add(1);
                    self.landing_task = None;
                    self.landing = None;
                    self.notice = Some(error.reason().into());
                }
            }
        }
        fn land(&mut self, block: usize, cx: &mut Context<Self>) {
            self.landing_serial = self.landing_serial.wrapping_add(1);
            let serial = self.landing_serial;
            let generation = self.controller.generation;
            let content = self.content.as_ref().unwrap().clone();
            self.notice = None;
            self.landing = Some(block);
            self.landing_task = Some(cx.spawn(async move |this, cx| {
                for attempt in 0..100 {
                    cx.background_executor().timer(Duration::from_millis(50)).await;
                    let done = this.update(cx, |this, cx| {
                        let current = generation == this.controller.generation && serial == this.landing_serial;
                        let same = this.content.as_ref().is_some_and(|now| now.entity_id() == content.entity_id());
                        match reader_landing_state(current, same, content.read(cx).list_state().item_count(), block, attempt) {
                            ReaderLanding::Cancelled => true,
                            ReaderLanding::Waiting => { cx.notify(); false },
                            ReaderLanding::Failed => {
                                this.landing = None;
                                this.notice = Some("Heading landing failed: the remote document did not finish rendering. Try the link again.".into());
                                cx.notify(); true
                            }
                            ReaderLanding::Ready => {
                                content.update(cx, |state, cx| {
                                    state.list_state().scroll_to(ListOffset { item_ix: block, offset_in_item: px(0.) });
                                    cx.notify();
                                });
                                this.landing = None;
                                cx.notify(); true
                            }
                        }
                    }).unwrap_or(true);
                    if done { break; }
                }
            }));
        }
    }
    impl Drop for Panel {
        fn drop(&mut self) {
            if let Some(cancel) = &self.folder_cancel {
                cancel.store(true, Ordering::Release);
            }
            if let Some(cancel) = &self.write_cancel {
                cancel.store(true, Ordering::Release);
            }
        }
    }
    impl Render for Panel {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let loading = matches!(self.controller.state, State::Loading { .. });
            let mut body = v_flex().size_full().min_h_0()
                .child(TitleBar::new().on_close_window({
                    // GNOME's drawn close button stops like the platform close (#2805).
                    let panel = cx.entity().downgrade();
                    move |_, window, cx| {
                        crate::reader_diagnostics::window_close_requested(window, cx, "title bar close button");
                        if panel.update(cx, |panel, cx| panel.stop(cx)).unwrap_or(true) {
                            window.remove_window();
                        }
                    }
                }).child("Remote Markdown"))
                .child(h_flex().p_3().gap_2()
                    .child(div().flex_1().child(if self.browser.is_some(){"Remote folder · read-only listing".into()} else if self.edit.is_some() { "Editing a protected local draft · explicit Save only".into() } else { self.controller.state.message() }))
                    .child(self.folder_controls(cx))
                    .child(self.edit_controls(cx))
                    .child(Button::new("remote-markdown-stop").debug_selector(||"remote-markdown-stop".into()).small().label("Stop").disabled(!loading && self.landing.is_none() && !self.browser.as_ref().is_some_and(|b|b.result.is_none()))
                        .on_click(cx.listener(|this, _, _, cx| { this.stop(cx); }))))
                .child(div().px_3().text_sm().text_color(cx.theme().muted_foreground)
                    .child("2 MiB limit · Existing SSH alias and known host key required. Edit keeps a local draft; only Save/Retry writes. Safe saving requires Linux + Python 3. Remote media and HTML are not loaded."));
            if let Some(notice) = &self.notice {
                body = body.child(
                    div()
                        .p_3()
                        .text_color(brand::palette(cx).danger)
                        .child(notice.clone()),
                );
            }
            if self.landing.is_some() {
                body = body.child(div().px_3().child("Locating heading…"));
            }
            if let Some(folder) = self.folder_view(cx) {
                return body.child(folder);
            }
            if let Some(editor) = self.editor(cx) {
                return body.child(editor);
            }
            if let (Some(content), State::Ready { source, .. }) =
                (&self.content, &self.controller.state)
            {
                body = body.child(
                    div().flex_1().min_h_0().px(px(READER_SIDE_PADDING)).child(
                        self.markdown_view(content, source.clone(), self.controller.generation, cx)
                            .size_full(),
                    ),
                );
            }
            body
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use ::core::prelude::v1::test;
        struct Offline {
            bytes: Vec<u8>,
            ui_thread: std::thread::ThreadId,
            reads: Arc<std::sync::atomic::AtomicUsize>,
        }
        impl Transport for Offline {
            fn open(&mut self, _: &okilum_core::log::sftp::Target) -> io::Result<()> {
                assert_ne!(
                    std::thread::current().id(),
                    self.ui_thread,
                    "the existing controller must read on its background worker"
                );
                Ok(())
            }
            fn read_at(&mut self, offset: u64, limit: usize) -> io::Result<Vec<u8>> {
                self.reads.fetch_add(1, Ordering::Relaxed);
                let offset = offset as usize;
                Ok(self.bytes[offset..self.bytes.len().min(offset + limit.min(7))].to_vec())
            }
            fn disconnect(&mut self) {}
        }

        fn offline_reply(
            controller: &mut Controller,
            url: &str,
            markdown: &str,
            reads: Arc<std::sync::atomic::AtomicUsize>,
        ) -> io::Result<Reply> {
            // Wait only for this owned in-memory fixture, then queue its real
            // controller reply before the GPUI executor runs. No timing polls.
            controller
                .start(
                    Source::parse(url)?,
                    Offline {
                        bytes: markdown.as_bytes().to_vec(),
                        ui_thread: std::thread::current().id(),
                        reads,
                    },
                )
                .recv_blocking()
                .map_err(|error| io::Error::other(error.to_string()))
        }
        fn queued(reply: Reply) -> async_channel::Receiver<Reply> {
            let (send, receive) = async_channel::bounded(1);
            assert!(send.try_send(reply).is_ok());
            receive
        }
        fn fixture(cx: &mut TestAppContext) -> (Entity<Panel>, &mut VisualTestContext) {
            cx.update(crate::test_init);
            let mut panel = None;
            let (_, visual) = cx.add_window_view(|window, cx| {
                let entity = cx.new(|_| Panel::new());
                panel = Some(entity.clone());
                Root::new(entity, window, cx)
            });
            (panel.unwrap(), visual)
        }
        fn draw(visual: &mut VisualTestContext) {
            visual.run_until_parked();
            visual.update(|window, cx| window.draw(cx).clear(cx));
            visual.run_until_parked();
        }
        fn load(panel: &Entity<Panel>, visual: &mut VisualTestContext, url: &str, markdown: &str) {
            let reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            panel.update(visual, |panel, cx| {
                panel.open_with(
                    Source::parse(url).unwrap(),
                    None,
                    |controller, url| {
                        Ok(queued(offline_reply(
                            controller,
                            url,
                            markdown,
                            reads.clone(),
                        )?))
                    },
                    cx,
                );
            });
            draw(visual);
            assert!(
                reads.load(Ordering::Relaxed) > 1,
                "actual short-read worker positive control"
            );
        }

        include!("reader_remote_markdown/mounted_tests.rs");
        include!("reader_remote_markdown/folder_mounted_tests.rs");

        #[gpui::test]
        fn remote_window_offline_worker_renders_readonly_payload_and_lands_current_heading(
            cx: &mut TestAppContext,
        ) {
            let executor = cx.executor();
            let (panel, visual) = fixture(cx);
            let original = "---\ntitle: Remote\n---\n# Start\n\n## Part One\n\nbody e\u{301} שלום\n\n<img src=\"file:///private/a.png\">\n\n![remote](https://invalid.test/a.png)\n";
            load(&panel, visual, "ssh://alias/notes/start.md", original);
            let (source, generation, target) = panel.read_with(visual, |panel, cx| {
                let State::Ready { source, markdown } = &panel.controller.state else {
                    panic!("worker not accepted")
                };
                assert_eq!(markdown, original, "read-only canonical bytes remain exact");
                let prepared = panel.prepared.as_ref().unwrap();
                assert!(!prepared.rendered.contains("<img"));
                assert!(!prepared.rendered.contains("![remote](https:"));
                assert!(prepared
                    .rendered
                    .contains(okilum_core::render::UNAVAILABLE_IMAGE));
                let target = prepared.headings.locate("Part One").unwrap().block;
                let content = panel.content.as_ref().unwrap().read(cx);
                assert!(
                    content.list_state().item_count() > target,
                    "actual Markdown view rendered its blocks"
                );
                assert!(panel.controller.state.message().contains("Read-only"));
                assert_eq!(
                    panel.controller.resolve_link("../next.md").unwrap().0.url(),
                    "ssh://alias/next.md"
                );
                (source.clone(), panel.controller.generation, target)
            });
            panel.update(visual, |panel, cx| {
                panel.follow(&source, generation, "#Part%20One", cx)
            });
            panel.read_with(visual, |panel, _| assert_eq!(panel.landing, Some(target)));
            visual.run_until_parked();
            executor.advance_clock(Duration::from_millis(60));
            draw(visual);
            panel.read_with(visual, |panel, _| {
                assert!(
                    panel.landing.is_none() && panel.notice.is_none(),
                    "current heading completed actual list landing"
                );
                assert!(panel.controller.is_current(&source, generation));
            });
        }

        #[gpui::test]
        fn remote_window_replacement_and_stop_reject_old_response_and_link_authority(
            cx: &mut TestAppContext,
        ) {
            let (panel, visual) = fixture(cx);
            load(
                &panel,
                visual,
                "ssh://alias/a.md",
                "# Start\n\naccepted A\n",
            );
            let (old, old_generation) = panel.read_with(visual, |panel, _| {
                let State::Ready { source, .. } = &panel.controller.state else {
                    unreachable!()
                };
                (source.clone(), panel.controller.generation)
            });
            let reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let mut held = None;
            panel.update(visual, |panel, cx| {
                panel.open_with(
                    Source::parse("ssh://alias/b.md").unwrap(),
                    None,
                    |controller, url| {
                        let reply =
                            offline_reply(controller, url, "# B\n\nlate B\n", reads.clone())?;
                        let (send, receive) = async_channel::bounded(1);
                        held = Some((send, reply));
                        Ok(receive)
                    },
                    cx,
                );
                panel.follow(&old, old_generation, "#Start", cx);
                assert_eq!(
                    panel.notice.as_deref(),
                    Some("This link belongs to an inactive remote document.")
                );
            });
            draw(visual);
            panel.read_with(visual, |panel, _| {
                assert!(matches!(&panel.controller.state, State::Loading { target } if target == "ssh://alias/b.md"));
                assert!(panel.content.is_none());
            });
            load(
                &panel,
                visual,
                "ssh://alias/c.md",
                "# Current\n\naccepted C\n",
            );
            let (send, reply) = held.take().unwrap();
            assert!(
                send.try_send(reply).is_err(),
                "replacement dropped the old UI receiver"
            );
            panel.update(visual, |panel, cx| {
                panel.follow(&old, old_generation, "#Start", cx)
            });
            panel.read_with(visual, |panel, cx| {
                assert!(
                    matches!(&panel.controller.state, State::Ready { source, markdown }
                    if source.url() == "ssh://alias/c.md" && markdown.contains("accepted C"))
                );
                assert!(
                    panel
                        .content
                        .as_ref()
                        .unwrap()
                        .read(cx)
                        .list_state()
                        .item_count()
                        > 0
                );
                assert_eq!(
                    panel.notice.as_deref(),
                    Some("This link belongs to an inactive remote document.")
                );
            });
            let mut cancelled = None;
            panel.update(visual, |panel, cx| {
                panel.open_with(
                    Source::parse("ssh://alias/d.md").unwrap(),
                    None,
                    |controller, url| {
                        let reply = offline_reply(controller, url, "# D\n", reads)?;
                        cancelled = controller.cancel.clone();
                        let (send, receive) = async_channel::bounded(1);
                        held = Some((send, reply));
                        Ok(receive)
                    },
                    cx,
                );
                panel.stop(cx);
            });
            assert!(
                cancelled.unwrap().load(Ordering::Acquire),
                "Stop cancels the owned generation"
            );
            // Dropping a never-polled GPUI task schedules cancellation; its
            // captured receiver is released when the executor drains. Generation
            // revocation above is synchronous and already prevents publication.
            draw(visual);
            let (send, reply) = held.take().unwrap();
            assert!(
                send.try_send(reply).is_err(),
                "Stop dropped the pending UI receiver after executor cancellation"
            );
            panel.read_with(visual, |panel, _| {
                assert!(matches!(panel.controller.state, State::Cancelled));
                assert!(
                    panel.reply_task.is_none()
                        && panel.landing_task.is_none()
                        && panel.landing.is_none()
                );
            });
        }

        #[test]
        fn remote_preparation_and_heading_inventory_share_the_exact_rendered_source() {
            let prepared = prepare("---\ntitle: Remote\n---\n# Start\n\nSetext\n------\n\n## Duplicate\n\n## Duplicate\n\n<img src=\"file:///private/local.png\">\n\n![remote](file:///private/local.png)\n");
            assert!(prepared.headings.locate("Setext").is_ok());
            assert!(prepared.headings.locate("Missing").is_err());
            assert!(prepared.headings.locate("Duplicate").is_err());
            assert!(!prepared.rendered.contains("<img"));
            assert!(!prepared.rendered.contains("![remote](file:"));
            assert!(prepared
                .rendered
                .contains(okilum_core::render::UNAVAILABLE_IMAGE));
        }
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
    fn remote_html_is_literal_while_code_and_autolinks_keep_their_source() {
        let source = "<img src=\"file:///local\">\n<svg><image href=\"file:///local\"/></svg>\n<ssh://alias/a.md>\n`<img src=\"example\">`\n\n```html\n<img src=\"example\">\n```\n";
        let rendered = literal_remote_html(source);
        assert!(rendered.contains("&lt;img src="));
        assert!(!rendered.contains("<svg>"));
        assert!(!rendered.contains("<image href="));
        assert!(rendered.contains("<ssh://alias/a.md>"));
        assert!(rendered.contains("`<img src=\"example\">`"));
        assert!(rendered.contains("```html\n<img src=\"example\">\n```"));
    }
    #[test]
    fn reader_rewritten_url_recovers_only_exact_captured_ssh_identity() {
        let rendered = "okilum://unsupported/captured";
        let target = "ssh://alias/notes/a.md#Part%20One";
        assert_eq!(
            authored_ssh_url(rendered, [(rendered, false, target)])
                .unwrap()
                .as_deref(),
            Some(target)
        );
        assert!(authored_ssh_url("other", [(rendered, false, target)])
            .unwrap()
            .is_none());
        assert!(authored_ssh_url(rendered, [(rendered, true, target)])
            .unwrap()
            .is_none());
        assert!(
            authored_ssh_url(rendered, [(rendered, false, "https://host/a.md")])
                .unwrap()
                .is_none()
        );
        assert!(authored_ssh_url(
            rendered,
            [
                (rendered, false, target),
                (rendered, false, "ssh://other/a.md")
            ]
        )
        .is_err());
        assert_eq!(
            authored_ssh_url(target, []).unwrap().as_deref(),
            Some(target)
        );
    }
    #[test]
    fn explicit_ssh_route_preserves_heading_and_rejects_unsafe_targets() {
        assert!(is_ssh_url("ssh://alias/a.md#Part%20One"));
        assert!(is_ssh_url("SSH://alias/a.md"));
        assert!(!is_ssh_url("https://host/a.md"));
        assert!(!is_ssh_url("relative.md"));
        let (source, heading) = destination("ssh://alias/a.md#Part%20One").unwrap();
        assert_eq!(source.url(), "ssh://alias/a.md");
        assert_eq!(heading.as_deref(), Some("Part One"));
        for invalid in [
            "ssh://user@alias/a.md",
            "ssh://alias/a.md#",
            "ssh://alias/a.md#%20",
            "ssh://alias/a.md?x",
        ] {
            assert!(destination(invalid).is_err(), "{invalid}");
        }
    }
    #[test]
    fn captured_remote_origin_cannot_be_reused_after_replacement_or_cancel() {
        let mut controller = Controller::default();
        let source = Source::parse("ssh://alias/a.md").unwrap();
        controller.state = State::Ready {
            source: source.clone(),
            markdown: "# A".into(),
        };
        let generation = controller.generation;
        assert!(controller.is_current(&source, generation));
        assert!(!controller.is_current(&Source::parse("ssh://other/a.md").unwrap(), generation));
        controller.cancel();
        assert!(!controller.is_current(&source, generation));
        controller.state = State::Ready {
            source: source.clone(),
            markdown: "# New A".into(),
        };
        assert!(!controller.is_current(&source, generation));
        assert!(controller.is_current(&source, controller.generation));
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

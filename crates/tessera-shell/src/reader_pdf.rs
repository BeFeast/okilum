//! Inline PDF reading (#477, slice 1): read-only and page-virtualized.
//!
//! Every page slot is sized up front from the page dimensions, so the
//! scrollbar is right before any bitmap exists. Only the visible pages plus a
//! small margin are rasterized, on one worker thread per document, and the
//! bitmaps live in a byte-bounded cache. An evicted bitmap is also removed
//! from the GPU atlas: dropping the last `Arc<RenderImage>` frees only the
//! CPU copy (docs/research/477-inline-pdf.md §3.5).
use super::*;
use crate::pdf_engine::{self, OpenError, PageSize, PdfDocument};
use std::cell::Cell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::ops::Range;
use std::sync::{Condvar, Mutex};
use std::time::SystemTime;

/// Space between pages and above the first one.
const PAGE_GAP: f32 = 12.;
/// Space left and right of a page at fit width.
const SIDE_PADDING: f32 = 24.;
/// Pages rendered beyond each edge of the viewport.
const MARGIN_PAGES: usize = 1;
/// CPU bytes of page bitmaps kept per document; the GPU atlas mirrors them.
/// Pages the viewport needs are kept even when they alone exceed it.
const CACHE_BUDGET: usize = 128 << 20;
/// Larger files are not read into memory.
const MAX_FILE_BYTES: u64 = 512 << 20;
/// Zoom relative to fit width.
const ZOOM_STEPS: [f32; 12] = [
    0.5, 0.67, 0.75, 0.9, 1.0, 1.1, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0,
];
const FIT_STEP: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Zoom {
    In,
    Out,
    Fit,
}

/// The reading position of each PDF opened this session, keyed by path.
#[derive(Default)]
struct PdfPositions(HashMap<PathBuf, Position>);
impl Global for PdfPositions {}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Position {
    page: usize,
    /// How far into the page the viewport starts, as a share of its slot.
    fraction: f32,
    zoom: usize,
}

/// Page bitmaps by page index, one width per page, least recently used out
/// first. Generic so the policy is testable without a GPU.
pub(crate) struct PageCache<T> {
    budget: usize,
    bytes: usize,
    clock: u64,
    entries: Vec<CacheEntry<T>>,
}
struct CacheEntry<T> {
    page: usize,
    width: u32,
    bytes: usize,
    used: u64,
    value: T,
}
impl<T> PageCache<T> {
    pub(crate) fn new(budget: usize) -> Self {
        Self {
            budget,
            bytes: 0,
            clock: 0,
            entries: Vec::new(),
        }
    }
    #[cfg(test)]
    pub(crate) fn bytes(&self) -> usize {
        self.bytes
    }
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }
    pub(crate) fn contains(&self, page: usize, width: u32) -> bool {
        self.entries
            .iter()
            .any(|e| e.page == page && e.width == width)
    }
    /// The page's bitmap at whatever width it was last rendered.
    #[cfg(test)]
    pub(crate) fn get(&self, page: usize) -> Option<&T> {
        self.entries
            .iter()
            .find(|e| e.page == page)
            .map(|e| &e.value)
    }
    pub(crate) fn pages(&self) -> impl Iterator<Item = (usize, &T)> {
        self.entries.iter().map(|e| (e.page, &e.value))
    }
    pub(crate) fn touch(&mut self, pages: Range<usize>) {
        self.clock += 1;
        for entry in &mut self.entries {
            if pages.contains(&entry.page) {
                entry.used = self.clock;
            }
        }
    }
    /// Stores a page, replacing any other width of it, then evicts least
    /// recently used pages outside `keep` until the cache fits its budget.
    /// Returns everything that left the cache.
    pub(crate) fn insert(
        &mut self,
        page: usize,
        width: u32,
        bytes: usize,
        value: T,
        keep: Range<usize>,
    ) -> Vec<T> {
        let mut evicted = Vec::new();
        if let Some(ix) = self.entries.iter().position(|e| e.page == page) {
            evicted.push(self.remove(ix));
        }
        self.clock += 1;
        self.bytes += bytes;
        self.entries.push(CacheEntry {
            page,
            width,
            bytes,
            used: self.clock,
            value,
        });
        while self.bytes > self.budget {
            let Some(ix) = self
                .entries
                .iter()
                .enumerate()
                .filter(|(_, e)| !keep.contains(&e.page))
                .min_by_key(|(_, e)| e.used)
                .map(|(ix, _)| ix)
            else {
                break;
            };
            evicted.push(self.remove(ix));
        }
        evicted
    }
    pub(crate) fn clear(&mut self) -> Vec<T> {
        self.bytes = 0;
        self.entries.drain(..).map(|e| e.value).collect()
    }
    fn remove(&mut self, ix: usize) -> T {
        let entry = self.entries.swap_remove(ix);
        self.bytes -= entry.bytes;
        entry.value
    }
}

/// Pages that intersect a viewport of `viewport` height whose top edge is
/// `offset` into page `top`. `heights` are the full slot heights.
pub(crate) fn visible_pages(
    heights: &[f32],
    top: usize,
    offset: f32,
    viewport: f32,
) -> Range<usize> {
    if heights.is_empty() {
        return 0..0;
    }
    let start = top.min(heights.len() - 1);
    let mut end = start;
    let mut y = -offset;
    while end < heights.len() && (end == start || y < viewport) {
        y += heights[end];
        end += 1;
    }
    start..end
}

/// Visible pages first, top to bottom, then the margin nearest first,
/// favouring the reading direction.
pub(crate) fn render_order(visible: Range<usize>, count: usize, margin: usize) -> Vec<usize> {
    let mut order: Vec<usize> = visible.clone().collect();
    for step in 1..=margin {
        if visible.end + step - 1 < count {
            order.push(visible.end + step - 1);
        }
        if visible.start >= step {
            order.push(visible.start - step);
        }
    }
    order
}

fn keep_range(visible: &Range<usize>, count: usize) -> Range<usize> {
    visible.start.saturating_sub(MARGIN_PAGES)..(visible.end + MARGIN_PAGES).min(count)
}

#[derive(Clone, Debug, PartialEq)]
struct Revision {
    len: u64,
    modified: Option<SystemTime>,
}
impl Revision {
    fn read(path: &Path) -> std::io::Result<Self> {
        let meta = std::fs::metadata(path)?;
        Ok(Self {
            len: meta.len(),
            modified: meta.modified().ok(),
        })
    }
}

enum Event {
    Opened {
        sizes: Arc<[PageSize]>,
        revision: Revision,
    },
    Failed(OpenError),
    Page {
        page: usize,
        width: u32,
        image: Option<Arc<RenderImage>>,
    },
}

#[derive(Default)]
struct Requests {
    wanted: VecDeque<(usize, u32)>,
    in_flight: Option<(usize, u32)>,
    closed: bool,
}
#[derive(Default)]
struct Shared {
    requests: Mutex<Requests>,
    changed: Condvar,
}

/// One rendering thread per open document. Dropping the handle stops it
/// after the page in progress; nothing is queued behind a closed viewer.
struct Worker(Arc<Shared>);
impl Worker {
    fn start(path: PathBuf) -> anyhow::Result<(Self, async_channel::Receiver<Event>)> {
        let shared = Arc::new(Shared::default());
        let (events, receiver) = async_channel::bounded(2);
        let thread_shared = shared.clone();
        std::thread::Builder::new()
            .name("tessera-pdf".into())
            .spawn(move || run_worker(&path, &thread_shared, &events))?;
        Ok((Self(shared), receiver))
    }
    /// Replaces the queue. Pages already rendered or being rendered at the
    /// same width are left out by the caller and here respectively.
    fn want(&self, pages: Vec<(usize, u32)>) {
        let mut requests = self.0.requests.lock().unwrap_or_else(|e| e.into_inner());
        let in_flight = requests.in_flight;
        requests.wanted = pages
            .into_iter()
            .filter(|p| Some(*p) != in_flight)
            .collect();
        self.0.changed.notify_one();
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        let mut requests = self.0.requests.lock().unwrap_or_else(|e| e.into_inner());
        requests.closed = true;
        requests.wanted.clear();
        self.0.changed.notify_one();
    }
}

fn read_document(path: &Path) -> Result<(PdfDocument, Revision), OpenError> {
    // A file replaced while it is read is read again; a consistent snapshot
    // is what the viewer shows until the next change.
    for _ in 0..3 {
        let before = Revision::read(path).map_err(|_| OpenError::Unreadable)?;
        if before.len > MAX_FILE_BYTES {
            return Err(OpenError::Unreadable);
        }
        let bytes = std::fs::read(path).map_err(|_| OpenError::Unreadable)?;
        if Revision::read(path).ok().as_ref() == Some(&before) {
            return PdfDocument::open(bytes).map(|document| (document, before));
        }
    }
    Err(OpenError::Unreadable)
}

fn run_worker(path: &Path, shared: &Shared, events: &async_channel::Sender<Event>) {
    let document = match read_document(path) {
        Ok((document, revision)) => {
            let sizes = document.sizes().into();
            if events
                .send_blocking(Event::Opened { sizes, revision })
                .is_err()
            {
                return;
            }
            document
        }
        Err(error) => {
            let _ = events.send_blocking(Event::Failed(error));
            return;
        }
    };
    let renderer = document.renderer();
    loop {
        let (page, width) = {
            let mut requests = shared.requests.lock().unwrap_or_else(|e| e.into_inner());
            requests.in_flight = None;
            loop {
                if requests.closed {
                    return;
                }
                if let Some(next) = requests.wanted.pop_front() {
                    requests.in_flight = Some(next);
                    break next;
                }
                requests = shared
                    .changed
                    .wait(requests)
                    .unwrap_or_else(|e| e.into_inner());
            }
        };
        let image = renderer.render(page, width).and_then(|bitmap| {
            let buffer = image::RgbaImage::from_raw(bitmap.width, bitmap.height, bitmap.bgra)?;
            Some(Arc::new(RenderImage::new(vec![image::Frame::new(buffer)])))
        });
        if events
            .send_blocking(Event::Page { page, width, image })
            .is_err()
        {
            return;
        }
    }
}

enum State {
    Opening,
    Ready(Arc<[PageSize]>),
    Locked,
    Unreadable,
}

/// The page geometry of one frame: every page is scaled by the same factor,
/// so mixed page sizes keep their proportions and the widest page fits.
#[derive(Clone)]
struct Geometry {
    sizes: Arc<[PageSize]>,
    /// Logical pixels per PDF point.
    scale: f32,
}
impl Geometry {
    fn new(sizes: Arc<[PageSize]>, viewport_width: f32, zoom: f32) -> Self {
        let widest = sizes.iter().map(|s| s.width).fold(1., f32::max);
        let fit = (viewport_width - 2. * SIDE_PADDING).max(120.);
        Self {
            scale: fit * zoom / widest,
            sizes,
        }
    }
    fn page(&self, ix: usize) -> Size<Pixels> {
        let page = self.sizes[ix];
        size(
            px((page.width * self.scale).round()),
            px((page.height * self.scale).round()),
        )
    }
    fn slot_height(&self, ix: usize) -> f32 {
        let top = if ix == 0 { PAGE_GAP } else { 0. };
        top + f32::from(self.page(ix).height) + PAGE_GAP
    }
    fn content_width(&self) -> f32 {
        let widest = self.sizes.iter().map(|s| s.width).fold(1., f32::max);
        (widest * self.scale).round() + 2. * SIDE_PADDING
    }
    fn bitmap_width(&self, ix: usize, scale_factor: f32) -> u32 {
        ((self.sizes[ix].width * self.scale * scale_factor).round() as u32)
            .clamp(1, pdf_engine::MAX_SIDE)
    }
}

/// Reports the viewport size to the viewer as an absolute overlay; the page
/// list may be wider than the viewport when zoomed in.
#[derive(Clone)]
struct Pane {
    list: ListState,
    bounds: Rc<Cell<Bounds<Pixels>>>,
}
impl gpui_base::ScrollbarHandle for Pane {
    fn viewport_bounds(&self) -> Bounds<Pixels> {
        self.bounds.get()
    }
    fn offset(&self) -> Point<Pixels> {
        point(px(0.), self.list.scroll_px_offset_for_scrollbar().y)
    }
    fn set_offset(&self, offset: Point<Pixels>) {
        self.list.set_offset_from_scrollbar(point(px(0.), offset.y));
    }
    fn content_size(&self) -> Size<Pixels> {
        size(
            self.bounds.get().size.width,
            self.list.viewport_bounds().size.height + self.list.max_offset_for_scrollbar().y,
        )
    }
    fn start_drag(&self) {
        self.list.scrollbar_drag_started();
    }
    fn end_drag(&self) {
        self.list.scrollbar_drag_ended();
    }
}

pub(crate) struct PdfViewer {
    path: PathBuf,
    state: State,
    revision: Option<Revision>,
    pane: Pane,
    zoom: usize,
    /// Applied once the viewport width is known.
    restore: Option<Position>,
    /// The layout width the list was last measured at.
    measured: Option<(f32, usize)>,
    cache: PageCache<Arc<RenderImage>>,
    failed: HashSet<(usize, u32)>,
    /// Images that left the cache; removed from the atlas at the next frame,
    /// where a window is at hand.
    released: Vec<Arc<RenderImage>>,
    requested: Vec<(usize, u32)>,
    worker: Option<Worker>,
    _events: Option<Task<()>>,
}

impl PdfViewer {
    pub(crate) fn new(path: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.on_release_in(window, |this, window, _| {
            for image in this
                .cache
                .clear()
                .into_iter()
                .chain(this.released.drain(..))
            {
                let _ = window.drop_image(image);
            }
        })
        .detach();
        let mut this = Self {
            path,
            state: State::Opening,
            revision: None,
            pane: Pane {
                list: ListState::new(0, ListAlignment::Top, px(200.)),
                bounds: Rc::default(),
            },
            zoom: FIT_STEP,
            restore: None,
            measured: None,
            cache: PageCache::new(CACHE_BUDGET),
            failed: HashSet::new(),
            released: Vec::new(),
            requested: Vec::new(),
            worker: None,
            _events: None,
        };
        this.open(cx);
        this
    }

    fn open(&mut self, cx: &mut Context<Self>) {
        self.released.extend(self.cache.clear());
        self.failed.clear();
        self.requested.clear();
        self.measured = None;
        self.state = State::Opening;
        match Worker::start(self.path.clone()) {
            Ok((worker, events)) => {
                self.worker = Some(worker);
                self._events = Some(cx.spawn(async move |this, cx| {
                    while let Ok(event) = events.recv().await {
                        if this.update(cx, |this, cx| this.accept(event, cx)).is_err() {
                            break;
                        }
                    }
                }));
            }
            Err(_) => {
                self.worker = None;
                self._events = None;
                self.state = State::Unreadable;
            }
        }
        cx.notify();
    }

    #[cfg(test)]
    fn is_ready(&self) -> bool {
        matches!(self.state, State::Ready(_))
    }

    #[cfg(test)]
    pub(crate) fn zoom_step(&self) -> usize {
        self.zoom
    }

    /// False once the file turned out locked or unreadable.
    pub(crate) fn is_available(&self) -> bool {
        matches!(self.state, State::Opening | State::Ready(_))
    }

    /// Re-reads the file when it changed on disk; a no-op otherwise.
    pub(crate) fn check_revision(&mut self, cx: &mut Context<Self>) {
        let Some(known) = self.revision.clone() else {
            return;
        };
        let path = self.path.clone();
        cx.spawn(async move |this, cx| {
            let current = cx
                .background_executor()
                .spawn(async move { Revision::read(&path).ok() })
                .await;
            if current.as_ref() != Some(&known) {
                let _ = this.update(cx, |this, cx| {
                    if this.revision.as_ref() == Some(&known) {
                        this.remember_position(cx);
                        this.revision = None;
                        this.open(cx);
                    }
                });
            }
        })
        .detach();
    }

    pub(crate) fn zoom(&mut self, zoom: Zoom, cx: &mut Context<Self>) {
        let next = match zoom {
            Zoom::In => (self.zoom + 1).min(ZOOM_STEPS.len() - 1),
            Zoom::Out => self.zoom.saturating_sub(1),
            Zoom::Fit => FIT_STEP,
        };
        if next != self.zoom {
            self.zoom = next;
            cx.notify();
        }
    }

    fn accept(&mut self, event: Event, cx: &mut Context<Self>) {
        match event {
            Event::Opened { sizes, revision } => {
                self.revision = Some(revision);
                self.pane.list =
                    ListState::new(sizes.len(), ListAlignment::Top, px(200.)).measure_all();
                let remembered = cx
                    .try_global::<PdfPositions>()
                    .and_then(|positions| positions.0.get(&self.path))
                    .copied()
                    .filter(|p| p.page < sizes.len());
                if let Some(position) = remembered {
                    self.zoom = position.zoom.min(ZOOM_STEPS.len() - 1);
                }
                self.restore = remembered;
                self.state = State::Ready(sizes);
                cx.emit(AvailabilityChanged);
            }
            Event::Failed(error) => {
                self.state = match error {
                    OpenError::Locked => State::Locked,
                    OpenError::Unreadable => State::Unreadable,
                };
                cx.emit(AvailabilityChanged);
            }
            Event::Page { page, width, image } => {
                self.requested.retain(|r| *r != (page, width));
                let keep = self.keep();
                match image {
                    None => {
                        self.failed.insert((page, width));
                    }
                    // A page scrolled away while it rendered was never painted
                    // and has no atlas entry; it is simply dropped.
                    Some(image) if keep.contains(&page) => {
                        let bytes = image.as_bytes(0).map_or(0, <[u8]>::len);
                        let evicted = self.cache.insert(page, width, bytes, image, keep);
                        self.released.extend(evicted);
                    }
                    Some(_) => {}
                }
            }
        }
        cx.notify();
    }

    fn keep(&self) -> Range<usize> {
        match &self.state {
            State::Ready(sizes) => keep_range(&self.visible_now(sizes), sizes.len()),
            _ => 0..0,
        }
    }

    fn geometry(&self, sizes: &Arc<[PageSize]>) -> Option<Geometry> {
        let width = f32::from(self.pane.bounds.get().size.width);
        (width > 0.).then(|| Geometry::new(sizes.clone(), width, ZOOM_STEPS[self.zoom]))
    }

    fn visible_now(&self, sizes: &Arc<[PageSize]>) -> Range<usize> {
        let Some(geometry) = self.geometry(sizes) else {
            return 0..0;
        };
        let heights: Vec<f32> = (0..sizes.len())
            .map(|ix| geometry.slot_height(ix))
            .collect();
        let top = self.pane.list.logical_scroll_top();
        visible_pages(
            &heights,
            top.item_ix,
            f32::from(top.offset_in_item),
            f32::from(self.pane.bounds.get().size.height),
        )
    }

    fn remember_position(&self, cx: &mut App) {
        let State::Ready(sizes) = &self.state else {
            return;
        };
        let Some(geometry) = self.geometry(sizes) else {
            return;
        };
        if self.restore.is_some() {
            return;
        }
        let top = self.pane.list.logical_scroll_top();
        let page = top.item_ix.min(sizes.len() - 1);
        let position = Position {
            page,
            fraction: f32::from(top.offset_in_item) / geometry.slot_height(page),
            zoom: self.zoom,
        };
        let positions = &mut cx.default_global::<PdfPositions>().0;
        if positions.get(&self.path) != Some(&position) {
            positions.insert(self.path.clone(), position);
        }
    }

    fn render_unavailable(message: &'static str, cx: &App) -> AnyElement {
        v_flex()
            .id("pdf-unavailable")
            .p_6()
            .gap_3()
            .text_color(cx.theme().muted_foreground)
            .child(Icon::new(IconName::File).size(px(48.)))
            .child(message)
            .into_any_element()
    }

    fn render_pages(
        &mut self,
        sizes: Arc<[PageSize]>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let bounds = self.pane.bounds.clone();
        let measure = canvas(
            move |frame, window, _| {
                if bounds.get() != frame {
                    bounds.set(frame);
                    window.request_animation_frame();
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_full();
        let Some(geometry) = self.geometry(&sizes) else {
            return div().size_full().child(measure).into_any_element();
        };
        let state = self.pane.list.clone();
        let layout = (f32::from(self.pane.bounds.get().size.width), self.zoom);
        if self.measured != Some(layout) {
            if self.measured.is_some() {
                state.remeasure();
            }
            self.measured = Some(layout);
        }
        if let Some(position) = self.restore.take() {
            state.scroll_to(ListOffset {
                item_ix: position.page,
                offset_in_item: px(position.fraction * geometry.slot_height(position.page)),
            });
        }
        self.remember_position(cx);

        let visible = self.visible_now(&sizes);
        self.cache.touch(visible.clone());
        let scale_factor = window.scale_factor();
        let wanted: Vec<(usize, u32)> = render_order(visible, sizes.len(), MARGIN_PAGES)
            .into_iter()
            .map(|ix| (ix, geometry.bitmap_width(ix, scale_factor)))
            .filter(|&(ix, width)| {
                !self.cache.contains(ix, width) && !self.failed.contains(&(ix, width))
            })
            .collect();
        if wanted != self.requested {
            if let Some(worker) = &self.worker {
                worker.want(wanted.clone());
            }
            self.requested = wanted;
        }

        let images: Rc<HashMap<usize, Arc<RenderImage>>> = Rc::new(
            self.cache
                .pages()
                .map(|(ix, image)| (ix, image.clone()))
                .collect(),
        );
        let failed: Rc<HashSet<usize>> = Rc::new(self.failed.iter().map(|(ix, _)| *ix).collect());
        let muted = cx.theme().muted_foreground;
        let item_geometry = geometry.clone();
        let pages = list(state, move |ix, _, _| {
            let page_size = item_geometry.page(ix);
            let mut page = div()
                .debug_selector(move || format!("pdf-page-{ix}"))
                .flex_none()
                .w(page_size.width)
                .h(page_size.height)
                .bg(gpui::white());
            if let Some(image) = images.get(&ix) {
                page = page.child(
                    img(ImageSource::Render(image.clone()))
                        .size_full()
                        .object_fit(ObjectFit::Fill),
                );
            } else if failed.contains(&ix) {
                page = page.child(
                    h_flex()
                        .size_full()
                        .justify_center()
                        .text_color(muted)
                        .child(Icon::new(IconName::File).size(px(32.))),
                );
            }
            h_flex()
                .w_full()
                .justify_center()
                .px(px(SIDE_PADDING))
                .when(ix == 0, |row| row.pt(px(PAGE_GAP)))
                .pb(px(PAGE_GAP))
                .child(page)
                .into_any_element()
        })
        .w(px(geometry
            .content_width()
            .max(f32::from(self.pane.bounds.get().size.width))))
        .h_full();

        div()
            .id("pdf-viewer")
            .debug_selector(|| "pdf-viewer".into())
            .relative()
            .size_full()
            .overflow_hidden()
            .bg(cx.theme().secondary)
            .child(measure)
            .child(
                div()
                    .id("pdf-pan")
                    .size_full()
                    .overflow_x_scroll()
                    // Vertical wheel motion belongs to the page list, never to panning.
                    .restrict_scroll_to_axis()
                    .child(pages),
            )
            .child(gpui_base::Scrollbar::vertical(&self.pane))
            .into_any_element()
    }
}

/// The document opened or failed to; the header's zoom controls follow it.
pub(crate) struct AvailabilityChanged;
impl EventEmitter<AvailabilityChanged> for PdfViewer {}

impl Render for PdfViewer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        for image in self.released.drain(..) {
            let _ = window.drop_image(image);
        }
        let content = match &self.state {
            State::Opening => div().into_any_element(),
            State::Ready(sizes) => self.render_pages(sizes.clone(), window, cx),
            State::Locked => Self::render_unavailable(
                if cfg!(target_os = "macos") {
                    "This PDF is password-protected. Press Space for Quick Look, or open it in another app."
                } else {
                    "This PDF is password-protected. Open it with the default app."
                },
                cx,
            ),
            State::Unreadable => Self::render_unavailable(
                if cfg!(target_os = "macos") {
                    "This PDF can't be shown here. Press Space for Quick Look, or open it in another app."
                } else {
                    "This PDF can't be shown here. Open it with the default app."
                },
                cx,
            ),
        };
        div().size_full().child(content)
    }
}

pub(crate) fn is_pdf(rel: &str) -> bool {
    Path::new(rel)
        .extension()
        .and_then(|s| s.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("pdf"))
}

impl Reader {
    pub(crate) fn pdf_viewer(&self) -> Option<&Entity<PdfViewer>> {
        self.file_preview.as_ref()?.pdf.as_ref()
    }

    pub(crate) fn pdf_zoom(&mut self, zoom: Zoom, cx: &mut Context<Self>) {
        if let Some(viewer) = self.pdf_viewer().cloned() {
            viewer.update(cx, |viewer, cx| viewer.zoom(zoom, cx));
        }
    }

    /// Glyph controls for the document header while a PDF is shown.
    pub(crate) fn render_pdf_controls(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        // Shown while the file opens too, so the header does not shift when
        // the first page arrives; hidden only when there is nothing to zoom.
        let viewer = self.pdf_viewer()?;
        if !viewer.read(cx).is_available() {
            return None;
        }
        let mac = cfg!(target_os = "macos");
        let buttons = [
            (
                "pdf-zoom-out",
                IconName::Minus,
                if mac {
                    "Zoom out ⌘−"
                } else {
                    "Zoom out Ctrl+−"
                },
                Zoom::Out,
            ),
            (
                "pdf-zoom-fit",
                IconName::RotateCw,
                if mac {
                    "Fit to width ⌘0"
                } else {
                    "Fit to width Ctrl+0"
                },
                Zoom::Fit,
            ),
            (
                "pdf-zoom-in",
                IconName::Plus,
                if mac {
                    "Zoom in ⌘+"
                } else {
                    "Zoom in Ctrl++"
                },
                Zoom::In,
            ),
        ];
        Some(
            h_flex()
                .gap_1()
                .children(buttons.map(|(id, icon, tip, zoom)| {
                    reader_icon_button(id, icon, tip, cx)
                        .debug_selector(move || id.into())
                        .on_click(cx.listener(move |this, _, _, cx| this.pdf_zoom(zoom, cx)))
                })),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pdf_engine::fixture;
    use ::core::prelude::v1::test;

    #[test]
    fn cache_evicts_least_recent_outside_the_kept_pages() {
        let mut cache = PageCache::new(300);
        assert!(cache.insert(0, 10, 100, "p0", 0..10).is_empty());
        assert!(cache.insert(1, 10, 100, "p1", 0..10).is_empty());
        assert!(cache.insert(2, 10, 100, "p2", 0..10).is_empty());
        assert_eq!(cache.bytes(), 300);
        // Page 0 is used again, so page 1 is the least recent.
        cache.touch(0..1);
        assert_eq!(cache.insert(3, 10, 100, "p3", 3..4), vec!["p1"]);
        assert_eq!(cache.bytes(), 300);
        assert!(cache.get(1).is_none() && cache.get(0).is_some());
        // Kept pages survive even over budget; the cache never drops what the
        // viewport needs.
        assert!(cache.insert(4, 10, 500, "big", 0..5).is_empty());
        assert_eq!(cache.bytes(), 800);
        let evicted = cache.insert(5, 10, 10, "p5", 4..6);
        assert_eq!(evicted.len(), 3, "everything outside 4..6 leaves");
        assert_eq!(cache.bytes(), 510);
        assert_eq!(cache.len(), 2);
    }

    #[test]
    fn cache_replaces_a_page_at_a_new_width() {
        let mut cache = PageCache::new(1000);
        cache.insert(7, 800, 100, "narrow", 0..10);
        assert!(cache.contains(7, 800));
        assert_eq!(cache.insert(7, 1600, 400, "wide", 0..10), vec!["narrow"]);
        assert!(!cache.contains(7, 800) && cache.contains(7, 1600));
        assert_eq!(cache.get(7), Some(&"wide"));
        assert_eq!(cache.bytes(), 400);
        assert_eq!(cache.clear(), vec!["wide"]);
        assert_eq!((cache.bytes(), cache.len()), (0, 0));
    }

    #[test]
    fn visible_range_and_render_order() {
        let heights = [100., 100., 100., 100., 100.];
        assert_eq!(visible_pages(&heights, 0, 0., 150.), 0..2);
        assert_eq!(visible_pages(&heights, 1, 50., 100.), 1..3);
        assert_eq!(visible_pages(&heights, 1, 0., 100.), 1..2);
        assert_eq!(visible_pages(&heights, 4, 0., 1000.), 4..5);
        assert_eq!(visible_pages(&heights, 9, 0., 100.), 4..5);
        assert_eq!(visible_pages(&[], 0, 0., 100.), 0..0);
        assert_eq!(render_order(2..4, 5, 1), vec![2, 3, 4, 1]);
        assert_eq!(render_order(0..2, 2, 1), vec![0, 1]);
        assert_eq!(keep_range(&(2..4), 5), 1..5);
    }

    fn recv(events: &async_channel::Receiver<Event>) -> Event {
        let start = std::time::Instant::now();
        loop {
            match events.try_recv() {
                Ok(event) => return event,
                Err(async_channel::TryRecvError::Empty) => {
                    assert!(
                        start.elapsed() < Duration::from_secs(60),
                        "worker timed out"
                    );
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("worker stopped: {error}"),
            }
        }
    }

    #[test]
    fn worker_renders_requested_pages_and_stops_when_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("doc.pdf");
        let pages: Vec<_> = (0..6).map(|_| (300., 400., [0.2, 0.4, 0.8])).collect();
        std::fs::write(&path, fixture::pdf(&pages)).unwrap();
        let (worker, events) = Worker::start(path).unwrap();
        let Event::Opened { sizes, .. } = recv(&events) else {
            panic!("expected the document to open");
        };
        assert_eq!(sizes.len(), 6);
        worker.want(vec![(4, 150), (0, 300)]);
        for expected in [(4, 150, 200), (0, 300, 400)] {
            let Event::Page { page, width, image } = recv(&events) else {
                panic!("expected a page");
            };
            let image = image.expect("page renders");
            assert_eq!((page, width), (expected.0, expected.1));
            assert_eq!(
                (image.size(0).width.0 as u32, image.size(0).height.0 as u32),
                (expected.1, expected.2)
            );
        }
        // Dropping the handle ends the thread, which closes the channel.
        drop(worker);
        let start = std::time::Instant::now();
        while !events.is_closed() {
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "worker kept running"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn worker_reports_unreadable_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("broken.pdf");
        std::fs::write(&path, b"%PDF-1.4 nothing else").unwrap();
        let (_worker, events) = Worker::start(path).unwrap();
        assert!(matches!(
            recv(&events),
            Event::Failed(OpenError::Unreadable)
        ));
        let (_worker, events) = Worker::start(dir.path().join("missing.pdf")).unwrap();
        assert!(matches!(
            recv(&events),
            Event::Failed(OpenError::Unreadable)
        ));
    }

    struct Host {
        viewer: Option<Entity<PdfViewer>>,
    }
    impl Render for Host {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .size_full()
                .debug_selector(|| "pdf-host".into())
                .children(self.viewer.clone())
        }
    }

    fn draw(visual: &mut gpui::VisualTestContext) {
        for _ in 0..3 {
            visual.run_until_parked();
            visual.update(|window, cx| {
                window.simulate_next_frame(cx);
                window.draw(cx).clear(cx);
            });
        }
    }

    /// Waits for the worker thread, which runs outside the test executor.
    fn settle(
        visual: &mut gpui::VisualTestContext,
        viewer: &Entity<PdfViewer>,
        done: impl Fn(&PdfViewer) -> bool,
    ) {
        let start = std::time::Instant::now();
        loop {
            draw(visual);
            if viewer.read_with(visual, |viewer, _| done(viewer)) {
                return;
            }
            assert!(
                start.elapsed() < Duration::from_secs(60),
                "viewer did not settle"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn atlas_has(visual: &mut gpui::VisualTestContext, image: &Arc<RenderImage>) -> bool {
        visual.update(|window, _| window.has_image_atlas_entry(image))
    }

    #[gpui::test]
    fn pages_are_sized_up_front_virtualized_and_released(cx: &mut TestAppContext) {
        // The worker is a real thread; its wake-ups are not test-scheduled.
        cx.executor().allow_parking();
        cx.update(gpui_component::init);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("book.pdf");
        let pages: Vec<_> = (0..40)
            .map(|ix| (612., 792., [ix as f32 / 40., 0.5, 0.5]))
            .collect();
        std::fs::write(&path, fixture::pdf(&pages)).unwrap();
        let viewer_path = path.clone();
        let (host, visual) = cx.add_window_view(|window, cx| Host {
            viewer: Some(cx.new(|cx| PdfViewer::new(viewer_path, window, cx))),
        });
        let viewer = host.read_with(visual, |host, _| host.viewer.clone().unwrap());
        settle(visual, &viewer, |v| v.is_ready() && v.measured.is_some());
        let pane = visual.debug_bounds("pdf-viewer").unwrap();
        let expected_width = f32::from(pane.size.width) - 2. * SIDE_PADDING;
        // Every slot has its final size before any bitmap arrives; the last
        // page is sized although it was never rendered.
        let first = visual.debug_bounds("pdf-page-0").unwrap();
        assert!(
            (f32::from(first.size.width) - expected_width).abs() < 1.5,
            "{first:?}"
        );
        assert!(
            (f32::from(first.size.height) - expected_width * 792. / 612.).abs() < 1.5,
            "{first:?}"
        );
        settle(visual, &viewer, |v| {
            v.requested.is_empty() && v.cache.len() > 0
        });
        let (cached, first_image) = viewer.read_with(visual, |v, _| {
            (
                v.cache.pages().map(|(ix, _)| ix).collect::<Vec<_>>(),
                v.cache.get(0).cloned().unwrap(),
            )
        });
        assert!(cached.contains(&0) && cached.contains(&1), "{cached:?}");
        assert!(
            cached.iter().all(|&ix| ix < 4),
            "only visible pages and the margin render: {cached:?}"
        );
        // Positive control: the painted first page is in the atlas.
        assert!(atlas_has(visual, &first_image));

        // Jump far ahead with a small budget: the old pages leave the cache
        // and the atlas, the new ones arrive.
        viewer.update(visual, |v, cx| {
            v.cache.budget = 1;
            v.pane.list.scroll_to(ListOffset {
                item_ix: 30,
                offset_in_item: px(0.),
            });
            cx.notify();
        });
        settle(visual, &viewer, |v| {
            v.cache.get(30).is_some() && v.requested.is_empty()
        });
        draw(visual);
        let cached = viewer.read_with(visual, |v, _| {
            v.cache.pages().map(|(ix, _)| ix).collect::<Vec<_>>()
        });
        assert!(
            cached.iter().all(|&ix| (29..34).contains(&ix)),
            "{cached:?}"
        );
        assert!(
            !atlas_has(visual, &first_image),
            "evicted page left the atlas"
        );
        let current = viewer.read_with(visual, |v, _| v.cache.get(30).cloned().unwrap());
        assert!(atlas_has(visual, &current));

        // Zoom changes the slot width and keeps the reading position.
        viewer.update(visual, |v, cx| v.zoom(Zoom::In, cx));
        settle(visual, &viewer, |v| v.requested.is_empty());
        let zoomed = visual.debug_bounds("pdf-page-30").unwrap();
        assert!(
            (f32::from(zoomed.size.width) - expected_width * 1.1).abs() < 1.5,
            "{zoomed:?}"
        );
        assert_eq!(
            viewer.read_with(visual, |v, _| v.pane.list.logical_scroll_top().item_ix),
            30
        );

        // Leaving the PDF stops the worker and releases the atlas.
        let weak = viewer.downgrade();
        let shared = viewer.read_with(visual, |v, _| v.worker.as_ref().unwrap().0.clone());
        drop(viewer);
        host.update(visual, |host, cx| {
            host.viewer = None;
            cx.notify();
        });
        draw(visual);
        assert!(
            weak.upgrade().is_none(),
            "leaving the PDF releases the viewer"
        );
        assert!(shared.requests.lock().unwrap().closed);
        assert!(
            !atlas_has(visual, &current),
            "release removes atlas entries"
        );

        // Opening the same document again restores page and zoom.
        let viewer_path = path.clone();
        host.update_in(visual, |host, window, cx| {
            host.viewer = Some(cx.new(|cx| PdfViewer::new(viewer_path, window, cx)));
            cx.notify();
        });
        let viewer = host.read_with(visual, |host, _| host.viewer.clone().unwrap());
        settle(visual, &viewer, |v| v.is_ready() && v.measured.is_some());
        draw(visual);
        viewer.read_with(visual, |v, _| {
            assert_eq!(v.zoom, FIT_STEP + 1);
            assert_eq!(v.pane.list.logical_scroll_top().item_ix, 30);
        });
    }

    #[gpui::test]
    fn changed_file_reopens_and_broken_file_says_so(cx: &mut TestAppContext) {
        // The worker is a real thread; its wake-ups are not test-scheduled.
        cx.executor().allow_parking();
        cx.update(gpui_component::init);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("doc.pdf");
        std::fs::write(&path, fixture::pdf(&[(300., 300., [1., 0., 0.])])).unwrap();
        let viewer_path = path.clone();
        let (host, visual) = cx.add_window_view(|window, cx| Host {
            viewer: Some(cx.new(|cx| PdfViewer::new(viewer_path, window, cx))),
        });
        let viewer = host.read_with(visual, |host, _| host.viewer.clone().unwrap());
        settle(visual, &viewer, |v| v.is_ready());
        let pages = |v: &PdfViewer| match &v.state {
            State::Ready(sizes) => sizes.len(),
            _ => 0,
        };
        assert_eq!(viewer.read_with(visual, |v, _| pages(v)), 1);
        // Negative control: an unchanged file is not reopened.
        viewer.update(visual, |v, cx| v.check_revision(cx));
        draw(visual);
        assert!(viewer.read_with(visual, |v, _| v.is_ready()));
        std::fs::write(
            &path,
            fixture::pdf(&[(300., 300., [1., 0., 0.]), (300., 300., [0., 1., 0.])]),
        )
        .unwrap();
        viewer.update(visual, |v, cx| v.check_revision(cx));
        settle(visual, &viewer, |v| pages(v) == 2);

        std::fs::write(&path, b"no longer a PDF").unwrap();
        viewer.update(visual, |v, cx| v.check_revision(cx));
        settle(visual, &viewer, |v| matches!(v.state, State::Unreadable));
        assert!(visual.debug_bounds("pdf-viewer").is_none());
    }

    fn book(pages: usize) -> Vec<u8> {
        let pages: Vec<_> = (0..pages)
            .map(|ix| {
                if ix % 10 == 9 {
                    (792., 612., [0.9, 0.9, 0.6])
                } else {
                    (612., 792., [0.95, 0.95, 0.95])
                }
            })
            .collect();
        fixture::pdf(&pages)
    }

    #[test]
    fn three_hundred_pages_open_and_render_anywhere() {
        let document = PdfDocument::open(book(300)).unwrap();
        assert_eq!(document.sizes().len(), 300);
        assert_eq!(document.sizes()[9].width, 792.);
        let renderer = document.renderer();
        for page in [0, 149, 299] {
            let bitmap = renderer.render(page, 400).unwrap();
            assert_eq!(bitmap.width, 400);
        }
    }

    /// Full sweep of a 300-page document through the worker and the cache,
    /// as a fast scroll would drive it. Slow without optimizations:
    /// `cargo test -p tessera-shell --release -- --ignored three_hundred`.
    #[test]
    #[ignore]
    fn three_hundred_page_sweep_stays_within_the_cache_budget() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("book.pdf");
        std::fs::write(&path, book(300)).unwrap();
        let (worker, events) = Worker::start(path).unwrap();
        let Event::Opened { sizes, .. } = recv(&events) else {
            panic!("expected the document to open");
        };
        let budget = 24 << 20;
        let mut cache = PageCache::new(budget);
        let mut largest = 0;
        let mut peak = 0;
        for top in 0..sizes.len() {
            let visible = top..(top + 2).min(sizes.len());
            let keep = keep_range(&visible, sizes.len());
            let wanted: Vec<_> = render_order(visible, sizes.len(), MARGIN_PAGES)
                .into_iter()
                .map(|ix| (ix, 1600))
                .filter(|&(ix, width)| !cache.contains(ix, width))
                .collect();
            let count = wanted.len();
            worker.want(wanted);
            for _ in 0..count {
                let Event::Page { page, width, image } = recv(&events) else {
                    panic!("expected a page");
                };
                let image = image.unwrap_or_else(|| panic!("page {page} renders"));
                let bytes = image.as_bytes(0).unwrap().len();
                largest = largest.max(bytes);
                cache.insert(page, width, bytes, image, keep.clone());
                peak = peak.max(cache.bytes());
            }
        }
        assert!(cache.contains(299, 1600));
        assert!(
            peak <= budget + 4 * largest,
            "peak {peak} bytes over a {budget} byte budget"
        );
    }
}

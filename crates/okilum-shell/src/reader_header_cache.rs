//! Board/Manager geometry changes do not rebuild the fixed Reader controls.
use super::*;

pub(super) struct Header {
    parent: WeakEntity<Reader>,
    available: f32,
    viewport_width: f32,
    scale_factor: f32,
    theme_mode: gpui_component::ThemeMode,
    _source: Option<Subscription>,
    #[cfg(test)]
    pub(crate) render_calls: std::cell::Cell<usize>,
}
impl Header {
    pub fn new(parent: WeakEntity<Reader>, cx: &mut Context<Self>) -> Self {
        let source = parent
            .upgrade()
            .map(|parent| cx.observe(&parent, |_, _, cx| cx.notify()));
        Self {
            parent,
            available: 0.,
            viewport_width: 0.,
            scale_factor: 0.,
            theme_mode: gpui_component::ThemeMode::Light,
            _source: source,
            #[cfg(test)]
            render_calls: std::cell::Cell::new(0),
        }
    }
    pub fn sync(
        &mut self,
        available: f32,
        viewport_width: f32,
        scale_factor: f32,
        theme_mode: gpui_component::ThemeMode,
        cx: &mut Context<Self>,
    ) {
        if self.available != available
            || self.viewport_width != viewport_width
            || self.scale_factor != scale_factor
            || self.theme_mode != theme_mode
        {
            self.available = available;
            self.viewport_width = viewport_width;
            self.scale_factor = scale_factor;
            self.theme_mode = theme_mode;
            cx.notify();
        }
    }
}
impl Render for Header {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(test)]
        self.render_calls.set(self.render_calls.get() + 1);
        #[cfg(feature = "project-ui-harness")]
        let _work = crate::project::performance::Work::start(29);
        let available = self.available;
        let root = self
            .parent
            .update(cx, |reader, cx| reader.render_header(available, window, cx))
            .unwrap_or_else(|_| div().into_any_element());
        #[cfg(feature = "project-ui-harness")]
        let root = crate::project::performance::Trace::new(root, 5).into_any_element();
        root
    }
}
impl Reader {
    pub(super) fn board_manager_header_cache(&self, available: f32) -> bool {
        self.project_navigation.active.is_some()
            && self.project_navigation.board_visible
            && self
                .panels
                .visible(reader_layout::Panel::Backlinks, available)
    }
}

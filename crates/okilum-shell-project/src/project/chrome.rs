//! Fixed Manager identity/navigation chrome stays independent of Feed redraws.
use super::*;

pub struct Chrome {
    parent: WeakEntity<ProjectView>,
    _source: Option<Subscription>,
    #[cfg(any(test, feature = "test-support"))]
    pub render_calls: std::cell::Cell<usize>,
}
impl Chrome {
    pub fn new(parent: WeakEntity<ProjectView>, cx: &mut Context<Self>) -> Self {
        let source = parent
            .upgrade()
            .map(|parent| cx.observe(&parent, |_, _, cx| cx.notify()));
        Self {
            parent,
            _source: source,
            #[cfg(any(test, feature = "test-support"))]
            render_calls: std::cell::Cell::new(0),
        }
    }
}
impl Render for Chrome {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(any(test, feature = "test-support"))]
        self.render_calls.set(self.render_calls.get() + 1);
        #[cfg(feature = "project-ui-harness")]
        let _work = super::performance::Work::start(28);
        let root = self
            .parent
            .update(cx, |parent, cx| parent.render_chrome(cx))
            .unwrap_or_else(|_| div().into_any_element());
        #[cfg(feature = "project-ui-harness")]
        let root = super::performance::Trace::new(root, 4).into_any_element();
        root
    }
}

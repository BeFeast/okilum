//! Native-only inclusive subtree timing; never changes the frame budget.
use super::*;

thread_local! {
    static WORK: std::cell::RefCell<[(u64, u64); 14]> = const { std::cell::RefCell::new([(0, 0); 14]) };
}
pub(super) fn reset() {
    WORK.with(|work| *work.borrow_mut() = [(0, 0); 14]);
}
pub(super) fn report() -> serde_json::Value {
    let names = [
        "code_layout",
        "code_prepaint",
        "code_paint",
        "tree_layout",
        "tree_prepaint",
        "tree_paint",
        "row_layout",
        "row_prepaint",
        "row_paint",
        "label_layout",
        "label_prepaint",
        "label_paint",
        "code_render",
        "row_build",
    ];
    WORK.with(|work| {
        names
            .into_iter()
            .zip(work.borrow().iter())
            .map(|(name, (calls, nanos))| {
                (
                    name.to_owned(),
                    serde_json::json!({"calls": calls, "ms": *nanos as f64 / 1_000_000.}),
                )
            })
            .collect::<serde_json::Map<_, _>>()
            .into()
    })
}
pub(in super::super) struct Work(usize, Instant);
impl Work {
    pub(in super::super) fn start(kind: usize) -> Self {
        Self(kind, Instant::now())
    }
}
impl Drop for Work {
    fn drop(&mut self) {
        let elapsed = self.1.elapsed();
        WORK.with(|work| {
            let mut work = work.borrow_mut();
            work[self.0].0 += 1;
            work[self.0].1 += elapsed.as_nanos() as u64;
        });
        if self.0 == 11
            && elapsed.as_millis() >= 5
            && std::env::var_os("OKILUM_CODE_TOOLBAR_PHASE_TRACE").is_some()
        {
            diagnostic_println!(
                "CODE2810_NATIVE_LABEL_PAINT ms={}",
                elapsed.as_secs_f64() * 1000.
            );
        }
    }
}
pub(in super::super) struct Trace(AnyElement, usize);
impl Trace {
    pub(in super::super) fn new(child: AnyElement, kind: usize) -> Self {
        Self(child, kind)
    }
}
impl IntoElement for Trace {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}
impl gpui::Element for Trace {
    type RequestLayoutState = ();
    type PrepaintState = ();
    fn id(&self) -> Option<ElementId> {
        None
    }
    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }
    fn request_layout(
        &mut self,
        _: Option<&gpui::GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (gpui::LayoutId, ()) {
        let _work = Work::start(self.1 * 3);
        (self.0.request_layout(window, cx), ())
    }
    fn prepaint(
        &mut self,
        _: Option<&gpui::GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let _work = Work::start(self.1 * 3 + 1);
        self.0.prepaint(window, cx);
    }
    fn paint(
        &mut self,
        _: Option<&gpui::GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let _work = Work::start(self.1 * 3 + 2);
        let glyphs = (self.1 == 1 && std::env::var_os("OKILUM_CODE_TOOLBAR_PHASE_TRACE").is_some())
            .then(platform::text_diagnostics::Probe::start);
        self.0.paint(window, cx);
        if let Some(glyphs) = glyphs {
            let calls = glyphs.report()["work"]["glyph_bitmap"]["calls"]
                .as_u64()
                .unwrap();
            if calls > 0 {
                diagnostic_println!("CODE2810_TREE_GLYPH_BITMAPS calls={calls}");
            }
        }
    }
}

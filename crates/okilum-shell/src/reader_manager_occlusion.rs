//! Native child views cannot participate in Metal's overlay stacking order.
//! Keep the largest rectangular part that no GPUI overlay covers; restore the
//! original frame on the first paint with no intersecting overlays.
use gpui::{point, px, size, Bounds, Pixels};

pub(super) fn uncovered_bounds(
    full: Bounds<Pixels>,
    overlays: &[Bounds<Pixels>],
) -> Option<Bounds<Pixels>> {
    let obstacles: Vec<_> = overlays
        .iter()
        .map(|bounds| bounds.intersect(&full))
        .filter(|bounds| bounds.size.width > px(0.) && bounds.size.height > px(0.))
        .collect();
    if full.size.width <= px(0.) || full.size.height <= px(0.) {
        return None;
    }
    if obstacles.is_empty() {
        return Some(full);
    }
    // Every maximal free rectangle has x edges at the frame or an obstacle.
    // For each pair, merge blocked y intervals and choose its largest free gap.
    let mut xs = vec![full.left(), full.right()];
    for bounds in &obstacles {
        xs.extend([bounds.left(), bounds.right()]);
    }
    xs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    xs.dedup();
    let mut best = None;
    let mut best_area = 0.;
    for (i, left) in xs.iter().enumerate() {
        for right in &xs[i + 1..] {
            let mut blocked: Vec<_> = obstacles
                .iter()
                .filter(|r| r.left() < *right && r.right() > *left)
                .map(|r| (r.top(), r.bottom()))
                .collect();
            blocked.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
            blocked.push((full.bottom(), full.bottom()));
            let mut top = full.top();
            for (start, end) in blocked {
                let width = *right - *left;
                let height = start - top;
                let area = f32::from(width) * f32::from(height);
                if height > px(0.) && area > best_area {
                    best_area = area;
                    best = Some(Bounds::new(point(*left, top), size(width, height)));
                }
                top = top.max(end);
            }
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    fn rect(x: f32, y: f32, w: f32, h: f32) -> Bounds<Pixels> {
        Bounds::new(point(px(x), px(y)), size(px(w), px(h)))
    }
    #[test]
    fn intersection_bookkeeping_restores_after_last_overlay_closes() {
        let full = rect(0., 0., 100., 100.);
        let dialog = rect(0., 0., 60., 100.);
        let toast = rect(60., 60., 40., 40.);
        let clear = uncovered_bounds(full, &[dialog, toast]).unwrap();
        assert_eq!(clear, rect(60., 0., 40., 60.));
        assert_eq!(
            uncovered_bounds(full, &[toast]),
            Some(rect(0., 0., 60., 100.))
        );
        assert_eq!(uncovered_bounds(full, &[]), Some(full));
        assert_eq!(uncovered_bounds(full, &[full]), None);
        assert_eq!(uncovered_bounds(full, &[]), Some(full));
    }
    #[test]
    fn edge_contacts_and_off_panel_overlays_do_not_shrink() {
        let full = rect(10., 20., 100., 100.);
        assert_eq!(
            uncovered_bounds(
                full,
                &[rect(110., 20., 20., 50.), rect(-40., -50., 10., 10.)]
            ),
            Some(full)
        );
        assert_eq!(
            uncovered_bounds(full, &[rect(-100., 0., 130., 200.)]),
            Some(rect(30., 20., 80., 100.))
        );
    }
    #[test]
    fn multiple_and_duplicate_overlays_leave_no_native_overlap() {
        let full = rect(0., 0., 100., 100.);
        let overlays = [
            rect(20., 20., 20., 20.),
            rect(60., 60., 30., 30.),
            rect(20., 20., 20., 20.),
        ];
        let clear = uncovered_bounds(full, &overlays).unwrap();
        for overlay in overlays {
            let overlap = clear.intersect(&overlay);
            assert!(overlap.size.width <= px(0.) || overlap.size.height <= px(0.));
        }
    }
}

#[cfg(test)]
mod frame_tests {
    use super::*;
    use gpui::{Context, IntoElement, Render, TestAppContext, Window};
    struct Harness;
    impl Render for Harness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            gpui::div()
        }
    }
    #[gpui::test]
    fn overlay_frames_are_replaced_and_isolated_per_window(cx: &mut TestAppContext) {
        let a = cx.add_window(|_, _| Harness);
        let b = cx.add_window(|_, _| Harness);
        let region = Bounds::new(point(px(1.), px(1.)), size(px(10.), px(10.)));
        a.update(cx, |_, window, cx| {
            gpui_base::GlobalState::begin_native_overlays(window, cx);
            gpui_base::GlobalState::record_native_overlay(region, window, cx);
            assert_eq!(
                gpui_base::GlobalState::native_overlays(window, cx),
                &[region]
            );
        })
        .unwrap();
        b.update(cx, |_, window, cx| {
            gpui_base::GlobalState::begin_native_overlays(window, cx);
            assert!(gpui_base::GlobalState::native_overlays(window, cx).is_empty());
        })
        .unwrap();
        a.update(cx, |_, window, cx| {
            assert_eq!(
                gpui_base::GlobalState::native_overlays(window, cx),
                &[region]
            );
            gpui_base::GlobalState::begin_native_overlays(window, cx);
            assert!(gpui_base::GlobalState::native_overlays(window, cx).is_empty());
        })
        .unwrap();
    }
}

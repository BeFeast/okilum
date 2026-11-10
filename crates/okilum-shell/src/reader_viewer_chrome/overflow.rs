//! Pure, viewer-supplied overflow policy. No file IO or GPUI state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Disposition {
    Shown,
    Collapsed,
    Menu,
}

#[derive(Clone, Debug)]
pub(crate) struct OverflowStep {
    pub priority: u16,
    pub width: f32,
    pub disposition: Disposition,
}

#[derive(Clone, Debug)]
pub(crate) struct LayoutItem {
    pub id: &'static str,
    pub width: f32,
    pub minimum_width: f32,
    pub never_dropped: bool,
    pub steps: Vec<OverflowStep>,
}

impl LayoutItem {
    pub(crate) fn action(
        id: &'static str,
        width: f32,
        minimum: f32,
        priority: u16,
        never: bool,
    ) -> Self {
        let mut steps = Vec::new();
        if width > minimum {
            steps.push(OverflowStep {
                priority: 0,
                width: minimum,
                disposition: Disposition::Collapsed,
            });
        }
        if !never {
            steps.push(OverflowStep {
                priority,
                width: 0.,
                disposition: Disposition::Menu,
            });
        }
        Self {
            id,
            width,
            minimum_width: minimum,
            never_dropped: never,
            steps,
        }
    }
    pub(crate) fn optional(id: &'static str, width: f32, minimum: f32, priority: u16) -> Self {
        Self::action(id, width, minimum, priority, false)
    }
    pub(crate) fn pinned(id: &'static str, width: f32, minimum: f32) -> Self {
        Self::action(id, width, minimum, u16::MAX, true)
    }
}

/// `fixed` includes padding, navigation and separators. Each shown control has
/// a 4 px gap; menu items take no row space. Steps are stable at equal priority.
/// Impossible widths preserve mandatory items rather than silently dropping one.
pub(crate) fn layout(
    available: f32,
    fixed: f32,
    title_minimum: f32,
    items: &[LayoutItem],
) -> Vec<Disposition> {
    let mut states = vec![Disposition::Shown; items.len()];
    let mut widths: Vec<_> = items.iter().map(|i| i.width).collect();
    let mut steps: Vec<_> = items
        .iter()
        .enumerate()
        .flat_map(|(index, item)| {
            item.steps
                .iter()
                .enumerate()
                .map(move |(order, step)| (step.priority, index, order, step))
        })
        .collect();
    steps.sort_by_key(|(priority, index, order, _)| (*priority, *index, *order));
    let used = |widths: &[f32], states: &[Disposition]| {
        fixed
            + title_minimum
            + widths.iter().sum::<f32>()
            + 4. * states.iter().filter(|s| **s != Disposition::Menu).count() as f32
    };
    for (_, index, _, step) in steps {
        if used(&widths, &states) <= available {
            break;
        }
        if step.disposition == Disposition::Menu && items[index].never_dropped {
            continue;
        }
        widths[index] = if step.disposition == Disposition::Menu {
            0.
        } else {
            step.width.max(items[index].minimum_width)
        };
        states[index] = step.disposition;
    }
    states
}

#[cfg(test)]
mod tests {
    use super::*;

    fn full_slots(kind: &str, editing: bool, large: bool) -> Vec<LayoutItem> {
        let mut items = vec![
            LayoutItem::optional("reveal", 28., 28., 10),
            LayoutItem::optional("width", 28., 28., 20),
            LayoutItem::optional("maximize", 28., 28., 21),
        ];
        let actions = match kind {
            "pdf" => vec!["zoom-out", "fit-width", "zoom-in"],
            "zip" => vec!["extract-selected", "extract-all"],
            "canvas" => vec!["fit-content", "zoom-out", "zoom-in"],
            "image" => vec![],
            _ => vec!["fold", "unfold"],
        };
        for id in actions {
            items.push(LayoutItem::action(id, 72., 28., 30, id == "extract-all"));
        }
        items.push(LayoutItem::optional("open-with", 120., 40., 40));
        items.push(LayoutItem::optional("copy", 90., 40., 50));
        if kind != "pdf" && kind != "image" && kind != "canvas" {
            let mut find = LayoutItem::pinned("find", 200., if large { 128. } else { 28. });
            find.steps = vec![OverflowStep {
                priority: 60,
                width: 128.,
                disposition: Disposition::Collapsed,
            }];
            if !large {
                find.steps.push(OverflowStep {
                    priority: 61,
                    width: 28.,
                    disposition: Disposition::Collapsed,
                });
            }
            items.push(find);
        }
        if kind == "json" {
            let mut views = LayoutItem::pinned("views", 240., 80.);
            views.steps = vec![OverflowStep {
                priority: 70,
                width: 80.,
                disposition: Disposition::Collapsed,
            }];
            items.push(views);
        }
        items.push(LayoutItem::pinned("edit", 74., 28.));
        if editing {
            items.push(LayoutItem::pinned("save", 28., 28.));
            items.push(LayoutItem::pinned("format", 90., 28.));
        }
        items.push(LayoutItem::pinned("more", 28., 28.));
        items
    }

    #[test]
    fn every_viewer_obeys_order_at_reference_widths_and_restores_on_widening() {
        for kind in ["json", "pdf", "image", "zip", "canvas"] {
            for editing in [false, true] {
                let items = full_slots(kind, editing, false);
                let snapshots: Vec<_> = [1400., 1100., 700., 520.]
                    .map(|w| layout(w, 128., 120., &items))
                    .into_iter()
                    .collect();
                assert!(
                    snapshots[0].iter().all(|s| *s == Disposition::Shown),
                    "{kind}: widest header"
                );
                for states in &snapshots {
                    for (item, state) in items.iter().zip(states) {
                        if item.never_dropped {
                            assert_ne!(*state, Disposition::Menu, "{} at {kind}", item.id);
                        }
                    }
                }
                for pair in snapshots.windows(2) {
                    for (wide, narrow) in pair[0].iter().zip(&pair[1]) {
                        if *wide == Disposition::Menu {
                            assert_eq!(*narrow, Disposition::Menu);
                        }
                    }
                }
                assert_eq!(
                    layout(1400., 128., 120., &items),
                    snapshots[0],
                    "widening is stateless"
                );
            }
        }
    }

    #[test]
    fn each_line_of_the_overflow_order_fires_before_the_next() {
        let mut items = vec![
            LayoutItem::optional("label", 100., 28., 30),
            LayoutItem::optional("reveal", 28., 28., 10),
            LayoutItem::optional("width", 28., 28., 20),
            LayoutItem::optional("maximize", 28., 28., 21),
            LayoutItem::optional("viewer", 28., 28., 30),
            LayoutItem::optional("open-with", 40., 40., 40),
            LayoutItem::optional("copy", 40., 40., 50),
            LayoutItem::pinned("find", 200., 28.),
            LayoutItem::pinned("views", 240., 80.),
        ];
        items[7].steps = vec![
            OverflowStep {
                priority: 60,
                width: 128.,
                disposition: Disposition::Collapsed,
            },
            OverflowStep {
                priority: 61,
                width: 28.,
                disposition: Disposition::Collapsed,
            },
        ];
        items[8].steps = vec![OverflowStep {
            priority: 70,
            width: 80.,
            disposition: Disposition::Collapsed,
        }];
        let mut expected = vec![Disposition::Shown; 9];
        let mut width = 100. + 28. * 4. + 40. * 2. + 200. + 240. + 9. * 4. + 248.;
        for (index, state, saving) in [
            (0, Disposition::Collapsed, 72.),
            (1, Disposition::Menu, 32.),
            (2, Disposition::Menu, 32.),
            (3, Disposition::Menu, 32.),
            (0, Disposition::Menu, 32.),
            (4, Disposition::Menu, 32.),
            (5, Disposition::Menu, 44.),
            (6, Disposition::Menu, 44.),
            (7, Disposition::Collapsed, 72.),
            (7, Disposition::Collapsed, 100.),
            (8, Disposition::Collapsed, 160.),
        ] {
            expected[index] = state;
            assert_eq!(
                layout(width - 1., 128., 120., &items),
                expected,
                "first yielding item {index}"
            );
            width -= saving;
        }
    }

    #[test]
    fn large_file_find_keeps_128_pixels_and_zip_extract_all_stays_pinned() {
        for kind in ["json", "zip"] {
            let items = full_slots(kind, true, true);
            let states = layout(520., 128., 120., &items);
            for id in [
                "find",
                "views",
                "edit",
                "save",
                "format",
                "more",
                "extract-all",
            ] {
                if let Some((_, state)) = items.iter().zip(&states).find(|(i, _)| i.id == id) {
                    assert_ne!(*state, Disposition::Menu, "{id}");
                }
            }
            let find = items.iter().find(|i| i.id == "find").unwrap();
            assert_eq!(find.minimum_width, 128.);
            assert!(find.steps.iter().all(|s| s.width >= 128.));
        }
    }
}

//! Native view selection uses the accepted canonical snapshot, never UI-thread IO.
use super::*;
use gpui_component::button::ButtonGroup;
use tessera_core::typed_view::{self, layout, Preferences, Selection};

#[derive(Clone)]
enum View {
    Markdown,
    Tasks(Arc<layout::Dashboard>),
    Fallback(String),
}

struct Cached {
    source: Arc<str>,
    preferences: Preferences,
    today: time::Date,
    view: View,
    filter: usize,
    grouping: Option<layout::Grouping>,
}
impl Cached {
    fn new(source: Arc<str>, preferences: Preferences, today: time::Date) -> Self {
        let view = match typed_view::select(&source, &preferences.mappings) {
            Selection::Markdown => View::Markdown,
            Selection::Fallback(reason) => View::Fallback(reason),
            Selection::Native(_) => match layout::parse(&source, today, preferences.tasks) {
                Ok(dashboard) => View::Tasks(Arc::new(dashboard)),
                Err(reason) => View::Fallback(reason),
            },
        };
        Self {
            source,
            preferences,
            today,
            view,
            filter: 0,
            grouping: None,
        }
    }
    fn refresh(&mut self, source: Arc<str>, preferences: Preferences, today: time::Date) -> bool {
        if self.source == source && self.preferences == preferences && self.today == today {
            return false;
        }
        *self = Self::new(source, preferences, today);
        true
    }
}

pub(super) fn render(
    reader: &Reader,
    window: &mut Window,
    cx: &mut Context<Reader>,
) -> Option<AnyElement> {
    let source = reader.note_canonical_source.clone()?;
    let preferences = reader_ui_state::typed_views(cx);
    let today = reader_tasks::today();
    let cache = window.use_keyed_state(
        SharedString::from(format!(
            "typed-view:{}",
            reader.vault_root.join(reader.selected_file()).display()
        )),
        cx,
        |_, _| Cached::new(Arc::from(""), Preferences::default(), today),
    );
    let (view, changed) = cache.update(cx, |cached, _| {
        let changed = cached.refresh(source, preferences, today);
        (cached.view.clone(), changed)
    });
    let dashboard = match view {
        View::Markdown => return None,
        View::Fallback(reason) => {
            if changed {
                reader_toast::transient(reason, window, cx);
            }
            return None;
        }
        View::Tasks(dashboard) => dashboard,
    };
    let weak = cx.entity().downgrade();
    let filter = cache.read(cx).filter;
    let grouping = cache.read(cx).grouping.unwrap_or(dashboard.layout.grouping);
    let layout = layout::Defaults {
        density: dashboard.layout.density,
        grouping,
    };
    let filters = ButtonGroup::new("dashboard-filter").children(
        [(0, "All"), (1, "Open"), (2, "Overdue")].map(|(value, label)| {
            let cache = cache.clone();
            let reader = weak.clone();
            Button::new(("dashboard-filter", value))
                .small()
                .ghost()
                .label(label)
                .selected(filter == value)
                .on_click(move |_, _, cx| {
                    cache.update(cx, |s, _| s.filter = value);
                    let _ = reader.update(cx, |_, cx| cx.notify());
                })
        }),
    );
    let groups = ButtonGroup::new("dashboard-grouping").children(
        [
            (layout::Grouping::Query, "Query"),
            (layout::Grouping::Note, "Note"),
            (layout::Grouping::None, "None"),
        ]
        .into_iter()
        .enumerate()
        .map(|(i, (value, label))| {
            let cache = cache.clone();
            let reader = weak.clone();
            Button::new(("dashboard-group", i))
                .small()
                .ghost()
                .label(label)
                .tooltip(format!("Group by {}", label.to_lowercase()))
                .selected(grouping == value)
                .on_click(move |_, _, cx| {
                    cache.update(cx, |s, _| s.grouping = Some(value));
                    let _ = reader.update(cx, |_, cx| cx.notify());
                })
        }),
    );
    Some(
        div()
            .id("native-tasks-dashboard")
            .debug_selector(|| "native-tasks-dashboard".into())
            .size_full()
            .overflow_y_scroll()
            .px_6()
            .pt_4()
            .pb_8()
            .child(
                v_flex()
                    .w_full()
                    .gap_6()
                    .child(h_flex().gap_3().flex_wrap().child(filters).child(groups))
                    .children(dashboard.layout.order.iter().map(|&index| {
                        let section = &dashboard.sections[index];
                        reader_tasks::dashboard_section(
                            section.title.clone(),
                            match filter {
                                1 => format!("{}\nnot done", section.query_source),
                                2 => {
                                    format!("{}\nnot done\ndue before today", section.query_source)
                                }
                                _ => section.query_source.clone(),
                            },
                            section.source_line,
                            layout,
                            weak.clone(),
                        )
                    })),
            )
            .into_any_element(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    #[test]
    fn accepted_frontmatter_and_preferences_select_views_without_stale_cache() {
        let day = time::macros::date!(2026 - 10 - 08);
        let source: Arc<str> = Arc::from(
            "---\ntype: dashboard\nview: tasks\n---\n## Today\n```tasks\nnot done\n```\n",
        );
        let mut cache = Cached::new(source.clone(), Preferences::default(), day);
        assert!(matches!(cache.view, View::Tasks(_)));
        assert!(!cache.refresh(source.clone(), Preferences::default(), day));
        assert!(cache.refresh(
            source.clone(),
            Preferences::default(),
            day.next_day().unwrap()
        ));
        assert!(cache.refresh(
            Arc::from(source.replace("view: tasks", "view: unavailable")),
            Preferences::default(),
            day
        ));
        assert!(matches!(cache.view, View::Fallback(_)));
        let mapped: Arc<str> = Arc::from(source.replace("view: tasks\n", ""));
        cache.refresh(mapped.clone(), Preferences::default(), day);
        assert!(matches!(cache.view, View::Markdown));
        let mut preferences = Preferences::default();
        preferences
            .mappings
            .insert("dashboard".into(), "tasks".into());
        cache.refresh(mapped, preferences, day);
        assert!(matches!(cache.view, View::Tasks(_)));
        cache.refresh(
            Arc::from(source.replace("not done", "unsupported query")),
            Preferences::default(),
            day,
        );
        assert!(matches!(cache.view, View::Fallback(_)));
    }
    #[gpui::test]
    fn native_dashboard_keeps_source_escape_and_markdown_fallback(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let fixture = tempfile::tempdir().unwrap();
        let vault = fixture.path().join("vault");
        std::fs::create_dir(&vault).unwrap();
        let source = "---\nview: tasks\n---\n## Focus\n```tasks\nnot done\n```\n";
        std::fs::write(vault.join("Dashboard.md"), source).unwrap();
        std::fs::write(vault.join("Work.md"), "- [ ] Ship dashboard\n").unwrap();
        let mut entity = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(vault.clone()),
                        note: Some("Dashboard.md".into()),
                        index_dir: Some(fixture.path().join("cache")),
                        session_directory: Some(fixture.path().join("state")),
                        panel_settings_override: Some(fixture.path().join("panels.json")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            entity = Some(view.clone());
            Root::new(view, window, cx)
        });
        let view = entity.unwrap();
        visual.simulate_resize(size(px(1400.), px(960.)));
        visual.run_until_parked();
        assert!(visual.debug_bounds("native-tasks-dashboard").is_some());
        assert!(
            visual.debug_bounds("reader-edit").is_some(),
            "Source escape is visible"
        );
        view.update_in(visual, |v, window, cx| v.toggle_source(window, cx));
        visual.run_until_parked();
        view.read_with(visual, |v, _| assert!(v.editing.is_some()));
        assert!(visual.debug_bounds("native-tasks-dashboard").is_none());
        view.update_in(visual, |v, window, cx| v.toggle_source(window, cx));
        visual.run_until_parked();
        assert!(visual.debug_bounds("native-tasks-dashboard").is_some());
        view.update(visual, |v, cx| {
            v.note_canonical_source =
                Some(Arc::from(source.replace("view: tasks", "view: unknown")));
            cx.notify();
        });
        visual.run_until_parked();
        assert!(visual.debug_bounds("native-tasks-dashboard").is_none());
        assert!(visual.debug_bounds("reader-edit").is_some());
    }
}

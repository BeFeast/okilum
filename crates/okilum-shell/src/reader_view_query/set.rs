//! Explicit revision-bound `set` actions (`status`, `owner`, `due`) on note rows
//! in declarative query views. Writes go through `okilum_core::frontmatter_edit::write`
//! (FileEditor lock, dirty-recovery check, post-condition verification, atomic save, Undo).
use super::*;
use gpui_component::{
    calendar::{Calendar, CalendarEvent, CalendarState, Date as CalendarDate},
    input::{Input, InputEvent, InputState},
    menu::{DropdownMenu as _, PopupMenuItem},
    notification::Notification,
    popover::{Popover, PopoverState},
};
use okilum_core::{
    frontmatter_edit::{self, write::Receipt},
    records::{FieldSchema, FieldType, Projection, Value as RecordValue},
    view::Row,
};
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SetField {
    Status,
    Owner,
    Due,
}

impl SetField {
    pub(super) fn from_action(action: &str) -> Option<Self> {
        match action {
            "set:status" => Some(Self::Status),
            "set:owner" => Some(Self::Owner),
            "set:due" => Some(Self::Due),
            _ => None,
        }
    }

    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::Owner => "owner",
            Self::Due => "due",
        }
    }
}

#[derive(Clone)]
pub(super) struct SetAction {
    pub field: SetField,
    pub current: Option<String>,
    pub options: Vec<String>,
    pub schema: Arc<FieldSchema>,
}

pub(super) fn has_set_action(action: &str) -> bool {
    SetField::from_action(action).is_some()
}

pub(super) fn build_actions(row: &Row, projection: &Projection) -> Vec<SetAction> {
    // Binding rule: `set` only operates on NOTE rows (never tasks, sections, or JSONL).
    if row.line.is_some() {
        return Vec::new();
    }
    let Some(note) = projection.note(&row.path) else {
        return Vec::new();
    };
    let schema = projection.schema_arc();
    let mut actions = Vec::new();
    for raw_action in &row.actions {
        let Some(field) = SetField::from_action(raw_action) else {
            continue;
        };
        if actions
            .iter()
            .any(|existing: &SetAction| existing.field == field)
        {
            continue;
        }
        let Some(def) = schema.fields.get(field.name()) else {
            continue;
        };
        if !def.writable_by_actions
            || matches!(def.field_type, FieldType::List(_))
            || def.field_type.is_opaque()
        {
            continue;
        }
        let current = match note.fields.get(field.name()) {
            Some(RecordValue::String(text)) if !text.is_empty() => Some(text.clone()),
            Some(RecordValue::Date(day)) | Some(RecordValue::LegacyDate { day, .. }) => {
                Some(format!(
                    "{:04}-{:02}-{:02}",
                    day.year(),
                    u8::from(day.month()),
                    day.day()
                ))
            }
            _ => None,
        };
        let options = if field == SetField::Status {
            let layer = match note.fields.get("layer") {
                Some(RecordValue::String(layer)) => layer.as_str(),
                _ => "legacy",
            };
            def.values_by_layer
                .get(layer)
                .or_else(|| def.values_by_layer.get("state"))
                .cloned()
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| def.values.clone())
        } else {
            Vec::new()
        };
        actions.push(SetAction {
            field,
            current,
            options,
            schema: Arc::clone(&schema),
        });
    }
    actions
}

#[allow(clippy::too_many_arguments)]
pub(super) fn render_control(
    action: &SetAction,
    path: &str,
    rev: &str,
    nonce: &str,
    block_ix: usize,
    row_ix: usize,
    reader: &WeakEntity<Reader>,
    root: &Path,
) -> AnyElement {
    match action.field {
        SetField::Status => render_status(action, path, rev, nonce, block_ix, row_ix, reader, root),
        SetField::Due => render_due(action, path, rev, nonce, block_ix, row_ix, reader, root),
        SetField::Owner => render_owner(action, path, rev, nonce, block_ix, row_ix, reader, root),
    }
}

#[allow(clippy::too_many_arguments)]
fn render_status(
    action: &SetAction,
    path: &str,
    rev: &str,
    nonce: &str,
    block_ix: usize,
    row_ix: usize,
    reader: &WeakEntity<Reader>,
    root: &Path,
) -> AnyElement {
    let label = action
        .current
        .clone()
        .unwrap_or_else(|| "Set status".to_owned());
    let options = action.options.clone();
    let current = action.current.clone();
    let schema = Arc::clone(&action.schema);
    let path = path.to_owned();
    let rev = rev.to_owned();
    let reader = reader.clone();
    let root = root.to_path_buf();

    Button::new(SharedString::from(format!(
        "view-set-status-{nonce}-{block_ix}-{row_ix}"
    )))
    .small()
    .ghost()
    .flex_shrink_0()
    .label(label)
    .tooltip("Set status")
    .debug_selector(move || format!("view-action-set-status-{block_ix}-{row_ix}"))
    .dropdown_menu(move |mut menu, _, _| {
        for option in &options {
            let opt = option.clone();
            let opt_selector = opt.clone();
            let opt_label = opt.clone();
            let is_current = current.as_deref() == Some(opt.as_str());
            let reader = reader.clone();
            let expected_root = root.clone();
            let path = path.clone();
            let rev = rev.clone();
            let schema = Arc::clone(&schema);
            menu = menu.item(
                PopupMenuItem::element(move |_, _| {
                    let opt_selector = opt_selector.clone();
                    div()
                        .debug_selector(move || {
                            format!("view-action-set-status-opt-{block_ix}-{row_ix}-{opt_selector}")
                        })
                        .child(opt_label.clone())
                })
                .checked(is_current)
                .on_click(move |_, window, cx| {
                    let _ = reader.update(cx, |this, cx| {
                        if this.vault_root != expected_root {
                            return;
                        }
                        this.apply_view_frontmatter_set(
                            path.clone(),
                            rev.clone(),
                            Arc::clone(&schema),
                            SetField::Status,
                            opt.clone(),
                            window,
                            cx,
                        );
                    });
                }),
            );
        }
        menu
    })
    .into_any_element()
}

#[allow(clippy::too_many_arguments)]
fn render_due(
    action: &SetAction,
    path: &str,
    rev: &str,
    nonce: &str,
    block_ix: usize,
    row_ix: usize,
    reader: &WeakEntity<Reader>,
    root: &Path,
) -> AnyElement {
    let label = action
        .current
        .clone()
        .unwrap_or_else(|| "Set due".to_owned());
    let initial_date = action.current.as_deref().and_then(parse_iso_date);
    let schema = Arc::clone(&action.schema);
    let path = path.to_owned();
    let rev = rev.to_owned();
    let reader = reader.clone();
    let root = root.to_path_buf();

    Button::new(SharedString::from(format!(
        "view-set-due-{nonce}-{block_ix}-{row_ix}"
    )))
    .small()
    .ghost()
    .flex_shrink_0()
    .icon(IconName::Calendar)
    .label(label)
    .tooltip("Set due date")
    .debug_selector(move || format!("view-action-set-due-{block_ix}-{row_ix}"))
    .dropdown_menu(move |mut menu, window, cx| {
        let today = schema.today_now();
        let tomorrow = today.next_day().unwrap_or(today);
        for (item_label, date, tag) in [
            ("Due today", today, "today"),
            ("Due tomorrow", tomorrow, "tomorrow"),
        ] {
            let formatted = format_iso_date(date);
            let reader = reader.clone();
            let expected_root = root.clone();
            let path = path.clone();
            let rev = rev.clone();
            let schema = Arc::clone(&schema);
            menu = menu.item(
                PopupMenuItem::element(move |_, _| {
                    div()
                        .debug_selector(move || {
                            format!("view-action-set-due-opt-{block_ix}-{row_ix}-{tag}")
                        })
                        .child(item_label)
                })
                .on_click(move |_, window, cx| {
                    let _ = reader.update(cx, |this, cx| {
                        if this.vault_root != expected_root {
                            return;
                        }
                        this.apply_view_frontmatter_set(
                            path.clone(),
                            rev.clone(),
                            Arc::clone(&schema),
                            SetField::Due,
                            formatted.clone(),
                            window,
                            cx,
                        );
                    });
                }),
            );
        }

        let calendar = cx.new(|cx| {
            let mut state = CalendarState::new(window, cx);
            let seed = initial_date.unwrap_or(today);
            if let Some(date) = chrono::NaiveDate::from_ymd_opt(
                seed.year(),
                u8::from(seed.month()) as u32,
                seed.day() as u32,
            ) {
                state.set_date(date, window, cx);
            }
            state
        });
        let cal_reader = reader.clone();
        let cal_root = root.clone();
        let cal_path = path.clone();
        let cal_rev = rev.clone();
        let cal_schema = Arc::clone(&schema);
        cx.subscribe_in(&calendar, window, move |_, _, event, window, cx| {
            let CalendarEvent::Selected(CalendarDate::Single(Some(date))) = event else {
                return;
            };
            use chrono::Datelike;
            let Ok(date) = time::Date::from_calendar_date(
                date.year(),
                (date.month() as u8).try_into().expect("calendar month"),
                date.day() as u8,
            ) else {
                return;
            };
            let formatted = format_iso_date(date);
            let _ = cal_reader.update(cx, |this, cx| {
                if this.vault_root == cal_root {
                    this.apply_view_frontmatter_set(
                        cal_path.clone(),
                        cal_rev.clone(),
                        Arc::clone(&cal_schema),
                        SetField::Due,
                        formatted.clone(),
                        window,
                        cx,
                    );
                }
            });
            cx.emit(gpui::DismissEvent);
        })
        .detach();

        menu.separator()
            .label("Choose due date")
            .item(PopupMenuItem::element(move |_, _| {
                div()
                    .id(SharedString::from(format!(
                        "view-set-due-calendar-{block_ix}-{row_ix}"
                    )))
                    .on_click(|_, _, cx| cx.stop_propagation())
                    .child(Calendar::new(&calendar).border_0())
            }))
    })
    .into_any_element()
}

struct OwnerEditorState {
    input: Entity<InputState>,
    _enter: gpui::Subscription,
}

#[allow(clippy::too_many_arguments)]
fn render_owner(
    action: &SetAction,
    path: &str,
    rev: &str,
    nonce: &str,
    block_ix: usize,
    row_ix: usize,
    reader: &WeakEntity<Reader>,
    root: &Path,
) -> AnyElement {
    let label = action
        .current
        .clone()
        .unwrap_or_else(|| "Set owner".to_owned());
    let current = action.current.clone().unwrap_or_default();
    let schema = Arc::clone(&action.schema);
    let path = path.to_owned();
    let rev = rev.to_owned();
    let reader = reader.clone();
    let root = root.to_path_buf();
    let state_key = SharedString::from(format!("view-set-owner-state-{nonce}-{block_ix}-{row_ix}"));

    Popover::new(SharedString::from(format!(
        "view-set-owner-popover-{nonce}-{block_ix}-{row_ix}"
    )))
    .trigger(
        Button::new(SharedString::from(format!(
            "view-set-owner-{nonce}-{block_ix}-{row_ix}"
        )))
        .small()
        .ghost()
        .flex_shrink_0()
        .label(label)
        .tooltip("Set owner")
        .debug_selector(move || format!("view-action-set-owner-{block_ix}-{row_ix}")),
    )
    .content(move |_popover_state: &mut PopoverState, window, cx| {
        let popover = cx.entity().downgrade();
        let initial = current.clone();
        let enter_reader = reader.clone();
        let enter_root = root.clone();
        let enter_path = path.clone();
        let enter_rev = rev.clone();
        let enter_schema = Arc::clone(&schema);
        let enter_popover = popover.clone();
        let editor_state = window.use_keyed_state(state_key.clone(), cx, |window, cx| {
            let input = cx.new(|cx| {
                let mut state = InputState::new(window, cx).placeholder("owner-slug");
                if !initial.is_empty() {
                    state.set_value(initial.clone(), window, cx);
                }
                state
            });
            let _enter = cx.subscribe_in(&input, window, move |_, input, event, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    let value = input.read(cx).value().trim().to_owned();
                    let _ = enter_reader.update(cx, |this, cx| {
                        if this.vault_root == enter_root {
                            this.apply_view_frontmatter_set(
                                enter_path.clone(),
                                enter_rev.clone(),
                                Arc::clone(&enter_schema),
                                SetField::Owner,
                                value.clone(),
                                window,
                                cx,
                            );
                        }
                    });
                    let _ = enter_popover.update(cx, |state, cx| state.dismiss(window, cx));
                }
            });
            OwnerEditorState { input, _enter }
        });
        let input = editor_state.read(cx).input.clone();
        let submit_input = input.clone();
        let submit_reader = reader.clone();
        let submit_root = root.clone();
        let submit_path = path.clone();
        let submit_rev = rev.clone();
        let submit_schema = Arc::clone(&schema);
        h_flex()
            .w(px(240.))
            .p_2()
            .gap_2()
            .items_center()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .debug_selector(move || {
                        format!("view-action-set-owner-input-{block_ix}-{row_ix}")
                    })
                    .child(Input::new(&input).small()),
            )
            .child(
                Button::new(SharedString::from(format!(
                    "view-set-owner-apply-{block_ix}-{row_ix}"
                )))
                .small()
                .primary()
                .label("Set")
                .debug_selector(move || format!("view-action-set-owner-apply-{block_ix}-{row_ix}"))
                .on_click(move |_, window, cx| {
                    let value = submit_input.read(cx).value().trim().to_owned();
                    let _ = submit_reader.update(cx, |this, cx| {
                        if this.vault_root == submit_root {
                            this.apply_view_frontmatter_set(
                                submit_path.clone(),
                                submit_rev.clone(),
                                Arc::clone(&submit_schema),
                                SetField::Owner,
                                value.clone(),
                                window,
                                cx,
                            );
                        }
                    });
                    let _ = popover.update(cx, |state, cx| state.dismiss(window, cx));
                }),
            )
    })
    .into_any_element()
}

fn parse_iso_date(text: &str) -> Option<time::Date> {
    if text.len() != 10 {
        return None;
    }
    time::Date::parse(
        text,
        &time::macros::format_description!("[year]-[month]-[day]"),
    )
    .ok()
}

fn format_iso_date(date: time::Date) -> String {
    format!(
        "{:04}-{:02}-{:02}",
        date.year(),
        u8::from(date.month()),
        date.day()
    )
}

#[derive(Clone)]
struct SetOwner {
    root: PathBuf,
    state: PathBuf,
    index_directory: Option<PathBuf>,
    navigation: u64,
    document: String,
    content: Entity<TextViewState>,
}

impl SetOwner {
    fn capture(reader: &Reader) -> Option<Self> {
        Some(Self {
            root: reader.vault_root.clone(),
            state: reader.session_directory.clone()?,
            index_directory: reader.index_dir.clone(),
            navigation: reader.pane.navigation.generation,
            document: reader.pane.current_rel.clone(),
            content: reader.pane.content.clone(),
        })
    }

    fn current(&self, reader: &Reader) -> bool {
        reader.vault_root == self.root
            && reader.session_directory.as_ref() == Some(&self.state)
            && reader.index_dir == self.index_directory
            && reader.pane.navigation.generation == self.navigation
            && reader.pane.current_rel == self.document
            && reader.pane.content == self.content
            && reader.pane.editing.is_none()
    }
}

impl Reader {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn apply_view_frontmatter_set(
        &mut self,
        path: String,
        expected_rev: String,
        schema: Arc<FieldSchema>,
        field: SetField,
        value: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(owner) = SetOwner::capture(self).filter(|owner| owner.current(self)) else {
            reader_toast::error(
                "View or recovery storage changed. Refresh before editing.",
                window,
                cx,
            );
            return;
        };
        let target = match frontmatter_edit::Target::new(path, expected_rev) {
            Ok(target) => target,
            Err(error) => {
                reader_toast::error(format!("Note unchanged: {error}"), window, cx);
                return;
            }
        };

        let valid = Arc::new(AtomicBool::new(true));
        let observed = owner.clone();
        let revoke = valid.clone();
        let subscription = cx.observe_self(move |reader, _| {
            if !observed.current(reader) {
                revoke.store(false, Ordering::Release);
            }
        });
        let on_release = valid.clone();
        let release = cx.on_release(move |_, _| on_release.store(false, Ordering::Release));
        let drafts = owner.state.join("editor-drafts");

        cx.spawn_in(window, async move |reader, cx| {
            let job_owner = owner.clone();
            let allowed = valid.clone();
            let storage = drafts.clone();
            let field_name = field.name();
            let result = cx
                .background_executor()
                .spawn(async move {
                    anyhow::ensure!(
                        allowed.load(Ordering::Acquire),
                        "View changed before save; nothing was written"
                    );
                    frontmatter_edit::write::apply(
                        &job_owner.root,
                        &storage,
                        &schema,
                        &target,
                        field_name,
                        &value,
                    )
                })
                .await;
            drop(subscription);
            drop(release);
            let _ = reader.update_in_own_window(cx, |reader, window, cx| {
                let current = valid.load(Ordering::Acquire) && owner.current(reader);
                if !current {
                    cx.notify();
                    return;
                }
                match result {
                    Ok(Some(receipt)) => {
                        reader.queue_vault_mutation(
                            okilum_core::Changes {
                                changed: [receipt.path().to_owned()].into_iter().collect(),
                                ..Default::default()
                            },
                            cx,
                        );
                        let weak = cx.weak_entity();
                        let undo_owner = owner.clone();
                        reader_toast::push(
                            Notification::new()
                                .message(format!("Updated {}", field.name()))
                                .content(move |_, _, _| {
                                    let reader = weak.clone();
                                    let receipt = receipt.clone();
                                    let undo_owner = undo_owner.clone();
                                    Button::new("frontmatter-undo")
                                        .debug_selector(|| "frontmatter-undo".into())
                                        .small()
                                        .ghost()
                                        .label("Undo")
                                        .on_click(move |_, window, cx| {
                                            let _ = reader.update(cx, |reader, cx| {
                                                reader.undo_frontmatter(
                                                    receipt.clone(),
                                                    undo_owner.clone(),
                                                    window,
                                                    cx,
                                                )
                                            });
                                        })
                                        .into_any_element()
                                }),
                            Some(Duration::from_secs(8)),
                            window,
                            cx,
                        );
                    }
                    Ok(None) => {}
                    Err(error) => {
                        reader_toast::error(format!("Note unchanged: {error}"), window, cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn undo_frontmatter(
        &mut self,
        receipt: Receipt,
        owner: SetOwner,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !owner.current(self) {
            reader_toast::error(
                "Undo belongs to the previous Reader view. Its saved source was kept.",
                window,
                cx,
            );
            return;
        }
        let valid = Arc::new(AtomicBool::new(true));
        let lease = valid.clone();
        let expected = owner.clone();
        let subscription = cx.observe_self(move |reader, _| {
            if !expected.current(reader) {
                lease.store(false, Ordering::Release);
            }
        });
        let on_release = valid.clone();
        let release = cx.on_release(move |_, _| on_release.store(false, Ordering::Release));
        let path = receipt.path().to_owned();
        let job_owner = owner.clone();
        let allowed = valid.clone();
        cx.spawn_in(window, async move |reader, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    anyhow::ensure!(
                        allowed.load(Ordering::Acquire),
                        "View changed; saved source was kept"
                    );
                    receipt.undo(&job_owner.root, &job_owner.state.join("editor-drafts"))
                })
                .await;
            drop(subscription);
            drop(release);
            let _ = reader.update_in_own_window(cx, |reader, window, cx| {
                if !valid.load(Ordering::Acquire) || !owner.current(reader) {
                    return;
                }
                match result {
                    Ok(()) => {
                        reader.queue_vault_mutation(
                            okilum_core::Changes {
                                changed: [path].into_iter().collect(),
                                ..Default::default()
                            },
                            cx,
                        );
                        reader_toast::transient("Property change undone", window, cx);
                    }
                    Err(error) => {
                        reader_toast::error(
                            format!("Cannot undo property change: {error}"),
                            window,
                            cx,
                        );
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    use gpui::{TestAppContext, VisualTestContext};

    const FIXTURE_VIEWS: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../okilum-core/tests/fixtures/views"
    );

    fn copy_tree(src: &Path, dst: &Path) {
        std::fs::create_dir_all(dst).unwrap();
        for entry in std::fs::read_dir(src).unwrap() {
            let entry = entry.unwrap();
            let target = dst.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                copy_tree(&entry.path(), &target);
            } else {
                std::fs::copy(entry.path(), target).unwrap();
            }
        }
    }

    fn wait_for_bounds(visual: &mut VisualTestContext, selector: &'static str) -> Bounds<Pixels> {
        for _ in 0..60 {
            visual.run_until_parked();
            if let Some(bounds) = visual.debug_bounds(selector) {
                return bounds;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("expected debug bounds for {selector}");
    }

    fn click_undo(visual: &mut VisualTestContext) {
        std::thread::sleep(Duration::from_millis(450));
        visual.update(|window, _| window.refresh());
        visual.executor().advance_clock(Duration::from_millis(400));
        visual.run_until_parked();
        let undo = wait_for_bounds(visual, "frontmatter-undo");
        visual.simulate_mouse_move(undo.center(), None, Modifiers::default());
        visual.simulate_click(undo.center(), Modifiers::default());
        visual.run_until_parked();
    }

    #[cfg(unix)]
    #[gpui::test]
    fn view_set_status_updates_bom_crlf_note_removes_row_and_undo_restores_exact_bytes(
        cx: &mut TestAppContext,
    ) {
        let temp = tempfile::tempdir().unwrap();
        let vault = temp.path().join("vault");
        copy_tree(Path::new(FIXTURE_VIEWS), &vault);

        // Create a view with `set: status` on active Finance/Dev projects.
        let view_src = concat!(
            "---\n",
            "type: view\n",
            "view: query\n",
            "title: Active projects\n",
            "---\n",
            "# Active projects\n\n",
            "```view\n",
            "from: notes\n",
            "where: kind = project and status = active and area in (Dev, Finance)\n",
            "cols: [title, status]\n",
            "sort: title\n",
            "show: table\n",
            "actions:\n",
            "  - {set: status}\n",
            "```\n",
        );
        std::fs::write(vault.join("@views/set-status.md"), view_src).unwrap();

        // Give the first row (`state/finance/budget/index.md`) BOM + CRLF + comment to verify exact preservation.
        let budget_path = vault.join("state/finance/budget/index.md");
        let original_lf = std::fs::read_to_string(&budget_path).unwrap();
        let original = format!(
            "\u{feff}{}",
            original_lf
                .replace('\n', "\r\n")
                .replace("area: Finance\r\n", "# keep comment\r\narea: Finance\r\n")
        );
        std::fs::write(&budget_path, &original).unwrap();

        cx.executor().allow_parking();
        cx.update(|cx| {
            cx.set_reduce_motion(true);
            crate::test_init(cx);
            bind_keys(cx);
        });
        let mut entity = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(vault.clone()),
                        note: Some("@views/set-status.md".into()),
                        index_dir: Some(temp.path().join("index")),
                        session_directory: Some(temp.path().join("state")),
                        panel_settings_override: Some(temp.path().join("panels.json")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            entity = Some(view.clone());
            Root::new(view, window, cx)
        });
        let _reader = entity.unwrap();
        visual.simulate_resize(size(px(1400.), px(960.)));

        let status_btn = wait_for_bounds(visual, "view-action-set-status-0-0");
        assert!(
            visual.debug_bounds("view-action-set-status-0-1").is_some(),
            "two active projects initially (Budget 2027 and Okilum)"
        );
        visual.simulate_click(status_btn.center(), Modifiers::default());
        let parked_opt = wait_for_bounds(visual, "view-action-set-status-opt-0-0-parked");
        visual.simulate_click(parked_opt.center(), Modifiers::default());

        for _ in 0..60 {
            visual.run_until_parked();
            if visual.debug_bounds("view-action-set-status-0-1").is_none() {
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        assert!(
            visual.debug_bounds("view-action-set-status-0-1").is_none(),
            "setting Budget 2027 to parked removes it from the active table"
        );
        let expected = original.replacen("status: active\r\n", "status: parked\r\n", 1);
        assert_eq!(
            std::fs::read(&budget_path).unwrap(),
            expected.as_bytes(),
            "only status line changed; BOM, CRLF and comments preserved"
        );

        // Undo restores exact preimage bytes and brings Budget 2027 back.
        click_undo(visual);
        for _ in 0..60 {
            visual.run_until_parked();
            if visual.debug_bounds("view-action-set-status-0-1").is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        assert_eq!(
            std::fs::read(&budget_path).unwrap(),
            original.as_bytes(),
            "Undo restores exact previous bytes"
        );
        assert!(
            visual.debug_bounds("view-action-set-status-0-1").is_some(),
            "Undo brings Budget 2027 back into the active table"
        );
    }

    #[cfg(unix)]
    #[gpui::test]
    fn view_set_stale_revision_refuses_without_writing_and_shows_error_toast(
        cx: &mut TestAppContext,
    ) {
        let temp = tempfile::tempdir().unwrap();
        let vault = temp.path().join("vault");
        copy_tree(Path::new(FIXTURE_VIEWS), &vault);
        let view_src = concat!(
            "---\n",
            "type: view\n",
            "view: query\n",
            "title: Active projects\n",
            "---\n",
            "# Active projects\n\n",
            "```view\n",
            "from: notes\n",
            "where: kind = project and status = active and area = Finance\n",
            "cols: [title, status]\n",
            "show: table\n",
            "actions:\n",
            "  - {set: status}\n",
            "```\n",
        );
        std::fs::write(vault.join("@views/set-stale.md"), view_src).unwrap();

        cx.executor().allow_parking();
        cx.update(|cx| {
            cx.set_reduce_motion(true);
            crate::test_init(cx);
            bind_keys(cx);
        });
        let mut entity = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(vault.clone()),
                        note: Some("@views/set-stale.md".into()),
                        index_dir: Some(temp.path().join("index")),
                        session_directory: Some(temp.path().join("state")),
                        panel_settings_override: Some(temp.path().join("panels.json")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            entity = Some(view.clone());
            Root::new(view, window, cx)
        });
        let _reader = entity.unwrap();
        visual.simulate_resize(size(px(1400.), px(960.)));

        let status_btn = wait_for_bounds(visual, "view-action-set-status-0-0");
        let budget_path = vault.join("state/finance/budget/index.md");
        let external = format!(
            "{}\nExternal edit\n",
            std::fs::read_to_string(&budget_path).unwrap()
        );
        std::fs::write(&budget_path, &external).unwrap();

        visual.simulate_click(status_btn.center(), Modifiers::default());
        let parked_opt = wait_for_bounds(visual, "view-action-set-status-opt-0-0-parked");
        visual.simulate_click(parked_opt.center(), Modifiers::default());

        let toast = wait_for_bounds(visual, "reader-error-toast");
        assert!(toast.size.width > px(0.));
        assert_eq!(
            std::fs::read(&budget_path).unwrap(),
            external.as_bytes(),
            "stale revision refuses without touching disk"
        );
    }

    #[cfg(unix)]
    #[gpui::test]
    fn view_set_owner_and_due_append_absent_fields_and_undo_restores_bytes(
        cx: &mut TestAppContext,
    ) {
        let temp = tempfile::tempdir().unwrap();
        let vault = temp.path().join("vault");
        copy_tree(Path::new(FIXTURE_VIEWS), &vault);
        let view_src = concat!(
            "---\n",
            "type: view\n",
            "view: query\n",
            "title: Okilum project\n",
            "---\n",
            "# Okilum project\n\n",
            "```view\n",
            "from: notes\n",
            "where: kind = project and context = okilum\n",
            "cols: [title, owner, due]\n",
            "show: table\n",
            "actions:\n",
            "  - {set: owner}\n",
            "  - {set: due}\n",
            "```\n",
        );
        std::fs::write(vault.join("@views/set-owner-due.md"), view_src).unwrap();

        let okilum_path = vault.join("state/dev/okilum/index.md");
        let original = std::fs::read_to_string(&okilum_path).unwrap();
        assert!(!original.contains("\nowner:"));
        assert!(!original.contains("\ndue:"));

        cx.executor().allow_parking();
        cx.update(|cx| {
            cx.set_reduce_motion(true);
            crate::test_init(cx);
            bind_keys(cx);
        });
        let mut entity = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(vault.clone()),
                        note: Some("@views/set-owner-due.md".into()),
                        index_dir: Some(temp.path().join("index")),
                        session_directory: Some(temp.path().join("state")),
                        panel_settings_override: Some(temp.path().join("panels.json")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            entity = Some(view.clone());
            Root::new(view, window, cx)
        });
        let _reader = entity.unwrap();
        visual.simulate_resize(size(px(1400.), px(960.)));

        // Set due date via "Due today" quick menu item.
        let due_btn = wait_for_bounds(visual, "view-action-set-due-0-0");
        visual.simulate_click(due_btn.center(), Modifiers::default());
        let today_opt = wait_for_bounds(visual, "view-action-set-due-opt-0-0-today");
        visual.simulate_click(today_opt.center(), Modifiers::default());

        let schema = FieldSchema::load(&vault).unwrap();
        let today_str = format_iso_date(schema.today_now());
        let expected_due = original.replacen("\n---\n", &format!("\ndue: {today_str}\n---\n"), 1);
        for _ in 0..60 {
            visual.run_until_parked();
            if std::fs::read_to_string(&okilum_path).unwrap() == expected_due {
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        assert_eq!(
            std::fs::read(&okilum_path).unwrap(),
            expected_due.as_bytes(),
            "setting absent due appends one line at the end of frontmatter"
        );

        // Undo restores exact original bytes.
        click_undo(visual);
        for _ in 0..60 {
            visual.run_until_parked();
            if std::fs::read_to_string(&okilum_path).unwrap() == original {
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        assert_eq!(std::fs::read(&okilum_path).unwrap(), original.as_bytes());

        // Set owner via the inline slug popover: invalid slug refuses, valid slug appends + Undo restores.
        let owner_btn = wait_for_bounds(visual, "view-action-set-owner-0-0");
        visual.simulate_click(owner_btn.center(), Modifiers::default());
        let owner_input = wait_for_bounds(visual, "view-action-set-owner-input-0-0");
        visual.simulate_click(owner_input.center(), Modifiers::default());
        visual.simulate_input("Bad Owner");
        let apply_btn = wait_for_bounds(visual, "view-action-set-owner-apply-0-0");
        visual.simulate_click(apply_btn.center(), Modifiers::default());
        let err_toast = wait_for_bounds(visual, "reader-error-toast");
        assert!(err_toast.size.width > px(0.));
        assert_eq!(
            std::fs::read(&okilum_path).unwrap(),
            original.as_bytes(),
            "invalid owner slug must refuse without touching disk"
        );

        let owner_btn = wait_for_bounds(visual, "view-action-set-owner-0-0");
        visual.simulate_click(owner_btn.center(), Modifiers::default());
        let owner_input = wait_for_bounds(visual, "view-action-set-owner-input-0-0");
        visual.simulate_click(owner_input.center(), Modifiers::default());
        visual.simulate_keystrokes("ctrl-a backspace");
        visual.simulate_input("okilum-assistant");
        let apply_btn = wait_for_bounds(visual, "view-action-set-owner-apply-0-0");
        visual.simulate_click(apply_btn.center(), Modifiers::default());
        let expected_owner = original.replacen("\n---\n", "\nowner: okilum-assistant\n---\n", 1);
        for _ in 0..60 {
            visual.run_until_parked();
            if std::fs::read_to_string(&okilum_path).unwrap() == expected_owner {
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        assert_eq!(
            std::fs::read(&okilum_path).unwrap(),
            expected_owner.as_bytes(),
            "setting absent owner appends one line at the end of frontmatter"
        );
        click_undo(visual);
        for _ in 0..60 {
            visual.run_until_parked();
            if std::fs::read_to_string(&okilum_path).unwrap() == original {
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        assert_eq!(std::fs::read(&okilum_path).unwrap(), original.as_bytes());
    }

    #[test]
    fn build_actions_ignores_non_note_rows_and_disallowed_fields() {
        use okilum_core::records::Source;
        let schema = Arc::new(FieldSchema::load(Path::new(FIXTURE_VIEWS)).unwrap());
        let note = "---\ntype: project\nkind: project\narea: Dev\nstatus: active\n---\n# Okilum\n- [ ] Task\n";
        let projection = Projection::build(
            schema,
            [Source {
                path: "state/dev/okilum/index.md",
                bytes: note.as_bytes(),
                mtime: None,
            }],
        );
        let task_row = Row {
            path: "state/dev/okilum/index.md".into(),
            line: Some(8),
            anchor: None,
            rev: okilum_core::frontmatter_edit::revision(note),
            group: Vec::new(),
            fields: BTreeMap::new(),
            actions: vec!["open".into(), "set:status".into()],
            markdown: None,
        };
        assert!(
            build_actions(&task_row, &projection).is_empty(),
            "task/section/JSONL rows (line.is_some()) must never get set controls"
        );

        let note_row = Row {
            path: "state/dev/okilum/index.md".into(),
            line: None,
            anchor: None,
            rev: okilum_core::frontmatter_edit::revision(note),
            group: Vec::new(),
            fields: BTreeMap::new(),
            actions: vec![
                "open".into(),
                "set:status".into(),
                "set:area".into(),
                "set:title".into(),
            ],
            markdown: None,
        };
        let built = build_actions(&note_row, &projection);
        assert_eq!(built.len(), 1);
        assert_eq!(built[0].field, SetField::Status);
        assert_eq!(built[0].current.as_deref(), Some("active"));
        assert_eq!(built[0].options, vec!["active", "parked", "done"]);
    }

    #[test]
    fn build_actions_skips_opaque_fields_even_when_marked_writable() {
        use okilum_core::records::Source;

        let original_owner = concat!(
            "  owner:\n",
            "    type: string\n",
            "    format: slug\n",
            "    origin: stored\n",
            "    rule: identifies the person or assistant expected to act\n",
            "    writeback: none\n",
            "    writers: [people, agents]\n",
            "    required: {}\n",
            "    index: {indexed: true, facet: true, sort: true}\n",
            "    writable_by_actions: true",
        );
        let note = "---\ntype: project\nkind: project\narea: Dev\nstatus: active\n---\n# Okilum\n";

        for owner_def in [
            "  owner:\n    type: custom_blob\n    index: {indexed: false}\n    writable_by_actions: true",
            "  owner:\n    type: object\n    writable_by_actions: true",
        ] {
            let schema = Arc::new(
                FieldSchema::from_documents(|name| {
                    let mut text = std::fs::read_to_string(
                        Path::new(FIXTURE_VIEWS).join("@Schema").join(name),
                    )?;
                    if name == "fields.yaml" {
                        assert!(text.contains(original_owner));
                        text = text.replace(original_owner, owner_def);
                    }
                    Ok(text)
                })
                .unwrap(),
            );
            let projection = Projection::build(
                schema,
                [Source {
                    path: "state/dev/okilum/index.md",
                    bytes: note.as_bytes(),
                    mtime: None,
                }],
            );
            let row = Row {
                path: "state/dev/okilum/index.md".into(),
                line: None,
                anchor: None,
                rev: okilum_core::frontmatter_edit::revision(note),
                group: Vec::new(),
                fields: BTreeMap::new(),
                actions: vec!["open".into(), "set:owner".into(), "set:status".into()],
                markdown: None,
            };
            let built = build_actions(&row, &projection);
            assert_eq!(built.len(), 1);
            assert_eq!(built[0].field, SetField::Status);
        }
    }
}

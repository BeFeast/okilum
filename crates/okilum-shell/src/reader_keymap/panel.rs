use super::*;

struct Pending {
    owner: WindowId,
    generation: u64,
    row: Row,
    key: Option<String>,
    conflicts: Vec<String>,
    warnings: Vec<String>,
    reset: Option<Document>,
}

pub(crate) struct KeyboardSettings {
    reader: Option<WeakEntity<Reader>>,
    pub(super) search: Entity<InputState>,
    focus: FocusHandle,
    pending: Option<Pending>,
    error: Option<String>,
    status: Option<String>,
    saving: bool,
    visible: Vec<Row>,
    _subscriptions: Vec<Subscription>,
}

impl KeyboardSettings {
    pub(crate) fn new(
        reader: Option<WeakEntity<Reader>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        if !cx.has_global::<Keymap>() {
            install_at(None, cx);
        }
        let search = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Search actions, contexts or shortcuts")
        });
        let weak = cx.entity().downgrade();
        let intercept = cx.intercept_keystrokes(move |event, window, cx| {
            if !is_recording(window, cx) {
                return;
            }
            cx.stop_propagation();
            let key = event.keystroke.clone();
            let weak = weak.clone();
            // Keep the recording flag through the low-level capture phase,
            // so Settings cannot interpret a cancelling Esc as Close Settings.
            window.defer(cx, move |window, cx| {
                let _ = weak.update(cx, |this, cx| this.capture(&key, window, cx));
            });
        });
        Self {
            _subscriptions: vec![
                intercept,
                cx.observe(&search, |_, _, cx| cx.notify()),
                cx.observe_global::<Keymap>(|_, cx| cx.notify()),
            ],
            reader,
            search,
            focus: cx.focus_handle(),
            pending: None,
            error: None,
            status: None,
            saving: false,
            visible: Vec::new(),
        }
    }

    fn cancel(&mut self, cx: &mut Context<Self>) {
        if self
            .pending
            .as_ref()
            .is_some_and(|p| cx.global::<Keymap>().recording == Some(p.owner))
        {
            set_recording(None, cx);
        }
        self.pending = None;
        cx.notify();
    }

    pub(crate) fn deactivate(&mut self, cx: &mut Context<Self>) {
        self.cancel(cx);
    }

    fn record(&mut self, row: Row, window: &mut Window, cx: &mut Context<Self>) {
        self.error = None;
        self.status = None;
        self.pending = Some(Pending {
            owner: window.window_id(),
            generation: cx.global::<Keymap>().generation,
            row,
            key: None,
            conflicts: Vec::new(),
            warnings: Vec::new(),
            reset: None,
        });
        set_recording(Some(window.window_id()), cx);
        window.focus(&self.focus, cx);
        cx.notify();
    }

    fn capture(&mut self, key: &gpui::Keystroke, window: &mut Window, cx: &mut Context<Self>) {
        if self.pending.is_none() || !is_recording(window, cx) {
            return;
        }
        cx.stop_propagation();
        if key.key == "escape" && key.modifiers == Modifiers::default() {
            self.cancel(cx);
            return;
        }
        if self
            .pending
            .as_ref()
            .is_some_and(|p| p.key.is_some() || p.reset.is_some())
        {
            return;
        }
        if [
            "shift", "control", "alt", "super", "cmd", "ctrl", "fn", "function", "platform",
        ]
        .contains(&key.key.as_str())
        {
            return;
        }
        let key = match normalize_key(&key.unparse()) {
            Ok(key) => key,
            Err(error) => {
                self.error = Some(error.to_string());
                cx.notify();
                return;
            }
        };
        let generation = cx.global::<Keymap>().generation;
        let pending = self.pending.as_mut().unwrap();
        pending.generation = generation;
        (pending.conflicts, pending.warnings) = conflicts(&pending.row, &key, cx);
        pending.key = Some(key);
        cx.notify();
    }

    fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(mut pending) = self.pending.take() else {
            return;
        };
        if pending.generation != cx.global::<Keymap>().generation {
            set_recording(None, cx);
            self.error = Some(
                "Shortcuts changed while recording. Review the updated list and record again."
                    .into(),
            );
            cx.notify();
            return;
        }
        let document = if let Some(document) = pending.reset.take() {
            document
        } else {
            let Some(key) = pending.key.take() else {
                self.pending = Some(pending);
                return;
            };
            let mut document = cx.global::<Keymap>().document.clone();
            put(
                &mut document,
                Override {
                    context: pending.row.context,
                    key,
                    action: Some(pending.row.action),
                    args: pending.row.args,
                },
            );
            document
        };
        set_recording(None, cx);
        self.persist(document, false, window, cx);
    }

    fn persist(
        &mut self,
        document: Document,
        open: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.saving {
            return;
        }
        let state = cx.global::<Keymap>();
        if state.error.is_some() {
            self.error = Some(
                "Fix keymap.json and reload before saving. Your file has been kept intact.".into(),
            );
            cx.notify();
            return;
        }
        let Some(path) = state.path.clone() else {
            self.error = Some("The user settings directory is unavailable.".into());
            cx.notify();
            return;
        };
        if let Some(reader) = self.reader.as_ref().and_then(WeakEntity::upgrade) {
            if reader_layout::settings_path_at(&reader.read(cx).vault_root, path.clone()).is_none()
            {
                self.error = Some("The keymap must be stored outside the open vault.".into());
                cx.notify();
                return;
            }
        }
        if let Err(error) = compile(&document, &state.defaults, cx) {
            self.error = Some(error.to_string());
            cx.notify();
            return;
        }
        let expected = state.seen.clone();
        self.saving = true;
        self.error = None;
        self.status = None;
        cx.spawn_in(window, async move |this, cx| {
            let destination = path.clone();
            let result = cx
                .background_executor()
                .spawn(async move { save(&destination, &expected, &document) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.saving = false;
                match result {
                    Ok(bytes) => {
                        adopt(Ok(Some(bytes)), cx);
                        this.status = Some("Shortcuts saved".into());
                        if open {
                            if let Ok(url) = url::Url::from_file_path(&path) {
                                cx.open_url(url.as_str());
                            }
                        }
                    }
                    Err(error) => this.error = Some(error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn reset_row(&mut self, row: Row, window: &mut Window, cx: &mut Context<Self>) {
        let mut document = cx.global::<Keymap>().document.clone();
        document.bindings.retain(|entry| {
            let scope = context(&entry.context)
                .ok()
                .flatten()
                .unwrap_or_else(global_context);
            !(scope == row.context
                && ((entry.action.as_deref() == Some(row.action.as_str())
                    && target_args(entry) == row.args)
                    || normalize_key(&entry.key)
                        .ok()
                        .is_some_and(|key| row.defaults.contains(&key))))
        });
        let reassigned: Vec<_> = cx
            .global::<Keymap>()
            .rows
            .iter()
            .filter(|other| {
                (other.action != row.action || other.args != row.args)
                    && other.context == row.context
                    && other.keys.iter().any(|key| row.defaults.contains(key))
            })
            .map(|other| other.label.clone())
            .collect();
        if reassigned.is_empty() {
            self.persist(document, false, window, cx);
        } else {
            self.pending = Some(Pending {
                owner: window.window_id(),
                generation: cx.global::<Keymap>().generation,
                row,
                key: None,
                conflicts: reassigned,
                warnings: Vec::new(),
                reset: Some(document),
            });
            set_recording(Some(window.window_id()), cx);
            window.focus(&self.focus, cx);
            cx.notify();
        }
    }

    fn row(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let row = self.visible[index].clone();
        let record = row.clone();
        let reset = row.clone();
        h_flex()
            .id(("keymap-row", index))
            .w_full()
            .min_h(px(76.))
            .gap_3()
            .p_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .font_weight(FontWeight::MEDIUM)
                            .child(row.label.clone()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .truncate()
                            .child(format!("{} · {}", row.context, row.action)),
                    )
                    .child(
                        div()
                            .text_xs()
                            .child(if row.changed { "Modified" } else { "Default" }),
                    ),
            )
            .child(
                h_flex()
                    .id(("keymap-keys", index))
                    .gap_1()
                    .w(px(260.))
                    .flex_none()
                    .overflow_x_scroll()
                    .children(row.keys.iter().enumerate().map(|(key_index, key)| {
                        let entry = Override {
                            context: row.context.clone(),
                            key: key.clone(),
                            action: None,
                            args: None,
                        };
                        Button::new(SharedString::from(format!(
                            "keymap-remove-{index}-{key_index}"
                        )))
                        .small()
                        .ghost()
                        .label(format!("{} ×", key_label(key)))
                        .tooltip("Disable this shortcut")
                        .disabled(self.saving || self.pending.is_some())
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                let mut document = cx.global::<Keymap>().document.clone();
                                put(&mut document, entry.clone());
                                this.persist(document, false, window, cx);
                            },
                        ))
                    })),
            )
            .child(
                Button::new(("keymap-record", index))
                    .small()
                    .label("Record")
                    .debug_selector(move || format!("keymap-record-{index}"))
                    .disabled(self.saving || self.pending.is_some())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.record(record.clone(), window, cx)
                    })),
            )
            .child(
                Button::new(("keymap-reset", index))
                    .small()
                    .ghost()
                    .label("Reset")
                    .disabled(!row.changed || self.saving || self.pending.is_some())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.reset_row(reset.clone(), window, cx)
                    })),
            )
            .into_any_element()
    }
}

pub(super) fn matches_query(row: &Row, query: &str) -> bool {
    let text = format!(
        "{} {} {} {}",
        row.label,
        row.action,
        row.context,
        row.keys
            .iter()
            .map(|key| format!("{key} {}", key_label(key)))
            .collect::<Vec<_>>()
            .join(" ")
    )
    .to_lowercase();
    text.contains(query)
        || normalize_key(&query.replace('+', "-"))
            .ok()
            .is_some_and(|key| row.keys.contains(&key))
}

impl Render for KeyboardSettings {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.pending.is_some() && !is_recording(window, cx) {
            self.pending = None;
        }
        let query = self.search.read(cx).value().to_string().to_lowercase();
        self.visible = cx
            .global::<Keymap>()
            .rows
            .iter()
            .filter(|row| matches_query(row, query.trim()))
            .cloned()
            .collect();
        let error = cx
            .global::<Keymap>()
            .error
            .clone()
            .or_else(|| self.error.clone());
        let path = cx.global::<Keymap>().path.clone();
        let can_edit = path.is_some() && !self.saving && self.pending.is_none();
        let pending = self.pending.as_ref().map(|pending| {
            let ready = pending.key.is_some() || pending.reset.is_some();
            let label = pending.key.as_ref().map(|key| key_label(key)).unwrap_or_else(|| "Press a shortcut…".into());
            v_flex().id("keymap-recording").debug_selector(|| "keymap-recording".into()).gap_2().p_3()
                .border_1().border_color(cx.theme().border).rounded_md()
                .child(format!("{} · {}", pending.row.label, pending.row.context))
                .child(h_flex().gap_1().children(["Global", "ReadMode", "Editor"].into_iter().map(|scope| {
                    let normalized = context(scope).unwrap().unwrap_or_else(global_context);
                    Button::new(SharedString::from(format!("keymap-context-{scope}"))).small().ghost().label(scope)
                        .debug_selector(move || format!("keymap-context-{scope}"))
                        .selected(pending.row.context == normalized).disabled(pending.reset.is_some())
                        .on_click(cx.listener(move |this, _, window, cx| {
                            if let Some(pending) = &mut this.pending {
                                pending.row.context = normalized.clone(); pending.row.defaults.clear();
                                pending.key = None; pending.conflicts.clear(); pending.warnings.clear();
                            }
                            window.focus(&this.focus, cx); cx.notify();
                        }))
                })))
                .child(if pending.reset.is_some() { "Reset to defaults".into() } else { label })
                .children((!pending.conflicts.is_empty()).then(|| div().debug_selector(|| "keymap-conflict".into())
                    .child(format!("Already assigned to {}. Reassigning removes that assignment in this context.", pending.conflicts.join(", ")))))
                .children(pending.warnings.iter().map(|message| div().text_sm().child(message.clone())))
                .child(h_flex().gap_2()
                    .child(Button::new("keymap-confirm").label(if pending.conflicts.is_empty() { "Assign shortcut" } else { "Reassign shortcut" })
                        .debug_selector(|| "keymap-confirm".into()).disabled(!ready)
                        .on_click(cx.listener(|this, _, window, cx| this.confirm(window, cx))))
                    .child(Button::new("keymap-cancel").ghost().label("Cancel (Esc)")
                        .on_click(cx.listener(|this, _, _, cx| this.cancel(cx)))))
        });
        v_flex().id("keymap-settings").w_full().gap_3().key_context("KeymapSettings").track_focus(&self.focus)
            .child("Changes apply on this computer. keymap.json reloads automatically; all other defaults remain available.")
            .child(gpui_component::input::Input::new(&self.search))
            .child(h_flex().gap_2()
                .child(Button::new("keymap-open-file").label("Open keymap file").disabled(!can_edit)
                    .on_click(cx.listener(|this, _, window, cx| {
                        let path = cx.global::<Keymap>().path.clone();
                        if let Some(path) = path.filter(|path| path.is_file()) {
                            if let Ok(url) = url::Url::from_file_path(path) { cx.open_url(url.as_str()); }
                        } else { this.persist(cx.global::<Keymap>().document.clone(), true, window, cx); }
                    })))
                .child(Button::new("keymap-reload").ghost().label("Reload")
                    .on_click(|_, _, cx| { if let Some(path) = cx.global::<Keymap>().path.clone() { adopt(read(&path), cx); } }))
                .child(Button::new("keymap-reset-all").ghost().label("Reset all").disabled(!can_edit)
                    .on_click(cx.listener(|this, _, window, cx| this.persist(Document::default(), false, window, cx)))))
            .children(error.map(|error| div().debug_selector(|| "keymap-error".into()).text_color(brand::palette(cx).danger).child(error)))
            .children(self.status.clone().map(|status| div().text_sm().child(status)))
            .children(pending)
            .child(div().text_sm().child(format!("{} actions / contexts", self.visible.len())))
            .child(uniform_list("keymap-action-list", self.visible.len(), cx.processor(|this, range: std::ops::Range<usize>, _, cx| {
                range.map(|index| this.row(index, cx)).collect::<Vec<_>>()
            })).w_full().h(px(460.)))
    }
}

//! Explicit context-scoped relocation UI; opening/previewing never migrates.
use super::*;
use anyhow::{ensure, Result};
use gpui_component::{
    checkbox::Checkbox,
    input::{Input, InputEvent, InputState},
    WindowExt as _,
};
use okilum_t3::service_layout::{
    migration::{self, Plan, Receipt},
    Kind, ServiceLayout,
};

gpui::actions!(service_storage, [OpenServiceStorage]);

/// External agents can use the same resolver without launching a GUI, reading
/// credentials or inventing an ops fallback. This command is read-only.
pub(crate) fn command_line() -> Option<i32> {
    let arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
    if arguments.first().is_none_or(|arg| arg != "service-path") {
        return None;
    }
    if arguments.len() != 3 {
        diagnostic_error!("Usage: okilum service-path <owning-context> <project|board|board-local|service-filename> (JSON; read-only)");
        return Some(64);
    }
    let result = (|| -> Result<serde_json::Value> {
        let context = PathBuf::from(&arguments[1]);
        let layout = ServiceLayout::resolve(&context)?;
        let name = arguments[2]
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("Invalid service filename"))?;
        let path = if matches!(name, "project" | "board" | "board-local") {
            layout
                .preferred(name)?
                .ok_or_else(|| anyhow::anyhow!("Selected service file is absent"))?
        } else {
            let path = layout.path(name)?;
            ensure!(
                std::fs::symlink_metadata(&path)?.is_file(),
                "Service file is not regular"
            );
            path
        };
        layout.check_current()?;
        Ok(
            serde_json::json!({"context":layout.context,"directory":layout.directory(),"layout":layout.kind,"path":path}),
        )
    })();
    Some(match result {
        Ok(value) => {
            diagnostic_println!("{value}");
            0
        }
        Err(error) => {
            diagnostic_error!("Service storage: {error:#}");
            1
        }
    })
}

#[derive(Clone)]
enum State {
    Initial,
    Plan(Plan),
    Pending(Receipt),
    PendingRollback(Receipt),
    Done(Receipt),
    Hidden(Option<Receipt>),
}

struct Storage {
    root: PathBuf,
    local_state: Option<PathBuf>,
    reader: WeakEntity<Reader>,
    context: Entity<InputState>,
    _context: Subscription,
    state: State,
    generation: u64,
    busy: bool,
    confirmed: bool,
    error: Option<String>,
}

impl Reader {
    pub(super) fn open_service_storage(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.file_menu = None;
        let root = self.vault_root.clone();
        let relative = self
            .project_navigation
            .active
            .as_ref()
            .and_then(|p| p.folder.strip_prefix(&root).ok())
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.default_new_project_folder());
        let state = self.session_directory.clone();
        let owner = cx.entity().downgrade();
        let view = cx.new(|cx| Storage::new(root, relative, state, owner, window, cx));
        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .title("Project service storage")
                .width(px(700.))
                .on_ok(|_, _, _| false)
                .child(view.clone())
        });
    }
}

impl Storage {
    fn new(
        root: PathBuf,
        relative: String,
        state: Option<PathBuf>,
        owner: WeakEntity<Reader>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| {
            let mut input = InputState::new(window, cx)
                .placeholder("Owning project folder, relative to this vault");
            input.set_value(relative, window, cx);
            input
        });
        let subscription = cx.subscribe(&input, |this: &mut Storage, _, event, cx| {
            if matches!(event, InputEvent::Change) {
                this.generation = this.generation.wrapping_add(1);
                this.state = State::Initial;
                this.confirmed = false;
                this.error = None;
                cx.notify();
            }
        });
        Storage {
            root,
            local_state: state,
            reader: owner,
            context: input,
            _context: subscription,
            state: State::Initial,
            generation: 0,
            busy: false,
            confirmed: false,
            error: None,
        }
    }

    fn folder(&self, cx: &App) -> Result<PathBuf> {
        let value = self.context.read(cx).value();
        let relative = Path::new(value.trim());
        ensure!(
            !relative.is_absolute()
                && relative.components().all(|c| matches!(
                    c,
                    std::path::Component::Normal(_) | std::path::Component::CurDir
                )),
            "Choose an owning context inside this vault"
        );
        let folder = self.root.join(relative);
        ensure!(
            std::fs::symlink_metadata(&folder)?.is_dir(),
            "Owning context must be a real directory"
        );
        let folder = folder.canonicalize()?;
        ensure!(
            folder.starts_with(self.root.canonicalize()?),
            "Context escapes the vault"
        );
        Ok(folder)
    }

    fn run(
        &mut self,
        job: impl FnOnce() -> Result<State> + Send + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.error = None;
        let generation = self.generation;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx.background_executor().spawn(async move { job() }).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                if generation != this.generation {
                    return;
                }
                match result {
                    Ok(state) => {
                        let changed = matches!(state, State::Done(_));
                        this.state = state;
                        this.confirmed = false;
                        if changed {
                            let _ = this.reader.update(cx, |reader, cx| {
                                reader.refresh_service_metadata(window, cx)
                            });
                        }
                    }
                    Err(error) => {
                        this.error = Some(format!(
                            "{error:#}. Review storage again to inspect any pending recovery."
                        ))
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn review(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let folder = match self.folder(cx) {
            Ok(p) => p,
            Err(e) => {
                self.error = Some(format!("{e:#}"));
                cx.notify();
                return;
            }
        };
        let state = self.local_state.clone();
        self.run(move || {
            if let Some(pending) = migration::pending(&folder)? { return Ok(State::Pending(pending)); }
            let layout = ServiceLayout::resolve(&folder)?;
            ensure!(layout.preferred("project")?.is_some(), "This folder does not own project service files; select the owning Project/Area");
            if let Some(state) = state {
                let registry = layout.path("thread-ids.md")?;
                if registry.is_file() {
                    ensure!(!okilum_core::file_editor::FileEditor::has_unsaved_draft(&registry, &state.join("manager-registry-drafts"))?,
                        "Recover the unsaved Manager registry draft before migration");
                }
            }
            if layout.kind == Kind::Hidden { return Ok(State::Hidden(migration::latest(&folder)?)); }
            Plan::preview(&folder).map(State::Plan)
        }, window, cx);
    }

    fn apply(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let confirmed = self.confirmed;
        let state = self.state.clone();
        self.run(
            move || match state {
                State::Plan(plan) => plan.apply(confirmed).map(State::Done),
                State::Pending(receipt) => {
                    migration::resume(&receipt.plan.source.context, confirmed).map(State::Done)
                }
                State::PendingRollback(receipt) => {
                    migration::cancel_pending(&receipt.plan.source.context, confirmed)
                        .map(State::Done)
                }
                _ => anyhow::bail!("Review the concrete migration or rollback plan before Apply"),
            },
            window,
            cx,
        );
    }

    fn rollback(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.state.clone() {
            State::Done(receipt) | State::Hidden(Some(receipt)) => {
                self.run(
                    move || migration::rollback_preview(&receipt).map(State::Plan),
                    window,
                    cx,
                );
            }
            State::Pending(receipt) => {
                self.state = State::PendingRollback(receipt);
                self.confirmed = false;
                cx.notify();
            }
            _ => {}
        }
    }
}

impl Render for Storage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let summary = match &self.state {
            State::Initial => "Review a specific owning Project/Area. Reviewing does not change files.".to_owned(),
            State::Plan(plan) => plan.description(),
            State::Pending(receipt) => format!("Interrupted migration {}. All normal metadata consumers remain blocked until explicit recovery.\n{}", receipt.id, receipt.plan.description()),
            State::PendingRollback(receipt) => format!("Rollback plan for {}: restore the original coherent layout. Intervening edits refuse rollback and remain preserved.\n{}", receipt.id, receipt.plan.description()),
            State::Done(receipt) => format!("{:?}: {}\nBackup and journal retained outside the vault.\n{}", receipt.phase, receipt.id, receipt.plan.description()),
            State::Hidden(_) => "This project already uses hidden service storage. Existing settings, notes and workspace are unchanged.".to_owned(),
        };
        let applicable = matches!(
            self.state,
            State::Plan(_) | State::Pending(_) | State::PendingRollback(_)
        );
        let rollback = matches!(
            self.state,
            State::Done(_) | State::Hidden(Some(_)) | State::Pending(_)
        );
        v_flex().gap_3().w_full()
            .child(div().child("Owning project folder (relative to this vault; empty means vault root)"))
            .child(div().debug_selector(|| "service-storage-context".into()).child(Input::new(&self.context).disabled(self.busy)))
            .child(Button::new("service-storage-review").debug_selector(|| "service-storage-review".into()).label("Review storage").disabled(self.busy)
                .on_click(cx.listener(|this, _, window, cx| this.review(window, cx))))
            .child(div().id("service-storage-plan").max_h(px(240.)).overflow_y_scroll().text_sm().child(summary))
            .child(div().text_sm().child("Only the listed application service files move. Ordinary ops notes/runbooks/decisions/daily, existing .okilum settings, credentials, Manager identity and its workspace stay in place. The local backup and journal support explicit recovery and rollback."))
            .when(applicable, |view| view.child(Checkbox::new("service-storage-compatible").debug_selector(|| "service-storage-compatible".into())
                .label("All used clients support this layout; metadata editors and Sync writers are paused")
                .checked(self.confirmed).disabled(self.busy)
                .on_click(cx.listener(|this, value: &bool, _, cx| { this.confirmed = *value; cx.notify(); }))))
            .when(applicable, |view| view.child(Button::new("service-storage-apply").debug_selector(|| "service-storage-apply".into())
                .label(if matches!(self.state, State::PendingRollback(_)) { "Apply rollback" } else if matches!(self.state, State::Pending(_)) { "Resume migration" } else { "Apply reviewed plan" })
                .disabled(self.busy || !self.confirmed)
                .on_click(cx.listener(|this, _, window, cx| this.apply(window, cx)))))
            .when(rollback, |view| view.child(Button::new("service-storage-rollback").debug_selector(|| "service-storage-rollback".into()).label("Review rollback")
                .disabled(self.busy).on_click(cx.listener(|this, _, window, cx| this.rollback(window, cx)))))
            .when_some(self.error.clone(), |view, error| view.child(div().text_color(cx.theme().danger).child(error)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    use gpui::{TestAppContext, VisualTestContext};
    fn click(visual: &mut VisualTestContext, selector: &'static str) {
        let bounds = visual
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("mounted control {selector}"));
        visual.simulate_click(bounds.center(), Modifiers::default());
        visual.run_until_parked();
    }

    #[gpui::test]
    fn service2772_review_apply_refuses_stale_preview_and_requires_explicit_confirmation(
        cx: &mut TestAppContext,
    ) {
        cx.update(crate::test_init);
        let temp = tempfile::tempdir().unwrap();
        let vault = temp.path().join("vault");
        let context = vault.join("Areas/App");
        std::fs::create_dir_all(context.join("ops/daily")).unwrap();
        std::fs::write(vault.join("Start.md"), "# Synthetic\n").unwrap();
        std::fs::write(
            context.join("ops/project.yaml"),
            "# preserve YAML\nname: Synthetic\nkind: knowledge\n",
        )
        .unwrap();
        std::fs::write(
            context.join("ops/daily/keep.md"),
            "# keep ordinary ops notes\r\n",
        )
        .unwrap();
        let mut owner = None;
        let mut storage = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let reader = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(vault.clone()),
                        note: Some("Start.md".into()),
                        index_dir: Some(temp.path().join("index")),
                        session_directory: Some(temp.path().join("state")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            owner = Some(reader.clone());
            let view = cx.new(|cx| {
                Storage::new(
                    vault.clone(),
                    "Areas/App".into(),
                    Some(temp.path().join("state")),
                    reader.downgrade(),
                    window,
                    cx,
                )
            });
            storage = Some(view.clone());
            Root::new(view, window, cx)
        });
        visual.simulate_resize(size(px(900.), px(900.)));
        visual.run_until_parked();
        let storage = storage.unwrap();
        click(visual, "service-storage-review");
        storage.read_with(visual, |view, _| {
            assert!(matches!(view.state, State::Plan(_)), "{:?}", view.error)
        });
        click(visual, "service-storage-apply");
        assert!(context.join("ops/project.yaml").is_file());
        assert!(
            !context.join(".okilum/project.yaml").exists(),
            "disabled Apply cannot mutate"
        );
        let newer = "# intervening edit\nname: Newer\nkind: knowledge\n";
        std::fs::write(context.join("ops/project.yaml"), newer).unwrap();
        click(visual, "service-storage-compatible");
        click(visual, "service-storage-apply");
        storage.read_with(visual, |view, _| {
            assert!(view.error.is_some(), "stale plan must be refused")
        });
        assert_eq!(
            std::fs::read_to_string(context.join("ops/project.yaml")).unwrap(),
            newer
        );
        click(visual, "service-storage-review");
        click(visual, "service-storage-compatible");
        click(visual, "service-storage-apply");
        storage.read_with(visual, |view, _| {
            assert!(matches!(view.state, State::Done(_)), "{:?}", view.error)
        });
        assert_eq!(
            std::fs::read_to_string(context.join(".okilum/project.yaml")).unwrap(),
            newer
        );
        assert!(!context.join("ops/project.yaml").exists());
        assert!(!vault.join("ops").exists());
        assert_eq!(
            std::fs::read(context.join("ops/daily/keep.md")).unwrap(),
            b"# keep ordinary ops notes\r\n"
        );
        click(visual, "service-storage-rollback");
        click(visual, "service-storage-compatible");
        click(visual, "service-storage-apply");
        assert_eq!(
            std::fs::read_to_string(context.join("ops/project.yaml")).unwrap(),
            newer
        );
        assert!(!context.join(".okilum/project.yaml").exists());
        let _ = owner;
    }
}

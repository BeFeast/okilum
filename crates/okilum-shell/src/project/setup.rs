//! Local Manager setup, separate from project creation and thread launch.
use super::*;
use okilum_t3::projects::local_manager as model;

pub(super) struct Form {
    session: model::Session,
    origin: Entity<InputState>,
    workspace: Entity<InputState>,
}
impl Form {
    fn new(session: model::Session, origin: String, window: &mut Window, cx: &mut App) -> Self {
        let address = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Local T3 address, for example localhost:3778")
        });
        address.update(cx, |input, cx| input.set_value(origin, window, cx));
        let workspace =
            cx.new(|cx| InputState::new(window, cx).placeholder("Workspace folder on this device"));
        workspace.update(cx, |input, cx| {
            input.set_value(session.suggested_workspace(), window, cx)
        });
        address.update(cx, |input, cx| input.focus(window, cx));
        Self {
            session,
            origin: address,
            workspace,
        }
    }
}
impl ProjectView {
    pub(super) fn open_local_setup(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let Some(area) = self.area.clone() else {
            return;
        };
        let origin = model::environment(&self.origin.read(cx).value())
            .ok()
            .or_else(|| {
                cx.try_global::<reader_t3_settings::Connection>()
                    .and_then(|c| c.origin.as_deref())
                    .and_then(|s| model::environment(s).ok())
            })
            .map(|env| env.origin())
            .unwrap_or_default();
        self.busy = true;
        self.error = None;
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { model::Session::load(&area) })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                match result {
                    Ok(session) => this.local_setup = Some(Form::new(session, origin, window, cx)),
                    Err(e) => this.error = Some(format!("{e:#}")),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
    fn local_setup_environment(&self, cx: &App) -> anyhow::Result<Environment> {
        let form = self
            .local_setup
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Open local Manager setup first"))?;
        model::environment(&form.origin.read(cx).value())
    }
    fn sign_in_local(&mut self, cx: &mut Context<Self>) {
        match self.local_setup_environment(cx) {
            Ok(env) => reader_t3_settings::sign_in_to(env, self.state.clone(), cx),
            Err(e) => self.error = Some(format!("{e:#}")),
        }
        cx.notify();
    }
    fn save_local_setup(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let env = match self.local_setup_environment(cx) {
            Ok(env) => env,
            Err(e) => {
                self.error = Some(format!("{e:#}"));
                cx.notify();
                return;
            }
        };
        let Some(form) = &self.local_setup else {
            return;
        };
        let session = form.session.clone();
        let workspace = form.workspace.read(cx).value().to_string();
        let existing = reader_t3_settings::gateway(&env, cx);
        self.error = None;
        self.busy = true;
        let access = reader_features::access(reader_features::Feature::Agents, cx);
        cx.spawn_in(window, async move |this, cx| {
            let result = async {
                let gateway = match existing {
                    Some(gateway) => gateway,
                    None => {
                        #[cfg(target_os = "macos")]
                        let token: Option<StoredToken> = None;
                        #[cfg(not(target_os = "macos"))]
                        let token = {
                            let task = this.update_in(cx, |_, _, cx| {
                                cx.read_credentials(&env.credential_url())
                            })?;
                            task.await?
                                .map(|(_, bytes)| StoredToken::from_secret(&bytes))
                                .transpose()?
                        };
                        let target = env.clone();
                        cx.background_executor()
                            .spawn(async move {
                                reader_project_navigation::manager_gateway(
                                    target.clone(),
                                    Arc::new(
                                        McpGateway::new(target, token)?.with_access(access.clone()),
                                    ),
                                    access,
                                )
                            })
                            .await?
                    }
                };
                let target = env.clone();
                cx.background_executor()
                    .spawn(async move {
                        let project = session.bind(gateway.as_ref(), &target, &workspace)?;
                        let binding = project.native_binding()?.ok_or_else(|| {
                            anyhow::anyhow!("Manager binding is missing after setup")
                        })?;
                        Ok::<_, anyhow::Error>((binding, target, gateway))
                    })
                    .await
            }
            .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                match result {
                    Ok((binding, env, gateway)) => {
                        this.binding = Some(binding);
                        this.project_override = true;
                        this.local_setup = None;
                        this.origin
                            .update(cx, |input, cx| input.set_value(env.origin(), window, cx));
                        this.finish_connection(Ok((env, gateway)), window, cx);
                        this.reason = Some(
                            "Local T3 and workspace checked. Start Manager when ready.".into(),
                        );
                    }
                    Err(e) => this.error = Some(format!("{e:#}")),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
    pub(super) fn render_local_setup(&self, cx: &mut Context<Self>) -> AnyElement {
        let form = self.local_setup.as_ref().expect("local setup");
        v_flex().id("project-local-form").debug_selector(|| "project-local-form".into())
            .size_full().p_4().gap_3().bg(brand::palette(cx).canvas).text_color(brand::palette(cx).text)
            .child("Set up local Manager")
            .child(div().whitespace_normal().text_color(brand::palette(cx).text_muted)
                .child("Use T3 running on this device beside your checkout. Setup checks the connection and folder; it does not start a Manager."))
            .child("Local T3 address")
            .child(div().debug_selector(|| "project-local-origin".into()).child(Input::new(&form.origin).disabled(self.busy)))
            .child("Workspace on this device")
            .child(div().debug_selector(|| "project-local-workspace".into()).child(Input::new(&form.workspace).disabled(self.busy)))
            .when_some(self.error.clone(), |view,error| view.child(div().debug_selector(|| "project-local-error".into()).whitespace_normal().text_color(cx.theme().danger).child(error)))
            .child(h_flex().gap_2()
                .child(Button::new("project-local-sign-in").debug_selector(|| "project-local-sign-in".into()).ghost().label("Sign in to local T3").disabled(self.busy)
                    .on_click(cx.listener(|this,_,_,cx| this.sign_in_local(cx))))
                .child(Button::new("project-local-save").debug_selector(|| "project-local-save".into()).label("Check and connect").disabled(self.busy)
                    .on_click(cx.listener(|this,_,window,cx| this.save_local_setup(window,cx))))
                .child(Button::new("project-local-cancel").debug_selector(|| "project-local-cancel".into()).ghost().label("Cancel").disabled(self.busy)
                    .on_click(cx.listener(|this,_,_,cx| {this.local_setup=None;this.error=None;cx.notify();}))))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests;

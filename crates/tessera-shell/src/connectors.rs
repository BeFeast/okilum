//! Ordinary, retained connector settings. Stores references, never token values.
mod suggestions;
mod t3_routes;
use super::brain::rpc_guarded;
use gpui::*;
use gpui_component::{
    button::ButtonVariants as _,
    h_flex,
    input::{Input, InputState},
    v_flex, Disableable as _,
};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    net::SocketAddr,
};
use suggestions::Suggestions;

const FIELDS: &[(&str, &str, &str)] = &[
    (
        "maestro.base_url",
        "Maestro API origin reachable from the backend",
        "",
    ),
    ("maestro.instance_id", "Stable Maestro instance UUID", ""),
    (
        "maestro.token_env",
        "Optional credential reference: env:NAME or file:/absolute/path",
        "",
    ),
    (
        "maestro.ui_origin",
        "Optional Maestro origin reachable from this desktop",
        "",
    ),
    ("actor", "Review actor", "local operator"),
    (
        "chat.base_url",
        "CLIProxyAPI URL",
        "http://127.0.0.1:23020/v1",
    ),
    (
        "chat.api_key_env",
        "Credential reference: env:NAME or file:/absolute/path",
        "env:TESSERA_CHAT_TOKEN",
    ),
    (
        "chat.model",
        "Model (choose below after Check connections)",
        "unselected",
    ),
    (
        "todoist.base_url",
        "Todoist API URL",
        "https://api.todoist.com/api/v1",
    ),
    ("todoist.instance_id", "Task account label", "personal"),
    (
        "todoist.token_env",
        "Credential reference: env:NAME or file:/absolute/path",
        "env:TESSERA_TODOIST_TOKEN",
    ),
    ("t3.base_url", "T3 URL", "http://127.0.0.1:23010"),
    (
        "t3.token_env",
        "Credential reference: env:NAME or file:/absolute/path",
        "env:TESSERA_T3_TOKEN",
    ),
    (
        "t3.environment_id",
        "Environment (populated by discovery)",
        "unselected",
    ),
    (
        "t3.project_id",
        "Project (choose below after Check connections)",
        "unselected",
    ),
    (
        "t3.model_instance_id",
        "Model provider (populated by model choice)",
        "unselected",
    ),
    (
        "t3.model",
        "Model (choose below after Check connections)",
        "unselected",
    ),
    ("t3.runtime_mode", "Runtime mode", "approval-required"),
    ("t3.interaction_mode", "Interaction mode", "default"),
];
pub struct ConnectionsChanged;
impl EventEmitter<ConnectionsChanged> for ConnectorsView {}
pub struct ConnectorsView {
    endpoint: SocketAddr,
    identity: Value,
    fields: BTreeMap<String, Entity<InputState>>,
    enabled: BTreeSet<String>,
    states: Value,
    choices: Value,
    busy: bool,
    message: String,
    suggestions: Suggestions,
    target_routes: t3_routes::TargetRoutes,
}
impl ConnectorsView {
    pub fn new(
        endpoint: SocketAddr,
        identity: Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let fields = FIELDS
            .iter()
            .map(|(key, label, default)| {
                let input = cx.new(|cx| InputState::new(window, cx).placeholder(*label));
                input.update(cx, |input, cx| input.set_value(*default, window, cx));
                (key.to_string(), input)
            })
            .collect();
        let this = Self {
            endpoint,
            identity,
            fields,
            enabled: BTreeSet::new(),
            states: Value::Null,
            choices: Value::Null,
            busy: false,
            message: String::new(),
            suggestions: Suggestions::default(),
            target_routes: t3_routes::TargetRoutes::default(),
        };
        cx.spawn_in(window, async move |this, cx| {
            let _ = this.update_in(cx, |this, window, cx| {
                this.request("connectors_get", window, cx)
            });
        })
        .detach();
        this
    }
    fn config(&self, cx: &App) -> Value {
        let mut config = json!({"actor":"","chat":null,"todoist":null,"t3":null,"maestro":null});
        for (key, input) in &self.fields {
            let value = input.read(cx).value().trim().to_string();
            if let Some((provider, field)) = key.split_once('.') {
                if self.enabled.contains(provider) {
                    if config[provider].is_null() {
                        config[provider] = json!({});
                    }
                    config[provider][field] = if provider == "maestro"
                        && matches!(field, "token_env" | "ui_origin")
                        && value.is_empty()
                    {
                        Value::Null
                    } else {
                        json!(value)
                    };
                }
            } else {
                config[key] = json!(value);
            }
        }
        config
    }
    fn load(&mut self, config: &Value, window: &mut Window, cx: &mut Context<Self>) {
        self.enabled = ["chat", "todoist", "t3", "maestro"]
            .iter()
            .filter(|key| config[**key].is_object())
            .map(|s| s.to_string())
            .collect();
        for (key, input) in &self.fields {
            let value = if let Some((provider, field)) = key.split_once('.') {
                &config[provider][field]
            } else {
                &config[key]
            };
            if key.starts_with("maestro.") && value.is_null() {
                input.update(cx, |input, cx| input.set_value("", window, cx));
            }
            if let Some(value) = value.as_str() {
                input.update(cx, |input, cx| {
                    input.set_value(value.to_string(), window, cx)
                });
            }
        }
    }
    fn request(&mut self, operation: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let mut request = json!({"op":operation});
        if ["connectors_save", "connectors_check"].contains(&operation) {
            let form = self.config(cx);
            request["config"] = if operation == "connectors_save" {
                match self.generic_connector_config(form) {
                    Ok(config) => config,
                    Err(error) => {
                        self.message = error;
                        cx.notify();
                        return;
                    }
                }
            } else {
                form
            };
        }
        let form_revision = self.config(cx);
        let operation = operation.to_string();
        let endpoint = self.endpoint;
        let identity = self.identity.clone();
        self.busy = true;
        self.message = "Connecting…".into();
        cx.notify();
        cx.spawn_in(window,async move |this,cx|{
            let reply=cx.background_executor().spawn(async move{rpc_guarded(endpoint,request,Some(&identity))}).await;
            let _=this.update_in(cx,|this,window,cx|{
                this.busy=false;
                match reply {
                    Ok(data)=>{
                        let changed_during_request = this.config(cx) != form_revision;
                        if operation!="connectors_check" && data["config"].is_object(){this.remember_target_baseline(&data["config"]);}
                        if data["config"].is_object() && !changed_during_request {this.load(&data["config"],window,cx);}
                        this.states=data["states"].clone();
                        if data["choices"].is_object(){this.choices=data["choices"].clone();}
                        this.message=match operation.as_str(){"connectors_save"=>"Settings saved. Return to Project brain to use the refreshed connections.","connectors_reconnect"=>"Credential references reread. Existing work stays attached; chat is not resent.","connectors_check"=>"Discovery completed. Select model/project choices, then Save and connect.",_=>"Settings contain credential references only; their files are read on the backend."}.into();
                        if changed_during_request {this.message="The previous request completed. Newer edits are preserved; save them explicitly to apply.".into();}
                        if ["connectors_save","connectors_reconnect"].contains(&operation.as_str()){cx.emit(ConnectionsChanged);}
                        this.refresh_suggestions(window, cx);
                        this.refresh_target_routes(operation != "connectors_check" && !changed_during_request, window, cx);
                    },
                    Err(error)=>this.message=error,
                }
                cx.notify();
            });
        }).detach();
    }
    fn set(&self, key: &str, value: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(input) = self.fields.get(key) {
            input.update(cx, |input, cx| {
                input.set_value(value.to_string(), window, cx)
            });
        }
    }
}
impl Render for ConnectorsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = super::brand::palette(cx);
        let header = v_flex()
            .p_6()
            .gap_3()
            .border_b_1()
            .border_color(colors.border_subtle)
            .child(
                div()
                    .text_2xl()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Connections"),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(colors.text_muted)
                    .child("Models, tasks and execution — connected to this brain."),
            )
            .child(
                h_flex()
                    .gap_2()
                    .flex_wrap()
                    .child(
                        super::brand::control("connections-check", cx)
                            .label("Check connections")
                            .disabled(self.busy)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.request("connectors_check", window, cx)
                            })),
                    )
                    .child(
                        super::brand::control("connections-save", cx)
                            .primary()
                            .label("Save and connect")
                            .disabled(self.busy)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.request("connectors_save", window, cx)
                            })),
                    )
                    .child(
                        super::brand::control("connections-reconnect", cx)
                            .label("Reconnect saved settings")
                            .disabled(self.busy)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.request("connectors_reconnect", window, cx)
                            })),
                    ),
            )
            .child(div().text_sm().child(self.message.clone()));
        let mut form = v_flex().id("connector-settings").flex_1().min_h_0().overflow_y_scroll().p_6().gap_5()
            .child(self.suggestions_card(cx))
            .child(div().text_sm().text_color(colors.text_muted).child("Credential references only. Renew tokens in their existing file or environment reference on the backend."))
            .child(v_flex().gap_2().max_w(px(880.)).child(div().text_sm().font_weight(FontWeight::MEDIUM).child("Review actor"))
                .child(Input::new(&self.fields["actor"]).disabled(self.busy)));
        for (provider, label) in [
            ("chat", "CLIProxyAPI"),
            ("todoist", "Todoist"),
            ("t3", "T3 Code"),
            ("maestro", "Maestro — linked work"),
        ] {
            let enabled = self.enabled.contains(provider);
            let mut card = v_flex()
                .id(SharedString::from(format!("connector-card-{provider}")))
                .max_w(px(880.))
                .p_5()
                .gap_3()
                .rounded(px(10.))
                .border_1()
                .border_color(colors.border_subtle)
                .bg(colors.surface_raised)
                .child(
                    super::brand::control(
                        SharedString::from(format!("connector-enable-{provider}")),
                        cx,
                    )
                    .label(format!("{} {}", if enabled { "✓" } else { "○" }, label))
                    .disabled(self.busy)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.enabled.remove(provider) {
                            this.enabled.insert(provider.into());
                        }
                        cx.notify();
                    })),
                );
            if !enabled {
                card = card.child("Unconfigured");
                if provider == "maestro" {
                    card = card.child(div().text_sm().child("Observation only. Link an existing issue from a goal’s Execution view. Stopping observation preserves Maestro work and saved evidence."));
                    if let Some(projects) = self.choices["maestro"]["projects"].as_array() {
                        card = card.child(
                            div()
                                .text_sm()
                                .child(format!("{} supported projects discovered", projects.len())),
                        );
                    }
                }
                form = form.child(card);
                continue;
            }
            let status = self.states[provider]["status"]
                .as_str()
                .unwrap_or("unconfigured");
            let message = self.states[provider]["message"]
                .as_str()
                .unwrap_or("Check the connection after entering its settings.");
            card = card.child(format!("{status} · {message}"));
            for (key, label, _) in FIELDS
                .iter()
                .filter(|(key, _, _)| key.starts_with(&format!("{provider}.")))
            {
                card = card
                    .child(div().text_sm().child(*label))
                    .child(Input::new(&self.fields[*key]).disabled(self.busy));
            }
            if provider == "chat" {
                for (index, model) in self.choices["chat_models"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .take(100)
                    .enumerate()
                {
                    if let Some(model) = model.as_str() {
                        let model = model.to_string();
                        card = card.child(
                            super::brand::control(("chat-model-choice", index), cx)
                                .label(model.clone())
                                .disabled(self.busy)
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.set("chat.model", &model, window, cx)
                                })),
                        );
                    }
                }
            }
            if provider == "t3" {
                card = card.child(self.target_routes_card(_window, cx));
                let environment = self.choices["t3"]["environment_id"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                for (index, project) in self.choices["t3"]["projects"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .take(100)
                    .enumerate()
                {
                    let id = project["id"].as_str().unwrap_or_default().to_string();
                    let environment = environment.clone();
                    card = card.child(
                        super::brand::control(("t3-project-choice", index), cx)
                            .label(project["name"].as_str().unwrap_or("Project").to_string())
                            .disabled(self.busy)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.set("t3.project_id", &id, window, cx);
                                this.set("t3.environment_id", &environment, window, cx);
                            })),
                    );
                }
                for (index, model) in self.choices["t3"]["models"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .take(100)
                    .enumerate()
                {
                    let id = model["instance_id"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string();
                    let model_id = model["model"].as_str().unwrap_or_default().to_string();
                    card = card.child(
                        super::brand::control(("t3-model-choice", index), cx)
                            .label(model["name"].as_str().unwrap_or("Model").to_string())
                            .disabled(self.busy)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.set("t3.model_instance_id", &id, window, cx);
                                this.set("t3.model", &model_id, window, cx);
                            })),
                    );
                }
            }
            if provider == "maestro" {
                card = card.child(div().text_sm().child("Observation only. Link an existing issue from a goal’s Execution view. Stopping observation preserves Maestro work and saved evidence."));
                if let Some(projects) = self.choices["maestro"]["projects"].as_array() {
                    card = card.child(
                        div()
                            .text_sm()
                            .child(format!("{} supported projects discovered", projects.len())),
                    );
                }
            }
            form = form.child(card);
        }
        v_flex().size_full().min_h_0().bg(colors.surface).text_color(colors.text).child(header).child(form.child(div().text_sm().text_color(colors.text_muted).child("Existing work stays attached to its account and project. Reconnect the same account to recover it.")))
    }
}

#[cfg(test)]
mod maestro_tests {
    use super::*;
    use ::core::prelude::v1::test;
    #[gpui::test]
    fn loading_existing_maestro_settings_preserves_them_and_optional_nulls(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_component::init);
        cx.add_window_view(|window,cx|{
            let mut view=ConnectorsView::new("127.0.0.1:1".parse().unwrap(),json!({}),window,cx);view.busy=true;
            let config=json!({"actor":"operator","chat":null,"todoist":null,"t3":null,"maestro":{"base_url":"http://127.0.0.1:8786","instance_id":"aa000000-0000-4000-8000-000000000152","token_env":null,"ui_origin":"https://maestro.example.test"}});
            view.load(&config,window,cx);assert_eq!(view.config(cx),config);
            let mut without_ui=config;without_ui["maestro"]["ui_origin"]=Value::Null;
            view.load(&without_ui,window,cx);assert_eq!(view.config(cx),without_ui);view
        });
    }
}

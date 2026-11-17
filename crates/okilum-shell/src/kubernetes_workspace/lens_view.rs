//! Rendering of the Triage and Service path lenses. Both only select objects
//! in the shared inspector; logs still start only from its explicit Open logs.
use super::lenses::{self, Lens, Routing, Target, Triage, RULES};
use super::model::{self, Key};
use super::*;
use okilum_core::kubernetes::Kind;

fn caps(text: impl Into<SharedString>, cx: &App) -> Div {
    div()
        .text_xs()
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(brand::palette(cx).text_muted)
        .child(text.into())
}

fn muted(text: impl Into<SharedString>, cx: &App) -> Div {
    div()
        .text_xs()
        .text_color(brand::palette(cx).text_muted)
        .child(text.into())
}

fn yes_no(value: Option<bool>) -> &'static str {
    match value {
        Some(true) => "yes",
        Some(false) => "no",
        None => "unknown",
    }
}

impl Workspace {
    pub(crate) fn render_lens_switch(&self, cx: &mut Context<Self>) -> AnyElement {
        let tokens = brand::palette(cx);
        h_flex()
            .debug_selector(|| "k8s-lenses".into())
            .flex_none()
            .p(px(2.))
            .rounded(px(6.))
            .bg(tokens.border_subtle.opacity(0.5))
            .children(Lens::ALL.map(|lens| {
                Button::new(SharedString::from(format!("k8s-lens-{}", lens.id())))
                    .debug_selector(move || format!("k8s-lens-{}", lens.id()))
                    .small()
                    .ghost()
                    .label(lens.label())
                    .selected(self.lens == lens)
                    .on_click(cx.listener(move |this, _, _, cx| this.set_lens(lens, cx)))
            }))
            .into_any_element()
    }

    fn tile(
        &self,
        title: &'static str,
        value: String,
        note: &'static str,
        tone: Tone,
        cx: &App,
    ) -> Div {
        let tokens = brand::palette(cx);
        let color = match tone {
            Tone::Neutral => tokens.text,
            tone => tone_color(tone, cx),
        };
        v_flex()
            .flex_1()
            .min_w(px(150.))
            .gap(px(2.))
            .px_3()
            .py_2()
            .rounded(px(8.))
            .border_1()
            .border_color(tokens.border)
            .child(caps(title, cx))
            .child(
                div()
                    .text_lg()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(color)
                    .child(value),
            )
            .child(muted(note, cx))
    }

    pub(crate) fn render_triage_band(&self, cx: &mut Context<Self>) -> AnyElement {
        let tokens = brand::palette(cx);
        let band = v_flex()
            .debug_selector(|| "k8s-triage".into())
            .id("k8s-triage")
            // Shrinks (and scrolls) before the list or the dock do.
            .flex_shrink(1.)
            .min_h(px(80.))
            .max_h(px(260.))
            .overflow_y_scroll()
            .gap_2()
            .px_4()
            .py_2()
            .border_b_1()
            .border_color(tokens.border);
        let Some(data) = &self.data else {
            return band
                .child(muted("Load a scope to see what needs attention.", cx))
                .into_any_element();
        };
        let (summary, observed_at, stale) = match lenses::triage(data) {
            Triage::Unread => {
                return band
                    .child(muted("Pods not read yet: attention unknown.", cx))
                    .into_any_element()
            }
            Triage::Unknown(why) => {
                return band
                    .child(
                        div()
                            .debug_selector(|| "k8s-triage-unknown".into())
                            .text_sm()
                            .text_color(tokens.danger)
                            .child(why),
                    )
                    .into_any_element()
            }
            Triage::Ready {
                summary,
                observed_at,
                stale,
                ..
            } => (summary, observed_at, stale),
        };
        let denominator = if summary.complete {
            format!("{}", summary.pods)
        } else {
            format!("{} (first page)", summary.pods)
        };
        let tiles = h_flex()
            .gap_2()
            .flex_wrap()
            .child(self.tile(
                "PODS READY",
                format!("{} / {denominator}", summary.pods_ready),
                "Readiness, not application health",
                if summary.pods_ready < summary.pods {
                    Tone::Warning
                } else {
                    Tone::Good
                },
                cx,
            ))
            .child(self.tile(
                "NEEDS ATTENTION",
                format!("{} of {denominator} Pods", summary.pods_with_observations),
                "Includes recovered prior terminations",
                if summary.pods_with_observations > 0 {
                    Tone::Warning
                } else {
                    Tone::Good
                },
                cx,
            ))
            .child(
                self.tile(
                    "RESTARTS",
                    summary
                        .restarts
                        .map_or_else(|| "unknown".into(), |n| n.to_string()),
                    "Cumulative counter, not new incidents",
                    Tone::Neutral,
                    cx,
                ),
            )
            .child(self.tile(
                "METRICS",
                "Not read".into(),
                "No metrics API is used",
                Tone::Neutral,
                cx,
            ));
        let mut band = band
            .child(tiles)
            .child(muted(
                format!(
                    "{} · Pods read {} · rules (no acknowledge or dismiss; a row leaves only when a later read no longer matches a rule):",
                    data.scope.label(),
                    model::clock(observed_at)
                ),
                cx,
            ))
            .child(
                v_flex()
                    .debug_selector(|| "k8s-triage-rules".into())
                    .children(
                        RULES
                            .iter()
                            .enumerate()
                            .map(|(i, rule)| muted(format!("{}. {rule}", i + 1), cx)),
                    ),
            );
        if let Some(stale) = stale {
            band = band.child(div().text_xs().text_color(tokens.warning).child(stale));
        }
        if let Some(key) = &self.selected {
            if !self.rows.iter().any(|row| &row.key == key) {
                band = band.child(
                    div()
                        .debug_selector(|| "k8s-triage-selection-note".into())
                        .text_xs()
                        .text_color(tokens.warning)
                        .child(
                            "The selected object matches no rule in the latest read. It is not marked resolved; the inspector still shows it.",
                        ),
                );
            }
        }
        band.into_any_element()
    }

    fn node(
        &self,
        id: impl Into<ElementId>,
        selector: String,
        heading: &'static str,
        title: String,
        lines: Vec<(String, Tone)>,
        key: Option<Key>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tokens = brand::palette(cx);
        let active = key.is_some() && key.as_ref() == self.selected.as_ref();
        let body = v_flex()
            .id(id)
            .debug_selector(move || selector.clone())
            .min_w(px(180.))
            .gap(px(1.))
            .px_3()
            .py_2()
            .rounded(px(8.))
            .border_1()
            .border_color(if active { tokens.focus } else { tokens.border })
            .bg(tokens.surface_raised)
            .child(caps(heading, cx))
            .child(div().font_family(brand::MONO_FONT).text_sm().child(title))
            .children(lines.into_iter().map(|(text, tone)| {
                let color = match tone {
                    Tone::Neutral => tokens.text_muted,
                    tone => tone_color(tone, cx),
                };
                div().text_xs().text_color(color).child(text)
            }));
        match key {
            Some(key) => body
                .cursor_pointer()
                .hover(|s| s.bg(tokens.border_subtle.opacity(0.4)))
                .on_click(cx.listener(move |this, _, _, cx| this.select(key.clone(), cx)))
                .into_any_element(),
            None => body.into_any_element(),
        }
    }

    fn arrow(cx: &App) -> Div {
        div()
            .flex_none()
            .px_1()
            .pt_4()
            .text_color(brand::palette(cx).text_muted)
            .child("→")
    }

    pub(crate) fn render_path_band(&self, cx: &mut Context<Self>) -> AnyElement {
        let tokens = brand::palette(cx);
        let band = v_flex()
            .debug_selector(|| "k8s-path".into())
            .id("k8s-path")
            .flex_shrink(1.)
            .min_h(px(80.))
            .max_h(px(260.))
            .overflow_y_scroll()
            .gap_2()
            .px_4()
            .py_2()
            .border_b_1()
            .border_color(tokens.border)
            .bg(tokens.border_subtle.opacity(0.2));
        let Some(data) = &self.data else {
            return band
                .child(muted("Load a scope, then choose a Service.", cx))
                .into_any_element();
        };
        let Some(service) = self
            .path_service
            .as_ref()
            .and_then(|key| lenses::find_service(data, key))
        else {
            let text = if self.path_service.is_some() {
                "The Service this path showed is not in the latest read (deleted or replaced). Choose a Service."
            } else {
                "Choose a Service in the list to follow its routing."
            };
            return band
                .child(
                    div()
                        .debug_selector(|| "k8s-path-empty".into())
                        .child(muted(text, cx)),
                )
                .into_any_element();
        };
        let path = lenses::service_path(data, service);
        let selector = if service.selector.is_empty() {
            "no selector: endpoints are managed outside this Service".to_string()
        } else {
            format!(
                "selector {}",
                service
                    .selector
                    .iter()
                    .map(|(k, v)| format!("{k}={v}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        let mut service_lines = vec![(selector, Tone::Neutral)];
        service_lines.extend(path.ports.iter().map(|p| (p.clone(), Tone::Neutral)));
        let service_node = self.node(
            "k8s-path-service",
            "k8s-path-service".into(),
            "SERVICE",
            service.meta.name.clone(),
            service_lines,
            Some(Key::object(Kind::Service, &service.meta)),
            cx,
        );
        let mut row = h_flex()
            .gap_1()
            .items_start()
            .flex_wrap()
            .child(service_node);
        let mut notes: Vec<(String, Tone)> = Vec::new();
        match &path.routing {
            Routing::Unknown(why) => {
                row = row.child(Self::arrow(cx)).child(self.node(
                    "k8s-path-slices-unknown",
                    "k8s-path-slices-unknown".into(),
                    "ENDPOINTSLICES",
                    "unknown".into(),
                    vec![(
                        format!("{why}: endpoints are unknown, not zero"),
                        Tone::Warning,
                    )],
                    None,
                    cx,
                ));
            }
            Routing::Known {
                slices,
                partial,
                stale,
            } => {
                if let Some(stale) = stale {
                    notes.push((stale.clone(), Tone::Warning));
                }
                if *partial {
                    notes.push((
                        "The EndpointSlices read was a first page or had undecodable items; more endpoints may exist.".into(),
                        Tone::Warning,
                    ));
                }
                if slices.is_empty() {
                    row = row.child(Self::arrow(cx)).child(self.node(
                        "k8s-path-no-slices",
                        "k8s-path-no-slices".into(),
                        "ENDPOINTSLICES",
                        "none".into(),
                        vec![(
                            "No EndpointSlice names this Service: zero endpoints".into(),
                            if service.selector.is_empty() {
                                Tone::Neutral
                            } else {
                                Tone::Danger
                            },
                        )],
                        None,
                        cx,
                    ));
                }
                let mut slice_col = v_flex().gap_1();
                // Endpoints side by side, so the band stays short and every node
                // stays inside its visible area.
                let mut target_col = h_flex().gap_2().flex_wrap().items_start();
                for (si, s) in slices.iter().enumerate() {
                    let ready = s.endpoints.iter().filter(|e| e.ready == Some(true)).count();
                    let total = s.endpoints.len();
                    let tone = if total == 0 || ready == 0 {
                        Tone::Danger
                    } else if ready < total {
                        Tone::Warning
                    } else {
                        Tone::Good
                    };
                    let ports = s
                        .slice
                        .ports
                        .iter()
                        .map(|p| {
                            format!(
                                "{}{}/{}",
                                p.name.as_ref().map(|n| format!("{n} ")).unwrap_or_default(),
                                p.port.map_or_else(|| "?".into(), |p| p.to_string()),
                                p.protocol.as_deref().unwrap_or("TCP")
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(", ");
                    slice_col = slice_col.child(self.node(
                        ("k8s-path-slice", si),
                        format!("k8s-path-slice-{si}"),
                        "ENDPOINTSLICE",
                        s.slice.meta.name.clone(),
                        vec![
                            (format!("{ready} / {total} endpoints ready"), tone),
                            (
                                format!(
                                        "{} · ports {}",
                                        s.slice
                                            .address_type
                                            .as_deref()
                                            .unwrap_or("address type unknown"),
                                        if ports.is_empty() {
                                            "none".into()
                                        } else {
                                            ports
                                        }
                                    ),
                                Tone::Neutral,
                            ),
                        ],
                        None,
                        cx,
                    ));
                    for (ei, e) in s.endpoints.iter().enumerate() {
                        let conditions = format!(
                            "{} · ready {} · serving {} · terminating {}",
                            e.addresses.join(", "),
                            yes_no(e.ready),
                            yes_no(e.serving),
                            yes_no(e.terminating)
                        );
                        let ctone = if e.ready == Some(true) {
                            Tone::Good
                        } else {
                            Tone::Warning
                        };
                        let id = format!("k8s-path-endpoint-{si}-{ei}");
                        let node = match &e.target {
                            Target::Pod(pod) => {
                                let row_info = model::pod_row(pod);
                                let mut lines = vec![
                                    (conditions, ctone),
                                    (
                                        format!(
                                            "Pod {} · Ready {} · restarts {}",
                                            row_info.status,
                                            row_info.ready.clone().unwrap_or_default(),
                                            row_info.restarts.map_or_else(
                                                || "unknown".into(),
                                                |n| n.to_string()
                                            )
                                        ),
                                        row_info.tone,
                                    ),
                                ];
                                if e.not_selected {
                                    lines.push((
                                        "No longer matches the Service selector: membership is changing".into(),
                                        Tone::Warning,
                                    ));
                                }
                                lines.push((self.ownership_line(pod), Tone::Neutral));
                                let mut col = v_flex().gap(px(2.)).child(self.node(
                                    SharedString::from(id.clone()),
                                    id.clone(),
                                    "TARGET POD",
                                    pod.meta.name.clone(),
                                    lines,
                                    Some(Key::object(Kind::Pod, &pod.meta)),
                                    cx,
                                ));
                                let containers = h_flex().gap_1().flex_wrap().pl_3().children(
                                    pod.containers
                                        .iter()
                                        .filter(|c| {
                                            c.kind
                                                != okilum_core::kubernetes::ContainerKind::Ephemeral
                                        })
                                        .map(|c| {
                                            let crow = model::container_row(pod, c);
                                            let mut lines = vec![(crow.status.clone(), crow.tone)];
                                            if let Some(probe) = &c.probes.readiness {
                                                lines.push((
                                                    format!(
                                                        "readiness {}",
                                                        inspector::probe_text(probe)
                                                    ),
                                                    Tone::Neutral,
                                                ));
                                            }
                                            let cid = format!("{id}-{}", c.name);
                                            self.node(
                                                SharedString::from(cid.clone()),
                                                cid,
                                                "CONTAINER",
                                                c.name.clone(),
                                                lines,
                                                Some(crow.key.clone()),
                                                cx,
                                            )
                                        }),
                                );
                                col = col.child(containers);
                                col.into_any_element()
                            }
                            Target::MissingPod {
                                name,
                                uid,
                                pods_complete,
                            } => self.node(
                                SharedString::from(id.clone()),
                                id.clone(),
                                "TARGET POD",
                                name.clone(),
                                vec![
                                    (conditions, ctone),
                                    (
                                        format!(
                                            "UID {} is not in the latest Pods read: {}",
                                            uid.as_deref()
                                                .map(model::short_uid)
                                                .unwrap_or("unknown"),
                                            if *pods_complete {
                                                "deleted or replaced"
                                            } else {
                                                "the Pods read was partial"
                                            }
                                        ),
                                        Tone::Warning,
                                    ),
                                ],
                                None,
                                cx,
                            ),
                            Target::Other { kind, name } => self.node(
                                SharedString::from(id.clone()),
                                id.clone(),
                                "TARGET",
                                format!("{kind}/{name}"),
                                vec![(conditions, ctone)],
                                None,
                                cx,
                            ),
                            Target::None => self.node(
                                SharedString::from(id.clone()),
                                id.clone(),
                                "ENDPOINT",
                                e.addresses.join(", "),
                                vec![
                                    (conditions, ctone),
                                    ("No target reference".into(), Tone::Neutral),
                                ],
                                None,
                                cx,
                            ),
                        };
                        target_col = target_col.child(node);
                    }
                    if s.endpoints.is_empty() {
                        target_col = target_col.child(muted(
                            format!("{} has no endpoints.", s.slice.meta.name),
                            cx,
                        ));
                    }
                }
                if !slices.is_empty() {
                    row = row
                        .child(Self::arrow(cx))
                        .child(slice_col)
                        .child(Self::arrow(cx))
                        .child(target_col);
                }
            }
        }
        if !path.selected_not_endpoints.is_empty() {
            notes.push((
                format!(
                    "Selected by labels but not an endpoint (membership not yet reflected or not admitted): {}",
                    path.selected_not_endpoints
                        .iter()
                        .map(|p| p.meta.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                Tone::Warning,
            ));
        }
        band.child(row)
            .child(muted(
                "Routing: Service → EndpointSlice → target Pods (targetRef UID). Ownership is a separate relation: Deployment → ReplicaSet → Pod (ownerReferences UID).",
                cx,
            ))
            .children(notes.into_iter().map(|(text, tone)| {
                div()
                    .debug_selector(|| "k8s-path-note".into())
                    .text_xs()
                    .text_color(match tone {
                        Tone::Neutral => tokens.text_muted,
                        tone => tone_color(tone, cx),
                    })
                    .child(text)
            }))
            .into_any_element()
    }

    /// Ownership and autoscaling of a target Pod, labelled apart from routing.
    fn ownership_line(&self, pod: &okilum_core::kubernetes::Pod) -> String {
        let Some(data) = &self.data else {
            return String::new();
        };
        let chain = model::owner_chain(data, &pod.meta);
        let owners = if chain.is_empty() {
            "no owner references".to_string()
        } else {
            let mut parts: Vec<String> = chain
                .iter()
                .rev()
                .map(|l| {
                    if l.key.is_some() {
                        format!("{} {}", l.kind_name, l.name)
                    } else {
                        format!("{} {} (UID not observed)", l.kind_name, l.name)
                    }
                })
                .collect();
            parts.push("Pod".into());
            parts.join(" → ")
        };
        let hpa = match lenses::owning_deployment(data, pod) {
            None => String::new(),
            Some(d) => match (model::latest(&data.hpas), model::hpa_for(data, d)) {
                (None, _) => " · HPA unknown (not readable)".into(),
                (Some(_), None) => " · no HPA".into(),
                (Some(_), Some(h)) => format!(
                    " · HPA {}–{}, desired {}",
                    h.min_replicas.map_or_else(|| "?".into(), |v| v.to_string()),
                    h.max_replicas.map_or_else(|| "?".into(), |v| v.to_string()),
                    h.desired_replicas
                        .map_or_else(|| "unknown".into(), |v| v.to_string())
                ),
            },
        };
        format!("Ownership: {owners}{hpa}")
    }
}

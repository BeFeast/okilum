//! The selected-object inspector: Overview, Events and a read-only Manifest.
//! Relations are labelled by kind: ownership (ownerReferences with a matching
//! UID), label selection and EndpointSlice membership are never merged.
use super::model::{self, Endpoints, Key, LogTarget, Resolved};
use super::*;
use okilum_core::kubernetes::{
    Container, Deployment, Pod, Probe, ProbeHandler, ReplicaSet, Service, State,
};

pub(super) fn probe_text(probe: &Probe) -> String {
    let handler = match &probe.handler {
        ProbeHandler::Http { path, port, scheme } => format!(
            "{} GET {}:{port}",
            scheme.as_deref().unwrap_or("HTTP"),
            path.as_deref().unwrap_or("/")
        ),
        ProbeHandler::Tcp { port } => format!("TCP :{port}"),
        ProbeHandler::Grpc { port, service } => match service {
            Some(service) => format!("gRPC :{port} {service}"),
            None => format!("gRPC :{port}"),
        },
        ProbeHandler::Exec { arguments } => {
            format!("exec ({arguments} arguments, command not retained)")
        }
        ProbeHandler::Unknown => "unknown handler".into(),
    };
    let mut timing = Vec::new();
    if let Some(v) = probe.initial_delay_seconds {
        timing.push(format!("delay {v}s"));
    }
    if let Some(v) = probe.period_seconds {
        timing.push(format!("every {v}s"));
    }
    if let Some(v) = probe.timeout_seconds {
        timing.push(format!("timeout {v}s"));
    }
    if let Some(v) = probe.failure_threshold {
        timing.push(format!("failure ×{v}"));
    }
    if timing.is_empty() {
        handler
    } else {
        format!("{handler} · {}", timing.join(", "))
    }
}

fn state_detail(state: &State) -> String {
    match state {
        State::Waiting { reason, message } => match (reason, message) {
            (Some(r), Some(m)) => format!("Waiting: {r} — {m}"),
            (Some(r), None) => format!("Waiting: {r}"),
            (None, Some(m)) => format!("Waiting — {m}"),
            (None, None) => "Waiting".into(),
        },
        State::Running { started_at } => match started_at {
            Some(at) => format!("Running since {at}"),
            None => "Running".into(),
        },
        State::Terminated {
            started_at,
            finished_at,
            message,
            ..
        } => {
            let mut text = model::state_text(state);
            if let Some(at) = started_at {
                text.push_str(&format!(" · started {at}"));
            }
            if let Some(at) = finished_at {
                text.push_str(&format!(" · finished {at}"));
            }
            if let Some(message) = message {
                text.push_str(&format!(" — {message}"));
            }
            text
        }
    }
}

fn caps(text: impl Into<SharedString>, cx: &App) -> Div {
    div()
        .mt_3()
        .mb_1()
        .text_xs()
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(brand::palette(cx).text_muted)
        .child(text.into())
}

fn fact(label: impl Into<SharedString>, value: impl Into<SharedString>, cx: &App) -> Div {
    h_flex()
        .gap_3()
        .items_start()
        .text_sm()
        .child(
            div()
                .flex_none()
                .w(px(150.))
                .text_color(brand::palette(cx).text_muted)
                .child(label.into()),
        )
        .child(
            div()
                .min_w_0()
                .font_family(brand::MONO_FONT)
                .text_xs()
                .pt(px(2.))
                .child(value.into()),
        )
}

fn muted(text: impl Into<SharedString>, cx: &App) -> Div {
    div()
        .text_xs()
        .text_color(brand::palette(cx).text_muted)
        .child(text.into())
}

fn opt<T: std::fmt::Display>(value: Option<T>) -> String {
    value.map_or_else(|| "unknown".into(), |v| v.to_string())
}

impl Workspace {
    fn link(
        &self,
        id: impl Into<ElementId>,
        label: String,
        key: Key,
        cx: &mut Context<Self>,
    ) -> Button {
        Button::new(id)
            .small()
            .ghost()
            .label(label)
            .on_click(cx.listener(move |this, _, _, cx| this.select(key.clone(), cx)))
    }

    fn log_buttons(&self, pod: &Pod, c: &Container, cx: &mut Context<Self>) -> Div {
        let Some(data) = &self.data else {
            return div();
        };
        let current = LogTarget::new(&data.scope, pod, &c.name, false);
        let previous = LogTarget::new(&data.scope, pod, &c.name, true);
        let id = format!("{}-{}", pod.meta.uid, c.name);
        let previous_tip = match &previous {
            Ok(_) => "Logs of the last terminated instance (kubectl logs --previous)".to_string(),
            Err(error) => error.clone(),
        };
        h_flex()
            .gap_1()
            .child({
                let ok = current.ok();
                Button::new(SharedString::from(format!("k8s-logs-{id}")))
                    .debug_selector({
                        let name = c.name.clone();
                        move || format!("k8s-open-logs-{name}")
                    })
                    .small()
                    .primary()
                    .label("Open logs")
                    .disabled(ok.is_none())
                    .tooltip("Pin this container's current logs to the dock")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(target) = ok.clone() {
                            this.request_logs(target, cx);
                        }
                    }))
            })
            .child({
                let ok = previous.ok();
                Button::new(SharedString::from(format!("k8s-prev-{id}")))
                    .debug_selector({
                        let name = c.name.clone();
                        move || format!("k8s-open-previous-{name}")
                    })
                    .small()
                    .label("Previous logs")
                    .disabled(ok.is_none())
                    .tooltip(previous_tip)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(target) = ok.clone() {
                            this.request_logs(target, cx);
                        }
                    }))
            })
    }

    fn container_card(&self, pod: &Pod, c: &Container, cx: &mut Context<Self>) -> Div {
        let tokens = brand::palette(cx);
        let row = model::container_row(pod, c);
        let key = row.key.clone();
        let mut facts = vec![format!("Restarts {}", opt(c.restart_count))];
        if let Some(ready) = &row.ready {
            facts.insert(0, ready.clone());
        }
        v_flex()
            .gap_1()
            .p_2()
            .rounded(px(8.))
            .border_1()
            .border_color(tokens.border)
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(tone_dot(row.tone, cx).mt_0())
                    .child(
                        div()
                            .font_family(brand::MONO_FONT)
                            .text_sm()
                            .child(c.name.clone()),
                    )
                    .child(muted(model::kind_label(c.kind), cx))
                    .child(
                        div()
                            .text_xs()
                            .text_color(tone_color(row.tone, cx))
                            .child(row.status.clone()),
                    )
                    .child(muted(facts.join(" · "), cx))
                    .child(div().flex_1())
                    .child(self.link(
                        SharedString::from(format!("k8s-inspect-{}-{}", pod.meta.uid, c.name)),
                        "Details".into(),
                        key,
                        cx,
                    )),
            )
            .children(c.last_state.as_ref().map(|state| {
                muted(
                    format!(
                        "Last state (container status, not a Kubernetes Event): {}",
                        state_detail(state)
                    ),
                    cx,
                )
            }))
            .children(
                c.image
                    .clone()
                    .or_else(|| c.status_image.clone())
                    .map(|image| muted(format!("Image {image}"), cx)),
            )
            .child(self.log_buttons(pod, c, cx))
    }

    fn relations(&self, pod: &Pod, cx: &mut Context<Self>) -> Div {
        let Some(data) = &self.data else {
            return div();
        };
        let mut col = v_flex().gap_1();
        col = col.child(caps("OWNED BY (ownerReferences, matching UID)", cx));
        let chain = model::owner_chain(data, &pod.meta);
        if chain.is_empty() {
            col = col.child(muted("No owner references.", cx));
        }
        for (ix, link) in chain.into_iter().enumerate() {
            col = col.child(match link.key {
                Some(key) => h_flex()
                    .gap_2()
                    .items_center()
                    .child(muted(link.kind_name.clone(), cx))
                    .child(self.link(("k8s-owner", ix), link.name.clone(), key, cx)),
                None => h_flex()
                    .gap_2()
                    .child(muted(format!("{} {}", link.kind_name, link.name), cx))
                    .children(link.note.map(|note| muted(note, cx))),
            });
        }
        col = col.child(caps(
            "SELECTED BY SERVICES (label selector, not ownership)",
            cx,
        ));
        let services = model::services_selecting(data, pod);
        if model::latest(&data.services).is_none() {
            col = col.child(muted("Services not read.", cx));
        } else if services.is_empty() {
            col = col.child(muted("No Service selector matches this Pod's labels.", cx));
        }
        for (ix, service) in services.into_iter().enumerate() {
            col = col.child(self.link(
                ("k8s-selected-by", ix),
                service.meta.name.clone(),
                Key::object(okilum_core::kubernetes::Kind::Service, &service.meta),
                cx,
            ));
        }
        col = col.child(caps("ENDPOINTSLICE MEMBERSHIP (targetRef UID)", cx));
        if model::latest(&data.slices).is_none() {
            col = col.child(muted(
                format!(
                    "Unknown: EndpointSlices {}.",
                    model::freshness(&data.slices, std::time::SystemTime::now()).0
                ),
                cx,
            ));
        } else {
            let members = model::memberships(data, &pod.meta.uid);
            if members.is_empty() {
                col = col.child(muted("Not an endpoint of any observed EndpointSlice.", cx));
            }
            for m in members {
                col = col.child(muted(
                    format!(
                        "{} (Service {}) · ready {} · serving {} · terminating {}",
                        m.slice,
                        m.service.as_deref().unwrap_or("unknown"),
                        opt(m.ready),
                        opt(m.serving),
                        opt(m.terminating)
                    ),
                    cx,
                ));
            }
        }
        col
    }

    fn pod_overview(&self, pod: &Pod, cx: &mut Context<Self>) -> Div {
        let row = model::pod_row(pod);
        let mut col = v_flex()
            .gap_1()
            .child(muted("POD", cx))
            .child(
                div()
                    .debug_selector(|| "k8s-inspector-title".into())
                    .font_family(brand::MONO_FONT)
                    .text_lg()
                    .child(pod.meta.name.clone()),
            )
            .child(
                h_flex()
                    .gap_3()
                    .flex_wrap()
                    .text_sm()
                    .child(
                        div()
                            .text_color(tone_color(row.tone, cx))
                            .child(row.status.clone()),
                    )
                    .child(format!("Ready {}", row.ready.clone().unwrap_or_default()))
                    .child(format!("Restarts {}", opt(row.restarts))),
            )
            .when(!row.reasons.is_empty(), |col| {
                col.child(muted(format!("Reasons: {}", row.reasons.join(" · ")), cx))
            })
            .child(fact("UID", pod.meta.uid.clone(), cx))
            .child(fact("Node", opt(pod.node.as_ref()), cx))
            .child(fact("Pod IP", opt(pod.pod_ip.as_ref()), cx))
            .child(fact("QoS class", opt(pod.qos_class.as_ref()), cx))
            .child(fact("Created", opt(pod.meta.created.as_ref()), cx))
            .child(caps("CONTAINERS", cx));
        for c in &pod.containers {
            col = col.child(self.container_card(pod, c, cx));
        }
        if !pod.conditions.is_empty() {
            col = col.child(caps("CONDITIONS", cx));
            for c in &pod.conditions {
                col = col.child(fact(
                    c.kind.clone(),
                    match &c.reason {
                        Some(reason) => format!("{} ({reason})", c.status),
                        None => c.status.clone(),
                    },
                    cx,
                ));
            }
        }
        col.child(self.relations(pod, cx))
    }

    fn container_overview(&self, pod: &Pod, c: &Container, cx: &mut Context<Self>) -> Div {
        let row = model::container_row(pod, c);
        let mut col = v_flex()
            .gap_1()
            .child(muted(
                format!("CONTAINER · {}", model::kind_label(c.kind).to_uppercase()),
                cx,
            ))
            .child(
                div()
                    .debug_selector(|| "k8s-inspector-title".into())
                    .font_family(brand::MONO_FONT)
                    .text_lg()
                    .child(c.name.clone()),
            )
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(muted("In Pod", cx))
                    .child(self.link(
                        "k8s-container-pod",
                        pod.meta.name.clone(),
                        Key::object(okilum_core::kubernetes::Kind::Pod, &pod.meta),
                        cx,
                    ))
                    .child(muted(
                        format!(
                            "UID {} (containers have no UID of their own)",
                            model::short_uid(&pod.meta.uid)
                        ),
                        cx,
                    )),
            )
            .child(
                h_flex()
                    .gap_3()
                    .text_sm()
                    .child(
                        div()
                            .text_color(tone_color(row.tone, cx))
                            .child(row.status.clone()),
                    )
                    .child(row.ready.clone().unwrap_or_else(|| "readiness not reported".into()))
                    .child(format!("Restarts {}", opt(c.restart_count))),
            )
            .child(self.log_buttons(pod, c, cx))
            .child(caps("STATE", cx))
            .child(fact(
                "Current",
                c.state
                    .as_ref()
                    .map_or_else(|| "not reported".into(), state_detail),
                cx,
            ))
            .child(fact(
                "Last state",
                c.last_state
                    .as_ref()
                    .map_or_else(|| "none observed".into(), state_detail),
                cx,
            ))
            .child(muted(
                "Last state is this container's status from the kubelet; it is not a Kubernetes Event.",
                cx,
            ))
            .child(fact("Started", opt(c.started), cx))
            .child(caps("IMAGE", cx))
            .child(fact("Spec image", opt(c.image.as_ref()), cx))
            .child(fact("Running image", opt(c.status_image.as_ref()), cx))
            .child(fact("Image ID", opt(c.image_id.as_ref()), cx))
            .child(caps("RESOURCES", cx));
        if c.resources.requests.is_empty() && c.resources.limits.is_empty() {
            col = col.child(muted("No requests or limits.", cx));
        }
        for (name, value) in &c.resources.requests {
            col = col.child(fact(format!("Request {name}"), value.clone(), cx));
        }
        for (name, value) in &c.resources.limits {
            col = col.child(fact(format!("Limit {name}"), value.clone(), cx));
        }
        col = col.child(caps("PROBES", cx));
        let probes = [
            ("Startup", &c.probes.startup),
            ("Liveness", &c.probes.liveness),
            ("Readiness", &c.probes.readiness),
        ];
        if probes.iter().all(|(_, p)| p.is_none()) {
            col = col.child(muted("No probes.", cx));
        }
        for (name, probe) in probes
            .iter()
            .filter_map(|(name, probe)| probe.as_ref().map(|p| (*name, p)))
        {
            col = col.child(fact(name, probe_text(probe), cx));
        }
        col
    }

    fn deployment_overview(&self, d: &Deployment, cx: &mut Context<Self>) -> Div {
        let Some(data) = &self.data else {
            return div();
        };
        let row = model::deployment_row(d);
        let mut col = v_flex()
            .gap_1()
            .child(muted("DEPLOYMENT", cx))
            .child(
                div()
                    .debug_selector(|| "k8s-inspector-title".into())
                    .font_family(brand::MONO_FONT)
                    .text_lg()
                    .child(d.meta.name.clone()),
            )
            .child(
                h_flex()
                    .gap_3()
                    .text_sm()
                    .child(
                        div()
                            .text_color(tone_color(row.tone, cx))
                            .child(format!("Ready {}", opt(row.ready.as_ref()))),
                    )
                    .child(format!("Updated {}", opt(d.updated)))
                    .child(format!("Available {}", opt(d.available)))
                    .child(format!("Unavailable {}", d.unavailable.unwrap_or(0))),
            )
            .child(fact("UID", d.meta.uid.clone(), cx))
            .child(fact("Desired replicas", opt(d.replicas), cx))
            .child(fact(
                "Observed generation",
                format!(
                    "{} of {}",
                    opt(d.observed_generation),
                    opt(d.meta.generation)
                ),
                cx,
            ));
        if !d.conditions.is_empty() {
            col = col.child(caps("CONDITIONS", cx));
            for c in &d.conditions {
                col = col.child(fact(
                    c.kind.clone(),
                    match (&c.reason, &c.message) {
                        (Some(r), Some(m)) => format!("{} ({r}) — {m}", c.status),
                        (Some(r), None) => format!("{} ({r})", c.status),
                        _ => c.status.clone(),
                    },
                    cx,
                ));
            }
        }
        col = col.child(caps("AUTOSCALING", cx));
        match (model::latest(&data.hpas), model::hpa_for(data, d)) {
            (None, _) => {
                col = col.child(muted(
                    format!(
                        "HPA facts unknown: {}.",
                        model::freshness(&data.hpas, std::time::SystemTime::now()).0
                    ),
                    cx,
                ))
            }
            (Some(_), None) => {
                col = col.child(muted(
                    "No HorizontalPodAutoscaler targets this Deployment.",
                    cx,
                ))
            }
            (Some(_), Some(hpa)) => {
                col = col
                    .child(fact("HPA", hpa.meta.name.clone(), cx))
                    .child(fact(
                        "Replicas",
                        format!(
                            "min {} · max {} · current {} · desired {}",
                            opt(hpa.min_replicas),
                            opt(hpa.max_replicas),
                            opt(hpa.current_replicas),
                            opt(hpa.desired_replicas)
                        ),
                        cx,
                    ))
                    .child(fact("Last scale", opt(hpa.last_scale.as_ref()), cx));
                if hpa.metrics.is_empty() {
                    col = col.child(muted(
                        "Current metrics not reported (unknown, not zero).",
                        cx,
                    ));
                }
                for m in &hpa.metrics {
                    col = col.child(fact(
                        format!("{} {}", m.kind, m.name.clone().unwrap_or_default()),
                        m.current.clone().unwrap_or_else(|| "unknown".into()),
                        cx,
                    ));
                }
            }
        }
        col = col.child(caps("REPLICASETS (owned, matching UID)", cx));
        let sets = model::replica_sets_of(data, d);
        if sets.is_empty() {
            col = col.child(muted(
                "No observed ReplicaSet is owned by this Deployment.",
                cx,
            ));
        }
        for (ix, rs) in sets.into_iter().enumerate() {
            col = col.child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(self.link(
                        ("k8s-child-rs", ix),
                        rs.meta.name.clone(),
                        Key::object(okilum_core::kubernetes::Kind::ReplicaSet, &rs.meta),
                        cx,
                    ))
                    .child(muted(
                        format!("ready {} of {}", rs.ready.unwrap_or(0), opt(rs.replicas)),
                        cx,
                    )),
            );
        }
        col = col.child(caps("PODS (owned through ReplicaSets)", cx));
        let pods = model::pods_of_deployment(data, d);
        if pods.is_empty() {
            col = col.child(muted(
                "No observed Pod is owned through these ReplicaSets.",
                cx,
            ));
        }
        for (ix, pod) in pods.into_iter().enumerate() {
            let row = model::pod_row(pod);
            col = col.child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(tone_dot(row.tone, cx).mt_0())
                    .child(self.link(
                        ("k8s-child-pod", ix),
                        pod.meta.name.clone(),
                        row.key.clone(),
                        cx,
                    ))
                    .child(muted(
                        format!(
                            "{} · Ready {} · Restarts {}",
                            row.status,
                            row.ready.unwrap_or_default(),
                            opt(row.restarts)
                        ),
                        cx,
                    )),
            );
        }
        col
    }

    fn replica_set_overview(&self, rs: &ReplicaSet, cx: &mut Context<Self>) -> Div {
        let Some(data) = &self.data else {
            return div();
        };
        let mut col = v_flex()
            .gap_1()
            .child(muted("REPLICASET", cx))
            .child(
                div()
                    .debug_selector(|| "k8s-inspector-title".into())
                    .font_family(brand::MONO_FONT)
                    .text_lg()
                    .child(rs.meta.name.clone()),
            )
            .child(fact("UID", rs.meta.uid.clone(), cx))
            .child(fact(
                "Replicas",
                format!(
                    "desired {} · ready {} · available {}",
                    opt(rs.replicas),
                    rs.ready.unwrap_or(0),
                    rs.available.unwrap_or(0)
                ),
                cx,
            ))
            .child(fact("Template hash", opt(rs.template_hash.as_ref()), cx))
            .child(caps("OWNED BY", cx));
        for (ix, link) in model::owners(data, &rs.meta).into_iter().enumerate() {
            col = col.child(match link.key {
                Some(key) => self
                    .link(("k8s-rs-owner", ix), link.name.clone(), key, cx)
                    .into_any_element(),
                None => muted(
                    format!(
                        "{} {} — {}",
                        link.kind_name,
                        link.name,
                        link.note.unwrap_or_default()
                    ),
                    cx,
                )
                .into_any_element(),
            });
        }
        col = col.child(caps("PODS (owned, matching UID)", cx));
        for (ix, pod) in model::pods_owned_by(data, &rs.meta.uid)
            .into_iter()
            .enumerate()
        {
            col = col.child(self.link(
                ("k8s-rs-pod", ix),
                pod.meta.name.clone(),
                Key::object(okilum_core::kubernetes::Kind::Pod, &pod.meta),
                cx,
            ));
        }
        col
    }

    fn service_overview(&self, s: &Service, cx: &mut Context<Self>) -> Div {
        let Some(data) = &self.data else {
            return div();
        };
        let row = model::service_row(data, s);
        let mut col = v_flex()
            .gap_1()
            .child(muted("SERVICE", cx))
            .child(
                div()
                    .debug_selector(|| "k8s-inspector-title".into())
                    .font_family(brand::MONO_FONT)
                    .text_lg()
                    .child(s.meta.name.clone()),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(tone_color(row.tone, cx))
                    .child(row.status.clone()),
            )
            .child(fact("UID", s.meta.uid.clone(), cx))
            .child(fact("Type", opt(s.kind.as_ref()), cx))
            .child(fact("Cluster IP", opt(s.cluster_ip.as_ref()), cx));
        for p in &s.ports {
            col = col.child(fact(
                format!("Port {}", p.name.clone().unwrap_or_default()),
                format!(
                    "{}/{} → {}",
                    p.port,
                    p.protocol.clone().unwrap_or_else(|| "TCP".into()),
                    opt(p.target_port.as_ref())
                ),
                cx,
            ));
        }
        col = col.child(caps("SELECTS (label selector, not ownership)", cx));
        if s.selector.is_empty() {
            col = col.child(muted(
                "No selector: endpoints are managed outside this Service.",
                cx,
            ));
        } else {
            col = col.child(fact(
                "Selector",
                s.selector
                    .iter()
                    .map(|(k, v)| format!("{k}={v}"))
                    .collect::<Vec<_>>()
                    .join(", "),
                cx,
            ));
            for (ix, pod) in model::pods_selected_by(data, s).into_iter().enumerate() {
                let row = model::pod_row(pod);
                col = col.child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(tone_dot(row.tone, cx).mt_0())
                        .child(self.link(
                            ("k8s-svc-pod", ix),
                            pod.meta.name.clone(),
                            row.key.clone(),
                            cx,
                        ))
                        .child(muted(
                            format!("Ready {}", row.ready.unwrap_or_default()),
                            cx,
                        )),
                );
            }
        }
        col = col.child(caps("ENDPOINTSLICE MEMBERSHIP", cx));
        match model::service_endpoints(data, s) {
            Endpoints::Unknown(why) => {
                col = col.child(muted(
                    format!("Unknown ({why}); this does not mean zero endpoints."),
                    cx,
                ));
            }
            Endpoints::Known { .. } => {
                let slices = model::latest(&data.slices)
                    .map(|o| o.value.items.as_slice())
                    .unwrap_or_default();
                let mut any = false;
                for slice in slices
                    .iter()
                    .filter(|sl| sl.service.as_deref() == Some(s.meta.name.as_str()))
                {
                    for (ix, e) in slice.endpoints.iter().enumerate() {
                        any = true;
                        let state = format!(
                            "{} · ready {} · serving {} · terminating {}",
                            e.addresses.join(", "),
                            opt(e.ready),
                            opt(e.serving),
                            opt(e.terminating)
                        );
                        let pod = e.target.as_ref().and_then(|t| {
                            let uid = t.uid.as_deref()?;
                            data.pods().iter().find(|p| p.meta.uid == uid)
                        });
                        col = col.child(
                            h_flex()
                                .gap_2()
                                .items_center()
                                .child(muted(slice.meta.name.clone(), cx))
                                .children(pod.map(|pod| {
                                    self.link(
                                        SharedString::from(format!(
                                            "k8s-endpoint-{}-{ix}",
                                            slice.meta.uid
                                        )),
                                        pod.meta.name.clone(),
                                        Key::object(okilum_core::kubernetes::Kind::Pod, &pod.meta),
                                        cx,
                                    )
                                }))
                                .child(muted(state, cx)),
                        );
                    }
                }
                if !any {
                    col = col.child(muted("No endpoints in the observed EndpointSlices.", cx));
                }
            }
        }
        col
    }

    fn overview(&self, resolved: &Resolved, cx: &mut Context<Self>) -> Div {
        match resolved {
            Resolved::Pod(pod) => self.pod_overview(pod, cx),
            Resolved::Container(pod, c) => self.container_overview(pod, c, cx),
            Resolved::Deployment(d) => self.deployment_overview(d, cx),
            Resolved::ReplicaSet(rs) => self.replica_set_overview(rs, cx),
            Resolved::Service(s) => self.service_overview(s, cx),
            Resolved::Gone { replacement } => {
                let uid = self
                    .selected
                    .as_ref()
                    .map(|k| k.uid().to_string())
                    .unwrap_or_default();
                v_flex()
                    .gap_2()
                    .child(muted(
                        format!(
                            "The selected object (UID {}) is not in the latest read.",
                            model::short_uid(&uid)
                        ),
                        cx,
                    ))
                    .children(replacement.clone().map(|key| {
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(muted(
                                format!(
                                    "An object with the same name now has UID {}: a replacement, not the same object.",
                                    model::short_uid(key.uid())
                                ),
                                cx,
                            ))
                            .child(self.link("k8s-gone-replacement", "Select it".into(), key, cx))
                    }))
            }
        }
    }

    /// UIDs whose events belong to the selection: a workload includes its
    /// ReplicaSets and Pods, each group naming its involved object.
    pub(super) fn event_uids(&self, resolved: &Resolved) -> Vec<String> {
        let Some(data) = &self.data else {
            return Vec::new();
        };
        match resolved {
            Resolved::Pod(pod) | Resolved::Container(pod, _) => vec![pod.meta.uid.clone()],
            Resolved::Deployment(d) => {
                let mut uids = vec![d.meta.uid.clone()];
                uids.extend(
                    model::replica_sets_of(data, d)
                        .iter()
                        .map(|r| r.meta.uid.clone()),
                );
                uids.extend(
                    model::pods_of_deployment(data, d)
                        .iter()
                        .map(|p| p.meta.uid.clone()),
                );
                uids
            }
            Resolved::ReplicaSet(rs) => {
                let mut uids = vec![rs.meta.uid.clone()];
                uids.extend(
                    model::pods_owned_by(data, &rs.meta.uid)
                        .iter()
                        .map(|p| p.meta.uid.clone()),
                );
                uids
            }
            Resolved::Service(s) => vec![s.meta.uid.clone()],
            Resolved::Gone { .. } => Vec::new(),
        }
    }

    fn events(&self, resolved: &Resolved, cx: &mut Context<Self>) -> Div {
        let Some(data) = &self.data else {
            return div();
        };
        let tokens = brand::palette(cx);
        let uids = self.event_uids(resolved);
        let refs: Vec<&str> = uids.iter().map(String::as_str).collect();
        let (fresh, _) = model::freshness(&data.events, std::time::SystemTime::now());
        let mut col = v_flex()
            .gap_1()
            .child(muted(
                format!(
                    "Kubernetes Events by involved object (read {fresh}). Container lastState is in Overview, not here."
                ),
                cx,
            ));
        if model::latest(&data.events).is_none() {
            return col.child(muted(format!("Events unknown: {fresh}."), cx));
        }
        let groups = model::event_groups(data, &refs);
        if groups.is_empty() {
            return col.child(muted("No Events for these objects in the latest read.", cx));
        }
        for (ix, g) in groups.into_iter().enumerate() {
            let tone = if g.kind == "Warning" {
                Tone::Warning
            } else {
                Tone::Neutral
            };
            col = col.child(
                v_flex()
                    .debug_selector(move || format!("k8s-event-{ix}"))
                    .py_1()
                    .border_b_1()
                    .border_color(tokens.border_subtle)
                    .child(
                        h_flex()
                            .gap_3()
                            .text_sm()
                            .child(
                                div()
                                    .w(px(70.))
                                    .text_color(tone_color(tone, cx))
                                    .child(g.kind.clone()),
                            )
                            .child(
                                div()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(g.reason.clone()),
                            )
                            .child(
                                div()
                                    .font_family(brand::MONO_FONT)
                                    .text_xs()
                                    .child(g.involved.clone()),
                            )
                            .child(muted(format!("×{}", g.count), cx))
                            .child(div().flex_1())
                            .child(muted(
                                g.last.clone().unwrap_or_else(|| "time unknown".into()),
                                cx,
                            )),
                    )
                    .child(div().text_xs().child(g.message.clone())),
            );
        }
        col
    }

    pub(crate) fn render_inspector(&self, cx: &mut Context<Self>) -> AnyElement {
        let tokens = brand::palette(cx);
        let tabs = h_flex()
            .debug_selector(|| "k8s-inspector-tabs".into())
            .flex_none()
            .gap_1()
            .px_3()
            .py_1()
            .border_b_1()
            .border_color(tokens.border_subtle)
            .children([Tab::Overview, Tab::Events, Tab::Manifest].map(|tab| {
                let label = match tab {
                    Tab::Overview => "Overview",
                    Tab::Events => "Events",
                    Tab::Manifest => "Manifest",
                };
                Button::new(SharedString::from(format!("k8s-tab-{label}")))
                    .debug_selector(move || format!("k8s-tab-{label}"))
                    .small()
                    .ghost()
                    .label(label)
                    .map(|b| segment(b, self.tab == tab, cx))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.tab = tab;
                        cx.notify();
                    }))
            }));
        let body: AnyElement = match (&self.data, &self.selected) {
            (Some(data), Some(key)) => {
                let resolved = model::resolve(data, key);
                match self.tab {
                    Tab::Overview => self.overview(&resolved, cx).into_any_element(),
                    Tab::Events => self.events(&resolved, cx).into_any_element(),
                    Tab::Manifest => match model::manifest(&resolved) {
                        Some(text) => v_flex()
                            .gap_2()
                            .child(muted(model::MANIFEST_NOTE, cx))
                            .child(
                                div()
                                    .debug_selector(|| "k8s-manifest".into())
                                    .p_2()
                                    .rounded(px(6.))
                                    .bg(tokens.border_subtle.opacity(0.4))
                                    .font_family(brand::MONO_FONT)
                                    .text_xs()
                                                    .children(text.lines().map(|line| {
                                        div().child(SharedString::from(line.replace(' ', "\u{a0}")))
                                    })),
                            )
                            .into_any_element(),
                        None => muted("No manifest: the object is not in the latest read.", cx)
                            .into_any_element(),
                    },
                }
            }
            _ => muted(
                "Select a resource to inspect it. Selecting never reads the cluster or starts logs.",
                cx,
            )
            .into_any_element(),
        };
        v_flex()
            .debug_selector(|| "k8s-inspector".into())
            .flex_1()
            .min_w_0()
            .h_full()
            .child(tabs)
            .child(
                div()
                    .id("k8s-inspector-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p_4()
                    .child(body),
            )
            .into_any_element()
    }
}

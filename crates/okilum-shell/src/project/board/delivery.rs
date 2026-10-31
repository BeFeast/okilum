//! Compact delivery facts; existing Board navigation remains the only action route.
use super::*;
use okilum_t3::board::delivery as facts;

fn platform() -> facts::Platform {
    if cfg!(target_os = "macos") {
        facts::Platform::Macos
    } else if cfg!(target_os = "windows") {
        facts::Platform::Windows
    } else {
        facts::Platform::Linux
    }
}
impl Board {
    pub(super) fn refresh_delivery(
        &mut self,
        ticket: okilum_t3::board::Ticket,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.delivery_loading {
            return;
        }
        let Some(binding) = self.binding.clone() else {
            return;
        };
        self.delivery_loading = true;
        let area = self.area.clone();
        cx.spawn_in(window, async move |this, cx| {
            let update = cx
                .background_executor()
                .spawn(async move { facts::refresh(&area, &binding) })
                .await;
            let _ = this.update(cx, |this, cx| {
                if !this.active || !this.snapshot.matches(&ticket) {
                    return;
                }
                let old = this.delivery.problems(now(), binding_refresh(this));
                this.delivery.accept(update, now());
                this.delivery_loading = false;
                let current = this.delivery.problems(now(), binding_refresh(this));
                if current != old {
                    for problem in current {
                        eprintln!("Board delivery: {problem}");
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
    pub(super) fn delivery_column(&self, scope: &okilum_t3::board::Scope) -> Column {
        if self.delivery.enabled() {
            let rows = self
                .delivery
                .rows(scope, platform(), now(), binding_refresh(self));
            if rows.iter().any(|r| r.accepted) {
                return Column::Verified;
            }
            if rows.iter().any(|r| r.ready) {
                return Column::Ready;
            }
            // A legacy stage flag is not exact-build evidence. Preserve the
            // canonical value, but don't turn it into an unsupported delivery claim.
            let mut scope = scope.clone();
            scope.stage = None;
            return okilum_t3::board::column(
                &scope,
                self.snapshot.forgejo.value.get(&scope.number),
                &self.snapshot.t3.value,
            );
        }
        okilum_t3::board::column(
            scope,
            self.snapshot.forgejo.value.get(&scope.number),
            &self.snapshot.t3.value,
        )
    }
    pub(super) fn delivery_summary(&self, binding: &Binding, cx: &mut Context<Self>) -> AnyElement {
        let colors = brand::palette(cx);
        let mut view = v_flex()
            .id("board-delivery")
            .debug_selector(|| "board-delivery".into())
            .w_full()
            .min_w_0()
            .gap_1()
            .text_size(px(12.));
        for platform in facts::Platform::ALL {
            let mut line = h_flex().w_full().min_w_0().gap_2().flex_wrap().child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(platform.label()),
            );
            for channel in facts::Channel::ALL {
                let track = (platform, channel);
                let feed = self
                    .delivery
                    .config
                    .value
                    .as_ref()
                    .and_then(|c| c.feeds.iter().find(|f| (f.platform, f.channel) == track));
                let source = self.delivery.feeds.get(&track);
                let build = source.and_then(|s| s.value.as_ref());
                let text = match build {
                    Some(b) => format!(
                        "{} {} · {}",
                        channel.label(),
                        b.build,
                        &b.source[..b.source.len().min(8)]
                    ),
                    None => format!(
                        "{} · {}",
                        channel.label(),
                        if feed.is_some() {
                            "unknown"
                        } else {
                            "not bound"
                        }
                    ),
                };
                let id = format!("board-delivery-{}-{}", platform.label(), channel.label());
                let tooltip = match (build, feed) {
                    (Some(b), Some(f)) => format!(
                        "Source: {}\nFeed: {}\nArtifact: {}\n{}{}",
                        b.source,
                        f.url,
                        b.artifact,
                        f.next,
                        f.blocker
                            .as_ref()
                            .map_or(String::new(), |s| format!("\nBlocked: {s}"))
                    ),
                    _ => "No publication or installation is inferred".into(),
                };
                let url = build
                    .map(|b| b.artifact.clone())
                    .or_else(|| feed.map(|f| f.url.clone()));
                line = line.child(
                    Button::new(SharedString::from(id.clone()))
                        .ghost()
                        .small()
                        .debug_selector(move || id.clone())
                        .text_size(px(12.))
                        .label(text)
                        .tooltip(tooltip)
                        .disabled(url.is_none())
                        .on_click(move |_, _, cx| {
                            if let Some(url) = &url {
                                cx.open_url(url);
                            }
                        }),
                );
            }
            view = view.child(line);
        }
        if let Some(config) = &self.delivery.config.value {
            for feed in &config.feeds {
                if !feed.next.is_empty() || feed.blocker.is_some() {
                    view = view.child(div().text_color(colors.text_muted).child(format!(
                            "{} {} · {}{}",
                            feed.platform.label(),
                            feed.channel.label(),
                            feed.next,
                            feed.blocker
                                .as_ref()
                                .map_or(String::new(), |s| format!(" · Blocked: {s}"))
                        )));
                }
            }
        }
        for problem in self.delivery.problems(now(), binding.refresh_seconds) {
            view = view.child(
                div()
                    .text_color(colors.text_muted)
                    .whitespace_normal()
                    .child(problem),
            );
        }
        view.into_any_element()
    }
    pub(super) fn delivery_details(
        &self,
        scope: &okilum_t3::board::Scope,
        binding: &Binding,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = brand::palette(cx);
        let mut view = v_flex()
            .w_full()
            .min_w_0()
            .gap_1()
            .px_3()
            .pb_2()
            .text_size(px(12.));
        let mut count = 0;
        for pr in &scope.pull_requests {
            let ci = self.delivery.ci.get(pr);
            let label = ci.map_or("not read".into(), |s| {
                facts::problem(s, now(), binding.refresh_seconds).unwrap_or_else(|| {
                    format!(
                        "{} · source {}",
                        s.value.state,
                        &s.value.source[..s.value.source.len().min(8)]
                    )
                })
            });
            let mut line = h_flex()
                .w_full()
                .min_w_0()
                .gap_1()
                .flex_wrap()
                .child(format!("PR #{pr} CI: {label}"));
            if let Some(ci) = ci {
                let source = format!(
                    "{}/commit/{}",
                    binding.repository.trim_end_matches('/'),
                    ci.value.source
                );
                line = line.child(
                    Button::new(SharedString::from(format!(
                        "delivery-ci-source-{}-{pr}",
                        scope.number
                    )))
                    .ghost()
                    .small()
                    .label("Source / checks")
                    .on_click(move |_, _, cx| cx.open_url(&source)),
                );
                for (i, url) in ci.value.links.iter().take(2).enumerate() {
                    let url = url.clone();
                    line = line.child(
                        Button::new(SharedString::from(format!(
                            "delivery-ci-{}-{pr}-{i}",
                            scope.number
                        )))
                        .ghost()
                        .small()
                        .label("Actions job")
                        .on_click(move |_, _, cx| cx.open_url(&url)),
                    );
                }
            }
            view = view.child(line);
        }
        for platform in facts::Platform::ALL {
            for row in self
                .delivery
                .rows(scope, platform, now(), binding.refresh_seconds)
            {
                count += 1;
                let title = format!(
                    "{} {} · {} · {}",
                    platform.label(),
                    row.track.1.label(),
                    row.build.as_ref().map_or("unknown", |b| b.build.as_str()),
                    row.state
                );
                let mut entry = v_flex()
                    .w_full()
                    .min_w_0()
                    .child(div().whitespace_normal().child(title));
                let mut actions = h_flex().gap_1().flex_wrap();
                if let Some(build) = &row.build {
                    let url = build.artifact.clone();
                    actions = actions.child(
                        Button::new(SharedString::from(format!(
                            "delivery-artifact-{}-{}-{}",
                            scope.number,
                            platform.label(),
                            row.track.1.label()
                        )))
                        .ghost()
                        .small()
                        .label("Artifact")
                        .tooltip(url.clone())
                        .on_click(move |_, _, cx| cx.open_url(&url)),
                    );
                    let url = format!(
                        "{}/commit/{}",
                        binding.repository.trim_end_matches('/'),
                        build.source
                    );
                    actions = actions.child(
                        Button::new(SharedString::from(format!(
                            "delivery-source-{}-{}-{}",
                            scope.number,
                            platform.label(),
                            row.track.1.label()
                        )))
                        .ghost()
                        .small()
                        .label("Source")
                        .tooltip(build.source.clone())
                        .on_click(move |_, _, cx| cx.open_url(&url)),
                    );
                }
                if let Some(evidence) = row.evidence {
                    entry = entry.child(
                        div()
                            .text_color(colors.text_muted)
                            .whitespace_normal()
                            .child(evidence.scenario),
                    );
                    if let Some(at) = evidence.installed_at {
                        entry = entry.child(format!("Installed (explicit evidence): {at}"));
                    }
                    if let (Some(by), Some(at)) = (evidence.accepted_by, evidence.accepted_at) {
                        entry = entry.child(format!("Acceptance: {by} · {at}"));
                    }
                    let note = evidence.note;
                    actions = actions.child(
                        Button::new(SharedString::from(format!(
                            "delivery-evidence-{}-{}-{}",
                            scope.number,
                            platform.label(),
                            row.track.1.label()
                        )))
                        .ghost()
                        .small()
                        .label("Scenario / evidence")
                        .tooltip(note.clone())
                        .on_click(cx.listener(move |this, _, _, cx| this.note(&note, cx))),
                    );
                }
                view = view.child(entry.child(actions));
            }
        }
        if count == 0 {
            view = view.child(
                div()
                    .text_color(colors.text_muted)
                    .child("Delivery evidence is not configured for this issue"),
            );
        }
        view.into_any_element()
    }
}
fn binding_refresh(board: &Board) -> u64 {
    board.binding.as_ref().map_or(30, |b| b.refresh_seconds)
}

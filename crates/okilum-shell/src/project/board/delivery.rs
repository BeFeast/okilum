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
        if self.delivery_loading
            || !self.active
            || !reader_features::enabled(reader_features::Feature::Agents, cx)
        {
            return;
        }
        let Some(binding) = self.binding.clone() else {
            return;
        };
        self.delivery_loading = true;
        let area = self.area.clone();
        cx.spawn_in(window, {
            let module_access = reader_features::access(reader_features::Feature::Agents, cx);
            async move |this, cx| {
                okilum_t3::scope_network(module_access, async move {
                    let update = cx
                        .background_executor()
                        .spawn(okilum_t3::scope_current_network(async move {
                            facts::refresh(&area, &binding)
                        }))
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
                .await
            }
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
        self.delivery_summary_in("board-delivery", binding, cx)
    }
    fn delivery_summary_in(
        &self,
        namespace: &'static str,
        binding: &Binding,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = brand::palette(cx);
        let mut view = v_flex()
            .id(namespace)
            .debug_selector(move || namespace.into())
            .w_full()
            .min_w_0()
            .gap_1()
            .text_size(px(12.));
        let line_id = format!("{namespace}-line");
        let mut line = h_flex()
            .id(SharedString::from(line_id.clone()))
            .debug_selector(move || line_id.clone())
            .w_full()
            .min_w_0()
            .gap_2()
            .overflow_x_scroll();
        for channel in [facts::Channel::Stable, facts::Channel::Beta] {
            line = line.child(
                div()
                    .flex_shrink_0()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(channel.label()),
            );
            for platform in facts::Platform::ALL {
                let source = self.delivery.feeds.get(&(platform, channel));
                let build = source.and_then(|s| s.value.as_ref());
                let stale = facts::problem(&self.delivery.config, now(), binding.refresh_seconds)
                    .is_some()
                    || source.is_none_or(|s| {
                        facts::problem(s, now(), binding.refresh_seconds).is_some()
                    });
                let mut issue = build.is_none();
                let url = build.and_then(|b| b.release_url.clone());
                if let Some(b) = build {
                    issue |= b.published_at.is_none();
                    if let Some(artifact) = self.delivery.artifacts.get(&b.artifact) {
                        issue |= facts::problem(artifact, now(), binding.refresh_seconds).is_some()
                            || !artifact.value.available;
                    }
                }
                let name = if platform == facts::Platform::Windows {
                    "Win"
                } else {
                    platform.label()
                };
                let label =
                    build.map_or_else(|| format!("{name} —"), |b| format!("{name} {}", b.build));
                let tooltip = build.map_or("Publication is not available".into(), |b| {
                    format!(
                        "Published: {}\nSource: {}\nDate: release publication or feed update\n{}",
                        publication_date(b.published_at.as_deref()),
                        b.source,
                        if stale {
                            "Last known data; refresh is overdue or unavailable"
                        } else {
                            "Channel release notes"
                        }
                    )
                });
                let id = format!("{namespace}-{}-{}", platform.label(), channel.label());
                let problem_url = url.clone().unwrap_or_else(|| {
                    format!("{}/releases", binding.repository.trim_end_matches('/'))
                });
                line = line.child(
                    Button::new(SharedString::from(id.clone()))
                        .ghost()
                        .small()
                        .flex_shrink_0()
                        .debug_selector(move || id.clone())
                        .text_size(px(12.))
                        .label(label)
                        .tooltip(tooltip)
                        .disabled(url.is_none())
                        .on_click(move |_, _, cx| {
                            if let Some(url) = &url {
                                cx.open_url(url);
                            }
                        }),
                );
                if stale || issue {
                    // Preserve the main summary's existing problem identities.
                    let problem_prefix = if namespace == "board-delivery" {
                        "delivery"
                    } else {
                        namespace
                    };
                    let problem_id = format!(
                        "{problem_prefix}-problem-{}-{}",
                        platform.label(),
                        channel.label()
                    );
                    line = line.child(Button::new(SharedString::from(problem_id.clone()))
                        .debug_selector(move || problem_id.clone())
                        .ghost().small().flex_shrink_0().text_color(colors.text_muted).label(if stale { "Stale" } else { "Check" })
                        .tooltip(if stale { "Delivery data is out of date or unavailable. Open release notes for current publications." } else { "Publication date or download availability could not be verified. Open release notes." })
                        .on_click(move |_, _, cx| cx.open_url(&problem_url)));
                }
            }
        }
        view = view.child(line);
        view.into_any_element()
    }
    pub(super) fn delivery_details(
        &self,
        _: &okilum_t3::board::Scope,
        binding: &Binding,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // Board is a bird view. Diagnostics, PR history and evidence stay behind links.
        self.delivery_summary_in("board-delivery-details", binding, cx)
    }
}
fn binding_refresh(board: &Board) -> u64 {
    board.binding.as_ref().map_or(30, |b| b.refresh_seconds)
}

fn publication_date(value: Option<&str>) -> String {
    use chrono::{DateTime, Local};
    let Some(date) = value.and_then(|v| {
        DateTime::parse_from_rfc3339(v)
            .or_else(|_| DateTime::parse_from_rfc2822(v))
            .ok()
    }) else {
        return "date unavailable".into();
    };
    let date = date.with_timezone(&Local);
    if date.date_naive() == Local::now().date_naive() {
        format!("today {}", date.format("%H:%M"))
    } else {
        date.format("%d %b %Y %H:%M").to_string()
    }
}

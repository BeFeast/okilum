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
        let binding = okilum_t3::board::live::project(
            &binding,
            &self.snapshot.forgejo.value,
            self.projection_time(),
        );
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
                        this.list.invalidate();
                        this.delivery.accept(update, now());
                        this.filter_cache.invalidate();
                        this.refresh_waiting();
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
    pub(super) fn delivery_summary(
        &self,
        binding: &Binding,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        self.delivery_summary_in("board-delivery", binding, cx)
    }
    fn delivery_summary_in(
        &self,
        namespace: &'static str,
        binding: &Binding,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.delivery.enabled() || !self.delivery.feeds.values().any(|s| s.value.is_some()) {
            return None;
        }
        let colors = brand::palette(cx);
        let mut view = v_flex()
            .id(namespace)
            .debug_selector(move || namespace.into())
            .items_start()
            .min_w_0()
            // Expanded cards and narrow centers contain the intrinsic table.
            .overflow_x_scroll()
            .gap_1()
            .text_size(px(12.))
            .line_height(px(18.));
        let line_id = format!("{namespace}-line");
        // Intrinsic column widths keep dates beside builds at every viewport
        // width. Equal-height cells preserve platform/channel alignment.
        let mut line = h_flex()
            .id(SharedString::from(line_id.clone()))
            .debug_selector(move || line_id.clone())
            .items_start()
            .gap_2();
        let mut names = v_flex().flex_none().gap_1().child(div().h(px(24.)));
        let mut channels = [facts::Channel::Stable, facts::Channel::Beta]
            .into_iter()
            .filter(|channel| {
                self.delivery
                    .feeds
                    .iter()
                    .any(|((_, c), s)| c == channel && s.value.is_some())
            })
            .map(|channel| {
                (
                    channel,
                    v_flex().flex_none().items_start().gap_1().child(
                        h_flex()
                            .debug_selector(move || {
                                format!("{namespace}-channel-{}", channel.label())
                            })
                            .h(px(24.))
                            .text_color(colors.text_muted)
                            .child(channel.label()),
                    ),
                )
            })
            .collect::<Vec<_>>();
        for platform in facts::Platform::ALL {
            if !channels.iter().any(|(channel, _)| {
                self.delivery
                    .feeds
                    .get(&(platform, *channel))
                    .is_some_and(|s| s.value.is_some())
            }) {
                continue;
            }
            let mut problems = Vec::new();
            let mut cells = Vec::new();
            for (channel, _) in &channels {
                let channel = *channel;
                let source = self.delivery.feeds.get(&(platform, channel));
                let Some(build) = source.and_then(|s| s.value.as_ref()) else {
                    // Layout space keeps real cells aligned; absent data has no
                    // control, label, placeholder or diagnostic of its own.
                    cells.push(div().h(px(24.)));
                    continue;
                };
                let stale = facts::problem(&self.delivery.config, now(), binding.refresh_seconds)
                    .is_some()
                    || source.is_none_or(|s| {
                        facts::problem(s, now(), binding.refresh_seconds).is_some()
                    });
                let unverified = build.published_at.is_none()
                    || self
                        .delivery
                        .artifacts
                        .get(&build.artifact)
                        .is_none_or(|artifact| {
                            facts::problem(artifact, now(), binding.refresh_seconds).is_some()
                                || !artifact.value.available
                        });
                if stale {
                    problems.push(format!(
                        "{}: last known data; refresh is overdue or unavailable",
                        channel.label()
                    ));
                } else if unverified {
                    problems.push(format!(
                        "{}: publication date or download availability could not be verified",
                        channel.label()
                    ));
                }
                let url = build.release_url.clone();
                let label = format!(
                    "{} · {}",
                    build.build,
                    publication_date(build.published_at.as_deref())
                );
                let tooltip = format!(
                    "Source: {}\n{}",
                    build.source,
                    if stale {
                        "Last known data; refresh is overdue or unavailable"
                    } else {
                        "Open channel release notes"
                    }
                );
                let id = format!("{namespace}-{}-{}", platform.label(), channel.label());
                cells.push(
                    div().h(px(24.)).child(
                        Button::new(SharedString::from(id.clone()))
                            .ghost()
                            .small()
                            .debug_selector(move || id.clone())
                            .text_size(px(12.))
                            .accessibility_label(label.clone())
                            .child(div().line_height(px(18.)).child(label))
                            .tooltip(tooltip)
                            .disabled(url.is_none())
                            .on_click(move |_, _, cx| {
                                if let Some(url) = &url {
                                    cx.open_url(url);
                                }
                            }),
                    ),
                );
            }
            let mut name = h_flex()
                .debug_selector(move || format!("{namespace}-platform-{}", platform.label()))
                .h(px(24.))
                .flex_shrink_0()
                .gap_1()
                .child(platform.label());
            if !problems.is_empty() {
                let prefix = if namespace == "board-delivery" {
                    "delivery"
                } else {
                    namespace
                };
                let id = format!("{prefix}-problem-{}", platform.label());
                let url = format!("{}/releases", binding.repository.trim_end_matches('/'));
                name = name.child(
                    Button::new(SharedString::from(id.clone()))
                        .debug_selector(move || id.clone())
                        .ghost()
                        .small()
                        .text_color(colors.text_muted)
                        .label("ⓘ")
                        .tooltip(problems.join("\n"))
                        .on_click(move |_, _, cx| cx.open_url(&url)),
                );
            }
            names = names.child(name);
            for ((_, column), cell) in channels.iter_mut().zip(cells) {
                *column = std::mem::replace(column, v_flex()).child(cell);
            }
        }
        line = line
            .child(names)
            .children(channels.into_iter().map(|(_, column)| column));
        view = view.child(line);
        Some(view.into_any_element())
    }
    pub(super) fn delivery_details(
        &self,
        _: &okilum_t3::board::Scope,
        binding: &Binding,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
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

//! Reader-coupled glue and Reader-integration tests for `project::records`.
use super::*;
pub(crate) use okilum_shell_project::project::records::*;
use okilum_t3::records::Kind;

#[cfg(test)]
mod tests;

impl Reader {
    pub(crate) fn ensure_project_records(
        &mut self,
        area: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.project_navigation.records.contains_key(area) {
            return;
        }
        let mut published = self.vault.clone();
        let mut epoch = self.source_publication_epoch;
        let root = self.vault_root.clone();
        let records = cx.new(|cx| {
            RecordsView::new(area.to_owned(), root.clone(), published.clone(), window, cx)
        });
        let refreshed = records.downgrade();
        let publication = cx.observe(&cx.entity(), move |this, _, cx| {
            if this.vault_root != root
                || (Arc::ptr_eq(&published, &this.vault) && epoch == this.source_publication_epoch)
            {
                return;
            }
            published = this.vault.clone();
            epoch = this.source_publication_epoch;
            let _ = refreshed.update(cx, |records, cx| {
                records.vault = published.clone();
                records.changed(cx);
            });
        });
        records.update(cx, |records, _| records._publication = Some(publication));
        let owner = area.to_owned();
        cx.subscribe(&records, move |this, _, action: &board::Action, cx| {
            if this.project_navigation.active.as_ref().map(|p| &p.folder) != Some(&owner) {
                return;
            }
            if let Some(board) = this.project_navigation.boards.get(&owner) {
                board.update(cx, |_, cx| cx.emit(action.clone()));
            }
        })
        .detach();
        cx.observe(&records, |this, _, cx| this.project_board_changed(cx))
            .detach();
        self.project_navigation
            .records
            .insert(area.to_owned(), records);
    }

    pub(crate) fn render_project_tabs(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(area) = self
            .project_navigation
            .active
            .as_ref()
            .map(|p| p.folder.clone())
        else {
            return div().into_any_element();
        };
        let selected = self
            .project_navigation
            .records
            .get(&area)
            .and_then(|r| r.read(cx).kind);
        let mut tabs = h_flex()
            .w_full()
            .flex_shrink_0()
            .min_w_0()
            .px_3()
            .py_1()
            .gap_1()
            .flex_wrap()
            .debug_selector(|| "project-view-tabs".into())
            .border_b_1()
            .border_color(cx.theme().border);
        for kind in std::iter::once(None).chain(Kind::ALL.into_iter().map(Some)) {
            let label = kind.map_or("Board", Kind::label);
            let owner = area.clone();
            tabs = tabs.child(
                Button::new(SharedString::from(format!("project-tab-{label}")))
                    .debug_selector(move || format!("project-tab-{label}"))
                    .ghost()
                    .small()
                    .label(label)
                    .selected(selected == kind)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if this.project_navigation.active.as_ref().map(|p| &p.folder)
                            != Some(&owner)
                        {
                            return;
                        }
                        if let Some(records) = this.project_navigation.records.get(&owner) {
                            records.update(cx, |records, cx| records.select(kind, window, cx));
                        }
                        this.sync_project_board_visibility(window, cx);
                        cx.notify();
                    })),
            );
        }
        tabs.into_any_element()
    }
}

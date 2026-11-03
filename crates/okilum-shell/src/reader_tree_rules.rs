//! Folder rules navigation and bounded, off-thread shim annotation (#1587).
use super::*;
use okilum_core::quick_open::{read_agent_shim, AGENT_SHIM_MAX_BYTES};
use std::collections::BTreeSet;

fn discover_shims(vault: &Vault) -> BTreeSet<String> {
    vault
        .entries
        .iter()
        .filter(|entry| {
            entry.kind == okilum_core::vault::EntryKind::Markdown
                && matches!(
                    entry.path.rsplit('/').next(),
                    Some("CLAUDE.md" | "GEMINI.md")
                )
        })
        .filter_map(|entry| {
            let path = vault.root.join(&entry.path);
            let metadata = std::fs::symlink_metadata(&path).ok()?;
            if !metadata.is_file() || metadata.len() > AGENT_SHIM_MAX_BYTES {
                return None;
            }
            let file = std::fs::File::open(path).ok()?;
            read_agent_shim(&entry.path, file).then(|| entry.path.clone())
        })
        .collect()
}

impl Reader {
    pub(crate) fn refresh_tree_rules(&mut self, cx: &mut Context<Self>) {
        if self.single_file
            || !self.vault.inventory_scanned
            || self.tree_rules_running
            || self
                .tree_rules_source
                .as_ref()
                .is_some_and(|(source, epoch)| {
                    source.ptr_eq(&Arc::downgrade(&self.vault))
                        && *epoch == self.source_publication_epoch
                })
        {
            return;
        }
        self.tree_rules_running = true;
        let vault = self.vault.clone();
        let epoch = self.source_publication_epoch;
        cx.spawn(async move |this, cx| {
            let source = vault.clone();
            let shims = cx
                .background_executor()
                .spawn(async move { discover_shims(&source) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.tree_rules_running = false;
                // A different inventory/root supersedes this read. At most one
                // worker runs; coalesce changes into one follow-up on completion.
                if Arc::ptr_eq(&this.vault, &vault) && this.source_publication_epoch == epoch {
                    // This cache must not retain the full graph after a switch
                    // to single-file mode (the reader_loading release controls).
                    this.tree_rules_source = Some((Arc::downgrade(&vault), epoch));
                    this.tree.set_agent_shims(shims);
                    cx.notify();
                }
                this.refresh_tree_rules(cx);
            });
        })
        .detach();
    }
}

pub(crate) fn badge(
    path: String,
    reader: WeakEntity<Reader>,
    root: PathBuf,
    generation: u64,
) -> impl IntoElement {
    let id = SharedString::from(format!("tree-agent-rules-{path}"));
    Button::new(id.clone())
        .ghost()
        .xsmall()
        .label("@")
        .flex_none()
        .accessibility_label("Open agent rules")
        .tooltip(format!("Agent rules: {path}"))
        .debug_selector(move || id.to_string())
        // Do not select, expand, double-click or start dragging the folder.
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(move |_, window, cx| {
            cx.stop_propagation();
            let _ = reader.update(cx, |this, cx| {
                if this.vault_root == root && this.watcher_generation == generation {
                    this.select_panel_note(reader_layout::Panel::Notes, &path, None, window, cx);
                }
            });
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[test]
    fn issue1587_discovery_classifies_only_small_provider_shims() {
        let temp = tempfile::tempdir().unwrap();
        for (path, source) in [
            ("CLAUDE.md", "\u{feff} @./AGENTS.md\r\n"),
            ("GEMINI.md", "@AGENTS.md\n# Own rules\n"),
            ("AGENTS.md", "@AGENTS.md\n"),
        ] {
            std::fs::write(temp.path().join(path), source).unwrap();
        }
        std::fs::create_dir(temp.path().join("Large")).unwrap();
        std::fs::write(
            temp.path().join("Large/CLAUDE.md"),
            vec![b' '; 2 * 1024 * 1024],
        )
        .unwrap();
        let vault = Vault::scan_metadata(temp.path()).unwrap();
        assert_eq!(discover_shims(&vault), BTreeSet::from(["CLAUDE.md".into()]));
    }
}

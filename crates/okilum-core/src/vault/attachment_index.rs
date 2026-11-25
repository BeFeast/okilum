//! Vault-wide attachment suffix lookup, built once per inventory (#2803).
//!
//! Every Markdown render used to rebuild this map from the complete entry list:
//! each note open, Live Preview block preparation and incremental graph pass.
//! In a project folder with thousands of non-note files that cost ~250 ms per
//! render, on the UI thread for small notes. The index now lives with the
//! inventory it describes. Clones of an unchanged inventory share it, inventory
//! mutations drop it, and background publication warms it before the UI reads.
use super::{EntryKind, LinkKey, Vault};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, RwLock};

/// Every segment-boundary suffix of an attachment path (lowercased) -> the
/// attachments carrying that suffix, in path order. Values are positions in
/// the exact `Vault::entries` the index describes, so a retained index does not
/// duplicate every attachment path once per suffix.
pub(crate) struct AttachmentIndex {
    /// Inexpensive guard against an inventory mutation that bypassed
    /// [`Vault::invalidate_attachment_index`]; a mismatch rebuilds.
    entries: usize,
    symlinks: usize,
    /// Test/debug builds verify the exact inputs, so a missed invalidation
    /// fails loudly instead of resolving against a stale inventory.
    #[cfg(debug_assertions)]
    fingerprint: u64,
    map: HashMap<Box<str>, Vec<u32>>,
}

impl AttachmentIndex {
    /// Attachments carrying the lowercased suffix `key`, sorted by path.
    /// `vault` must be the inventory this index was fetched for.
    pub(crate) fn paths<'v>(
        &'v self,
        vault: &'v Vault,
        key: &str,
    ) -> impl Iterator<Item = &'v String> + 'v {
        self.map
            .get(key)
            .into_iter()
            .flatten()
            .map(move |&at| &vault.entries[at as usize].path)
    }

    fn build(vault: &Vault) -> Self {
        let _phase = crate::open_diagnostics::phase("link_index_build");
        BUILDS.with(|builds| builds.set(builds.get() + 1));
        let mut map: HashMap<Box<str>, Vec<u32>> = HashMap::new();
        for (at, entry) in vault.entries.iter().enumerate() {
            if entry.kind != EntryKind::Attachment || vault.graph_alias_path(Path::new(&entry.path))
            {
                continue;
            }
            let at = u32::try_from(at).expect("vault inventory exceeds u32 entries");
            let key = entry.path.link_key();
            let parts: Vec<_> = key.split('/').collect();
            for i in 0..parts.len() {
                map.entry(parts[i..].join("/").into_boxed_str())
                    .or_default()
                    .push(at);
            }
        }
        // Same order and uniqueness as sorting/deduplicating the path strings.
        for positions in map.values_mut() {
            positions.sort_by(|a, b| {
                vault.entries[*a as usize]
                    .path
                    .cmp(&vault.entries[*b as usize].path)
            });
            positions.dedup_by(|a, b| {
                vault.entries[*a as usize].path == vault.entries[*b as usize].path
            });
        }
        Self {
            entries: vault.entries.len(),
            symlinks: vault.symlink_paths.len(),
            #[cfg(debug_assertions)]
            fingerprint: fingerprint(vault),
            map,
        }
    }

    fn describes(&self, vault: &Vault) -> bool {
        if self.entries != vault.entries.len() || self.symlinks != vault.symlink_paths.len() {
            return false;
        }
        #[cfg(debug_assertions)]
        assert_eq!(
            self.fingerprint,
            fingerprint(vault),
            "attachment index outlived an inventory mutation without invalidation"
        );
        true
    }
}

/// Only attachment identities and symlink aliases determine the index.
#[cfg(debug_assertions)]
fn fingerprint(vault: &Vault) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for entry in &vault.entries {
        if entry.kind == EntryKind::Attachment {
            entry.path.hash(&mut hasher);
        }
    }
    // HashSet order is unspecified: combine element hashes commutatively.
    let symlinks = vault.symlink_paths.iter().fold(0u64, |sum, path| {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        path.hash(&mut hasher);
        sum.wrapping_add(hasher.finish())
    });
    symlinks.hash(&mut hasher);
    hasher.finish()
}

thread_local! {
    static BUILDS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Attachment index builds performed on the calling thread. Diagnostics and
/// tests use it to prove which thread pays for the index.
#[doc(hidden)]
pub fn attachment_index_builds() -> usize {
    BUILDS.with(std::cell::Cell::get)
}

/// Interior slot so immutable published `Arc<Vault>` handles can be warmed.
/// Clones of one inventory share the slot itself, so whichever clone builds
/// first (the temporary backlink resolver, a render, a publication warm) fills
/// it for all of them. A mutation detaches only the mutated vault.
#[derive(Default)]
pub(crate) struct Slot(Arc<RwLock<Option<Arc<AttachmentIndex>>>>);

impl Slot {
    /// The current index of `vault`, if one was already built for it.
    pub(crate) fn current(&self, vault: &Vault) -> Option<Arc<AttachmentIndex>> {
        self.0
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
            .filter(|index| index.describes(vault))
            .cloned()
    }

    pub(crate) fn get_or_build(&self, vault: &Vault) -> Arc<AttachmentIndex> {
        if let Some(index) = self.current(vault) {
            return index;
        }
        let mut slot = self
            .0
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(index) = slot.as_ref().filter(|index| index.describes(vault)) {
            return index.clone();
        }
        let index = Arc::new(AttachmentIndex::build(vault));
        *slot = Some(index.clone());
        index
    }

    /// A clone of an unchanged inventory shares the slot, cold or warm. The
    /// count/fingerprint guard still refuses an index built for other inputs.
    pub(crate) fn share(&self) -> Self {
        Self(self.0.clone())
    }

    /// Detach from every clone; their shared index stays valid for them.
    pub(crate) fn clear(&mut self) {
        self.0 = Arc::default();
    }
}

impl Vault {
    /// Build the attachment index now if this inventory has none. Background
    /// publication calls this so UI renders never pay for the vault-wide walk.
    pub fn warm_attachment_index(&self) {
        let _ = self.attachment_index.get_or_build(self);
    }

    /// Whether the current inventory already carries a built index.
    pub fn attachment_index_ready(&self) -> bool {
        self.attachment_index.current(self).is_some()
    }

    pub(crate) fn attachment_index(&self) -> Arc<AttachmentIndex> {
        self.attachment_index.get_or_build(self)
    }

    /// Drop the index after changing attachment or symlink identities.
    pub fn invalidate_attachment_index(&mut self) {
        self.attachment_index.clear();
    }

    /// The identity of the shared index, for tests that prove reuse.
    #[doc(hidden)]
    pub fn attachment_index_identity(&self) -> Option<usize> {
        self.attachment_index
            .current(self)
            .map(|index| Arc::as_ptr(&index) as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::VaultEntry;
    use ::core::prelude::v1::test;

    fn project(files: usize) -> Vault {
        let mut vault = Vault::from_note_paths(["notes/message.md".into()]);
        vault.root = std::path::PathBuf::from("/vault");
        vault.inventory_scanned = true;
        vault.inventory_complete = true;
        for i in 0..files {
            vault.entries.push(VaultEntry {
                path: format!("integrations/module{}/src/file{i}.json", i % 40),
                kind: EntryKind::Attachment,
            });
        }
        vault.entries.push(VaultEntry {
            path: "assets/diagram.png".into(),
            kind: EntryKind::Attachment,
        });
        vault.entries.sort_by(|a, b| a.path.cmp(&b.path));
        vault
    }

    fn render(vault: &Vault) -> String {
        crate::render::reader_document_from_source(
            vault,
            "notes/message.md",
            "# Message\n\n![](diagram.png)\n\n[spec](file7.json)\n",
        )
        .rendered
    }

    #[test]
    fn renders_of_one_inventory_build_the_attachment_index_once() {
        let vault = project(4_000);
        let before = attachment_index_builds();
        let first = render(&vault);
        assert_eq!(
            attachment_index_builds() - before,
            1,
            "positive control: the first file-link render builds the index"
        );
        assert!(first.contains("/vault/assets/diagram.png"), "{first}");
        for _ in 0..20 {
            assert_eq!(render(&vault), first, "cached index resolves identically");
        }
        assert_eq!(
            attachment_index_builds() - before,
            1,
            "later renders and Live Preview blocks reuse the inventory index"
        );
        let clone = vault.clone();
        assert_eq!(
            clone.attachment_index_identity(),
            vault.attachment_index_identity(),
            "a published clone shares the immutable index"
        );
        assert_eq!(render(&clone), first);
        assert_eq!(attachment_index_builds() - before, 1);
    }

    #[test]
    fn a_cold_clone_and_its_source_build_the_index_once() {
        let vault = project(1_000);
        let resolver = vault.clone();
        let before = attachment_index_builds();
        render(&resolver);
        assert_eq!(attachment_index_builds() - before, 1, "positive control");
        assert!(
            vault.attachment_index_ready(),
            "the source shares the clone's build"
        );
        vault.warm_attachment_index();
        assert_eq!(attachment_index_builds() - before, 1, "warming reuses it");
        let mut mutated = vault.clone();
        mutated.set_attachment_identity("assets/late.png", true);
        assert!(
            !mutated.attachment_index_ready(),
            "the mutated clone detaches"
        );
        assert!(
            vault.attachment_index_ready(),
            "detaching keeps the source's index"
        );
        assert_eq!(attachment_index_builds() - before, 1);
    }

    #[test]
    fn a_cold_reconcile_and_its_publication_warm_build_the_index_once() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path();
        std::fs::create_dir_all(root.join("integrations/src")).unwrap();
        for i in 0..200 {
            std::fs::write(root.join(format!("integrations/src/file{i}.json")), "{}").unwrap();
        }
        std::fs::write(root.join("message.md"), "[spec](file7.json)\n").unwrap();
        let before = attachment_index_builds();
        let (vault, _, _) =
            crate::vault::warm::reconcile(root, None, false, &mut |_, _| Ok(())).unwrap();
        assert_eq!(
            attachment_index_builds() - before,
            1,
            "positive control: the backlink pass resolved the file link"
        );
        assert_eq!(vault.backlinks("integrations/src/file7.json").len(), 1);
        vault.warm_attachment_index();
        vault.clone().warm_attachment_index();
        assert_eq!(
            attachment_index_builds() - before,
            1,
            "publication warming reuses the reconcile build"
        );
    }

    #[test]
    fn warming_moves_the_build_off_the_rendering_caller() {
        let vault = std::sync::Arc::new(project(2_000));
        let worker = vault.clone();
        std::thread::spawn(move || worker.warm_attachment_index())
            .join()
            .unwrap();
        assert!(vault.attachment_index_ready());
        let before = attachment_index_builds();
        render(&vault);
        assert_eq!(
            attachment_index_builds() - before,
            0,
            "a render after background warming performs no index build"
        );
    }

    #[test]
    fn inventory_mutations_rebuild_the_index_from_current_identities() {
        let mut vault = project(10);
        vault.warm_attachment_index();
        let stale = vault.attachment_index_identity();
        vault.set_attachment_identity("assets/new.png", true);
        assert!(
            !vault.attachment_index_ready(),
            "identity change invalidates"
        );
        let rendered =
            crate::render::reader_document_from_source(&vault, "notes/message.md", "![](new.png)")
                .rendered;
        assert!(rendered.contains("/vault/assets/new.png"), "{rendered}");
        assert_ne!(vault.attachment_index_identity(), stale);

        vault.set_attachment_identity("assets/new.png", false);
        let rendered =
            crate::render::reader_document_from_source(&vault, "notes/message.md", "![](new.png)")
                .rendered;
        assert!(!rendered.contains("/vault/assets/new.png"), "{rendered}");

        vault.warm_attachment_index();
        vault.register_created_note("notes/other.md");
        assert!(!vault.attachment_index_ready());
        vault.warm_attachment_index();
        vault.register_created_directory("fresh/dir");
        assert!(!vault.attachment_index_ready());
    }

    #[test]
    fn an_unannounced_entry_count_change_is_not_served_from_the_old_index() {
        let mut vault = project(10);
        vault.warm_attachment_index();
        // Direct field mutation bypasses invalidation; the guard still rebuilds.
        vault.entries.push(VaultEntry {
            path: "late/arrival.pdf".into(),
            kind: EntryKind::Attachment,
        });
        vault.entries.sort_by(|a, b| a.path.cmp(&b.path));
        assert!(!vault.attachment_index_ready());
        let before = attachment_index_builds();
        let index = vault.attachment_index();
        assert_eq!(
            index.paths(&vault, "arrival.pdf").collect::<Vec<_>>(),
            ["late/arrival.pdf"]
        );
        assert_eq!(attachment_index_builds() - before, 1);
    }
}

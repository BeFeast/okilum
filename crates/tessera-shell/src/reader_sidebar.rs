//! Sidebar sections (#369, docs/design/reader.md §Sidebar): Recent, Pinned,
//! Inbox and Folders. Recent/Pinned/collapsed sections are app-owned state per
//! root in the Reader state directory; nothing is written into notes. Inbox is
//! computed from the published inventory and is never stored as a list.
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use tessera_core::Vault;

pub const RECENT_SHOWN: usize = 5;
pub const RECENT_KEPT: usize = 10;

/// A section header's count badge (#687). An empty section shows no badge:
/// its placeholder row already says so, and the UI never shows a zero count.
pub fn section_count(len: usize) -> Option<usize> {
    (len > 0).then_some(len)
}

/// Notes created within this window and not yet filed are the Inbox.
pub const INBOX_WINDOW_SECS: u64 = 14 * 24 * 60 * 60;
/// `first_seen` marker for notes that existed when the baseline was taken.
const PRESENT_AT_BASELINE: u64 = 0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Section {
    Recent,
    Pinned,
    Inbox,
    Folders,
    /// The Properties section of the right panel (#386); kept here so its
    /// collapsed state persists with the other sections.
    Properties,
}

impl Section {
    pub fn label(self) -> &'static str {
        match self {
            Section::Recent => "Recent",
            Section::Pinned => "Pinned",
            Section::Inbox => "Inbox",
            Section::Folders => "Folders",
            Section::Properties => "Properties",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct State {
    /// Most recent first: (root-relative note, unix seconds).
    #[serde(default)]
    pub recent: Vec<(String, u64)>,
    /// Notes or folders, in pin order.
    #[serde(default)]
    pub pinned: Vec<String>,
    #[serde(default)]
    pub collapsed: BTreeSet<Section>,
    /// Kept outside `collapsed` so an older build, which cannot read the
    /// `Properties` variant, does not discard the whole state.
    #[serde(default)]
    pub properties_collapsed: bool,
    /// Show `_`/`.` files and folders in the tree for this root (#395).
    #[serde(default)]
    pub show_hidden: bool,
    /// Fallback creation time for files whose platform reports no birth time:
    /// the first time this app saw them. Paths present when the baseline was
    /// taken are treated as old and never recorded.
    #[serde(default)]
    pub first_seen: BTreeMap<String, u64>,
    #[serde(default)]
    pub baseline_taken: bool,
}

impl State {
    pub fn path(directory: &Path, root: &Path) -> PathBuf {
        let digest = Sha256::digest(root.to_string_lossy().as_bytes());
        let key: String = digest[..12].iter().map(|b| format!("{b:02x}")).collect();
        directory.join("sidebar").join(format!("{key}.json"))
    }

    pub fn load(path: &Path) -> Self {
        std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    /// Save from a background task. Saves are serialized and a snapshot older
    /// than the latest requested one is dropped, so disk never goes backwards.
    pub fn save_ordered(
        &self,
        path: &Path,
        sequence: u64,
        latest: &std::sync::atomic::AtomicU64,
    ) -> Result<()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        if latest.load(std::sync::atomic::Ordering::SeqCst) != sequence {
            return Ok(());
        }
        self.save(path)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let parent = path.parent().context("sidebar state has no directory")?;
        std::fs::create_dir_all(parent)?;
        let temporary = parent.join(format!(".sidebar-{}.json", uuid::Uuid::new_v4()));
        std::fs::write(&temporary, serde_json::to_vec(self)?)?;
        std::fs::rename(&temporary, path).inspect_err(|_| {
            let _ = std::fs::remove_file(&temporary);
        })?;
        Ok(())
    }

    /// Reopening (or reloading) the most recent note is not a new visit.
    pub fn record_open(&mut self, rel: &str, now: u64) -> bool {
        if self.recent.first().is_some_and(|(path, _)| path == rel) {
            return false;
        }
        self.recent.retain(|(path, _)| path != rel);
        self.recent.insert(0, (rel.to_owned(), now));
        self.recent.truncate(RECENT_KEPT);
        true
    }

    pub fn is_pinned(&self, path: &str) -> bool {
        self.pinned.iter().any(|p| p == path)
    }

    pub fn toggle_pin(&mut self, path: &str) {
        if self.is_pinned(path) {
            self.pinned.retain(|p| p != path);
        } else {
            self.pinned.push(path.to_owned());
        }
    }

    pub fn is_collapsed(&self, section: Section) -> bool {
        match section {
            Section::Properties => self.properties_collapsed,
            other => self.collapsed.contains(&other),
        }
    }

    pub fn toggle_section(&mut self, section: Section) {
        if section == Section::Properties {
            self.properties_collapsed = !self.properties_collapsed;
            return;
        }
        if !self.collapsed.remove(&section) {
            self.collapsed.insert(section);
        }
    }

    /// Drop entries whose files no longer exist in the inventory.
    pub fn prune(&mut self, vault: &Vault) {
        let entries: std::collections::HashSet<&str> =
            vault.entries.iter().map(|e| e.path.as_str()).collect();
        let known = |path: &str| entries.contains(path);
        if vault.inventory_complete {
            self.recent.retain(|(path, _)| known(path));
            self.pinned.retain(|path| known(path));
            self.first_seen.retain(|path, _| known(path));
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InboxReason {
    VaultRoot,
    DomainRoot,
    NoIncomingLinks,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InboxItem {
    pub path: String,
    pub created: u64,
    pub reason: InboxReason,
    /// The top-level folder for `DomainRoot`.
    pub domain: Option<String>,
}

/// Recently created notes not yet built into the structure: created within
/// the window, and either directly in the vault or a domain root (not inside
/// Projects/Areas/Resources/Archive), or without incoming links. Items leave
/// on their own once moved into a deeper folder and linked.
///
/// `created_at` supplies the platform birth time; `None` falls back to
/// `first_seen`, which this call extends for newly appeared notes.
pub fn compute_inbox(
    vault: &Vault,
    first_seen: &mut BTreeMap<String, u64>,
    baseline_taken: bool,
    now: u64,
    created_at: impl Fn(&str) -> Option<u64>,
) -> Vec<InboxItem> {
    let mut items = Vec::new();
    for note in &vault.notes {
        // Service folders are excluded by inventory; tree visibility is unrelated.
        if crate::reader_tree::archived(&note.path) {
            continue;
        }
        let created = match created_at(&note.path) {
            Some(created) => created,
            // Notes present when the baseline is taken are old: record them
            // with the epoch so a later pass does not date them "now".
            None if !baseline_taken => {
                first_seen
                    .entry(note.path.clone())
                    .or_insert(PRESENT_AT_BASELINE);
                continue;
            }
            None => match *first_seen.entry(note.path.clone()).or_insert(now) {
                PRESENT_AT_BASELINE => continue,
                seen => seen,
            },
        };
        if now.saturating_sub(created) > INBOX_WINDOW_SECS {
            continue;
        }
        let segments: Vec<&str> = note.path.split('/').collect();
        let reason = match segments.len() {
            1 => Some(InboxReason::VaultRoot),
            2 => Some(InboxReason::DomainRoot),
            _ if vault.backlinks(&note.path).is_empty() => Some(InboxReason::NoIncomingLinks),
            _ => None,
        };
        if let Some(reason) = reason {
            items.push(InboxItem {
                domain: (reason == InboxReason::DomainRoot).then(|| segments[0].to_owned()),
                path: note.path.clone(),
                created,
                reason,
            });
        }
    }
    items.sort_by(|a, b| b.created.cmp(&a.created).then_with(|| a.path.cmp(&b.path)));
    items
}

/// Platform birth time of a file, in unix seconds.
pub fn birth_time(root: &Path, rel: &str) -> Option<u64> {
    std::fs::metadata(root.join(rel))
        .and_then(|m| m.created())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
}

pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

/// Compact relative age: "now", "12m", "3h", "yesterday", "4d", "3w".
pub fn age_label(then: u64, now: u64) -> String {
    let secs = now.saturating_sub(then);
    match secs {
        0..60 => "now".into(),
        60..3_600 => format!("{}m", secs / 60),
        3_600..86_400 => format!("{}h", secs / 3_600),
        86_400..172_800 => "yesterday".into(),
        172_800..1_209_600 => format!("{}d", secs / 86_400),
        _ => format!("{}w", secs / 604_800),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: u64 = 86_400;

    fn vault_with(notes: &[(&str, &str)]) -> Vault {
        let root = std::env::temp_dir().join(format!("tessera-inbox-{}", uuid::Uuid::new_v4()));
        for (path, body) in notes {
            let file = root.join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, body).unwrap();
        }
        let vault = Vault::scan(&root).unwrap();
        let _ = std::fs::remove_dir_all(root);
        vault
    }

    #[test]
    fn empty_sections_carry_no_count() {
        assert_eq!(section_count(0), None);
        assert_eq!(section_count(3), Some(3));
    }

    #[test]
    fn inbox_is_new_and_unfiled_and_leaves_on_its_own() {
        let vault = vault_with(&[
            ("Loose.md", "# Loose"),
            ("Work/Idea.md", "# Idea"),
            ("Work/Projects/Linked.md", "# Linked"),
            ("Work/Projects/Orphan.md", "# Orphan"),
            ("Work/Projects/Old.md", "# Old"),
            ("Work/Areas/Index.md", "See [[Linked]]."),
            ("_Assets/Template.md", "# Template"),
            ("Work/4 Archive/Done.md", "# Done"),
        ]);
        let now = 100 * DAY;
        let created = |path: &str| match path {
            "Work/Projects/Old.md" => Some(now - 30 * DAY),
            "Work/Areas/Index.md" => Some(now - 60 * DAY),
            _ => Some(now - DAY),
        };
        let mut seen = BTreeMap::new();
        let items = compute_inbox(&vault, &mut seen, true, now, created);
        let found: Vec<(&str, InboxReason)> =
            items.iter().map(|i| (i.path.as_str(), i.reason)).collect();
        assert!(found.contains(&("Loose.md", InboxReason::VaultRoot)));
        assert!(found.contains(&("Work/Idea.md", InboxReason::DomainRoot)));
        assert!(found.contains(&("Work/Projects/Orphan.md", InboxReason::NoIncomingLinks)));
        // Filed and linked or too old: not Inbox. Hidden ordinary notes still qualify.
        assert!(!found.iter().any(|(p, _)| *p == "Work/Projects/Linked.md"));
        assert!(!found.iter().any(|(p, _)| *p == "Work/Projects/Old.md"));
        assert!(found.contains(&("_Assets/Template.md", InboxReason::DomainRoot)));
        assert!(!found.iter().any(|(p, _)| p.contains("Archive")));
        assert_eq!(
            items.len(),
            4,
            "positive control: all four new unfiled notes, including _Assets"
        );
        assert_eq!(
            items
                .iter()
                .find(|i| i.path == "Work/Idea.md")
                .unwrap()
                .domain
                .as_deref(),
            Some("Work")
        );
    }

    #[test]
    fn first_seen_fallback_starts_after_the_baseline() {
        let before = vault_with(&[("Loose.md", "# Loose")]);
        let mut seen = BTreeMap::new();
        // No birth time and no baseline yet: existing notes count as old and
        // stay old on later passes (no flood after the baseline).
        assert!(compute_inbox(&before, &mut seen, false, 10 * DAY, |_| None).is_empty());
        assert_eq!(seen.get("Loose.md"), Some(&PRESENT_AT_BASELINE));
        assert!(compute_inbox(&before, &mut seen, true, 11 * DAY, |_| None).is_empty());
        // A note that appears after the baseline is dated by first sight.
        let after = vault_with(&[("Loose.md", "# Loose"), ("New.md", "# New")]);
        let items = compute_inbox(&after, &mut seen, true, 12 * DAY, |_| None);
        assert_eq!(
            items.iter().map(|i| i.path.as_str()).collect::<Vec<_>>(),
            ["New.md"],
            "positive control: only the post-baseline note"
        );
        assert_eq!(seen.get("New.md"), Some(&(12 * DAY)));
        assert!(compute_inbox(&after, &mut seen, true, 30 * DAY, |_| None).is_empty());
    }

    #[test]
    fn auto_fold_survives_layout_clamp_without_changing_preferences() {
        let mut scroll = ScrollSections::default();
        let saved = BTreeSet::from([Section::Inbox]);
        assert!(scroll.observe(-28.));
        assert!(scroll.closed(Section::Recent, &saved));
        assert!(!scroll.closed(Section::Folders, &saved));
        // Enlarging the tree viewport can clamp a short tree back to zero.
        assert!(!scroll.observe(0.));
        assert!(!scroll.observe(0.));
        assert!(scroll.compact);
        scroll.reveal(Section::Pinned);
        assert!(!scroll.closed(Section::Pinned, &saved));
        assert!(!scroll.observe(-28.));
        assert!(scroll.observe(0.));
        assert!(!scroll.closed(Section::Recent, &saved));
        assert!(scroll.closed(Section::Inbox, &saved));
        assert!(!scroll.restore());
    }

    #[test]
    fn state_recent_pins_sections_round_trip_atomically() {
        let directory =
            std::env::temp_dir().join(format!("tessera-sidebar-{}", uuid::Uuid::new_v4()));
        let path = State::path(&directory, Path::new("/vault/a"));
        assert_ne!(path, State::path(&directory, Path::new("/vault/b")));
        let mut state = State::default();
        for i in 0..12 {
            state.record_open(&format!("n{i}.md"), i);
        }
        state.record_open("n3.md", 99);
        assert_eq!(state.recent.len(), RECENT_KEPT);
        assert_eq!(state.recent[0], ("n3.md".to_owned(), 99));
        state.toggle_pin("Work/Projects");
        state.toggle_section(Section::Recent);
        state.save(&path).unwrap();
        assert_eq!(State::load(&path), state);
        state.toggle_pin("Work/Projects");
        assert!(!state.is_pinned("Work/Projects"));
        assert_eq!(age_label(0, 90), "1m");
        assert_eq!(age_label(0, DAY + 5), "yesterday");
        let _ = std::fs::remove_dir_all(directory);
    }
}

/// Temporary presentation while browsing deeper in Folders. Never persisted.
#[derive(Default)]
pub(crate) struct ScrollSections {
    pub compact: bool,
    pub revealed: BTreeSet<Section>,
    last_offset: f32,
    ignore_reflow: bool,
}

impl ScrollSections {
    pub fn closed(&self, section: Section, saved: &BTreeSet<Section>) -> bool {
        saved.contains(&section)
            || (self.compact
                && matches!(section, Section::Recent | Section::Pinned | Section::Inbox)
                && !self.revealed.contains(&section))
    }

    /// Ignore the first clamp caused by changing the fixed section heights.
    /// Otherwise a short tree could alternate compact/expanded on every frame.
    pub fn observe(&mut self, offset: f32) -> bool {
        let mut changed = false;
        if !self.compact && offset < -0.5 {
            self.compact = true;
            self.revealed.clear();
            self.ignore_reflow = true;
            changed = true;
        } else if self.compact {
            if self.ignore_reflow {
                self.ignore_reflow = false;
            } else if offset >= -0.5 && self.last_offset < -0.5 {
                changed = self.restore();
            }
        }
        self.last_offset = offset;
        changed
    }

    pub fn restore(&mut self) -> bool {
        let changed = self.compact;
        *self = Self::default();
        changed
    }

    pub fn reveal(&mut self, section: Section) {
        self.revealed.insert(section);
        self.ignore_reflow = true;
    }
}

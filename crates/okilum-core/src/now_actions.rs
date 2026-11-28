//! Explicit, revision-bound edits behind Now's inline vault actions.
use crate::frontmatter_edit::{activate_case, has_lone_cr, revision, validate_relative_path};
use crate::{file_editor::FileEditor, now_feed, vault_descriptor, vault_mutation};
use anyhow::{bail, ensure, Context, Result};
use chrono::{DateTime, Utc};
use comrak::{nodes::NodeValue, parse_document, Arena, Options};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    path: String,
    revision: String,
}

impl Target {
    fn capture(path: &str, source: &str) -> Self {
        Self {
            path: path.to_owned(),
            revision: revision(source),
        }
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    fn verify(&self, source: &str) -> Result<()> {
        ensure!(
            revision(source) == self.revision,
            "The note changed. Rescan before submitting again."
        );
        ensure!(
            !has_lone_cr(source),
            "The note contains a lone carriage return"
        );
        Ok(())
    }
}

/// Revisions shown by one Now load. Each target is independently optional so
/// one unavailable file cannot stop the rest of the feed.
#[derive(Default)]
pub struct Targets {
    pub cases: BTreeMap<String, Target>,
    pub areas: BTreeMap<String, Target>,
}

pub fn load_targets(root: &Path, feed: &now_feed::LoadOutcome) -> Targets {
    let now_feed::LoadOutcome::Ready(feed) = feed else {
        return Targets::default();
    };
    let mut targets = Targets::default();
    let Ok(root) = root.canonicalize() else {
        return targets;
    };
    let capture = |path: &str| {
        let absolute = bound_path(&root, path).ok()?;
        let source = fs::read_to_string(absolute).ok()?;
        Some(Target::capture(path, &source))
    };
    let paths: BTreeSet<_> = feed
        .groups
        .iter()
        .flat_map(|group| group.items.iter().chain(&group.hidden_items))
        .filter_map(now_feed::FeedItem::case_reply_path)
        .collect();
    for path in paths {
        if let Some(target) = capture(path) {
            targets.cases.insert(path.to_owned(), target);
        }
    }
    let state = Path::new("state");
    let entries = vault_mutation::validate_directories(&root, state, false)
        .and_then(|()| Ok(fs::read_dir(root.join(state))?));
    if let Ok(entries) = entries {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(slug) = name.to_str() else { continue };
            let path = format!("state/{slug}/@index.md");
            if let Some(target) = capture(&path) {
                targets.areas.insert(slug.to_owned(), target);
            }
        }
    }
    targets
}

#[derive(Clone, Copy)]
pub enum Action {
    DecideCase,
    AddIntention,
}

impl Action {
    fn headings(self) -> [&'static str; 2] {
        match self {
            Self::DecideCase => ["История", "History"],
            Self::AddIntention => ["Намерения", "Intentions"],
        }
    }

    fn plan(self, source: &str, text: &str, owner: &str, now: DateTime<Utc>) -> Result<String> {
        let text = one_line(text)?;
        let now = now.with_timezone(&chrono_tz::Asia::Jerusalem);
        let today = now.format("%Y-%m-%d").to_string();
        let (base, bullet) = match self {
            Self::DecideCase => (
                activate_case(source, &today).map_err(anyhow::Error::msg)?,
                format!(
                    "- {} — {}: {text}",
                    now.format("%d.%m %H:%M"),
                    one_line(owner)?
                ),
            ),
            Self::AddIntention => (source.to_owned(), format!("- {text} ({today})")),
        };
        let body_start = body_start(&base);
        let body = &base[body_start..];
        let target_section = section(body, self.headings(), None)?;
        let heading_line = line(body, target_section.heading);
        let preceding = &base[..body_start + target_section.heading];
        let eol = if heading_line.ends_with("\r\n")
            || (!heading_line.ends_with('\n') && preceding.ends_with("\r\n"))
        {
            "\r\n"
        } else {
            "\n"
        };
        let after_heading = target_section.heading + line(body, target_section.heading).len();
        let mut at = match self {
            Self::DecideCase => {
                let blank = line(body, after_heading);
                if blank.trim().is_empty() && blank.ends_with('\n') {
                    after_heading + blank.len()
                } else {
                    after_heading
                }
            }
            Self::AddIntention => target_section.end,
        };
        if matches!(self, Self::AddIntention) {
            // Keep trailing separators in place, between the new item and the
            // next section, including when the source ends in blank lines.
            while at > after_heading {
                let previous = &body[..at];
                let start = previous
                    .strip_suffix('\n')
                    .unwrap_or(previous)
                    .rfind('\n')
                    .map_or(0, |index| index + 1);
                if start < after_heading || !body[start..at].trim().is_empty() {
                    break;
                }
                at = start;
            }
        }
        let prefix = &body[..at];
        let suffix = &body[at..];
        let mut inserted = String::new();
        if !prefix.ends_with('\n') {
            inserted.push_str(eol);
        }
        // A blank line is required to start a list after prose.
        let preceding_line = prefix.lines().next_back().unwrap_or_default();
        if at == after_heading
            || (matches!(self, Self::AddIntention)
                && !preceding_line.trim().is_empty()
                && !preceding_line.starts_with("- "))
        {
            inserted.push_str(eol);
        }
        let bullet_at = at + inserted.len();
        inserted.push_str(&bullet);
        if !suffix.is_empty() || body.ends_with('\n') {
            inserted.push_str(eol);
        }
        if !suffix.is_empty()
            && !suffix
                .trim_start_matches([' ', '\t'])
                .starts_with(['\r', '\n'])
            && !suffix.starts_with("- ")
        {
            inserted.push_str(eol);
        }

        let mut after = base.clone();
        after.insert_str(body_start + at, &inserted);
        ensure!(
            after[..body_start + at] == base[..body_start + at]
                && after[body_start + at + inserted.len()..] == base[body_start + at..],
            "The inline edit did not preserve unrelated source bytes"
        );
        let after_body = &after[body_start..];
        let verified = section(after_body, self.headings(), Some(bullet_at))?;
        ensure!(
            verified.heading == target_section.heading
                && line(after_body, bullet_at).trim_end_matches(['\r', '\n']) == bullet,
            "Could not verify the new line under the expected heading"
        );
        Ok(after)
    }
}

fn one_line(text: &str) -> Result<&str> {
    ensure!(
        !text.trim().is_empty()
            && !text
                .chars()
                .any(|ch| ch.is_control() || matches!(ch, '\u{2028}' | '\u{2029}')),
        "Enter one non-empty line of text"
    );
    Ok(text.trim())
}

fn body_start(source: &str) -> usize {
    crate::render::split_frontmatter(source).map_or_else(
        || usize::from(source.starts_with('\u{feff}')) * '\u{feff}'.len_utf8(),
        |(_, body)| source.len() - body.len(),
    )
}

fn line(source: &str, at: usize) -> &str {
    let tail = &source[at..];
    &tail[..tail.find('\n').map_or(tail.len(), |index| index + 1)]
}

struct Section {
    heading: usize,
    end: usize,
}

fn mask(bytes: &mut [u8]) {
    for byte in bytes {
        if !matches!(byte, b'\r' | b'\n') {
            *byte = b' ';
        }
    }
}

/// Mask HTML and Obsidian comments without changing authored byte offsets,
/// then use Markdown source positions to exclude code and verify the list item.
fn section(body: &str, names: [&str; 2], bullet: Option<usize>) -> Result<Section> {
    let mut masked = body.as_bytes().to_owned();
    if body.contains("<!--") {
        let prose = crate::prose::prose_spans(body);
        let mut end = 0;
        for (start, _) in body.match_indices("<!--") {
            let i = prose.partition_point(|span| span.end <= start);
            if start >= end && prose.get(i).is_some_and(|span| span.contains(&start)) {
                end = body[start + 4..]
                    .find("-->")
                    .map_or(body.len(), |delta| start + 7 + delta);
                mask(&mut masked[start..end]);
            }
        }
    }
    for span in crate::obsidian::comment_spans(std::str::from_utf8(&masked)?) {
        mask(&mut masked[span]);
    }
    let masked = std::str::from_utf8(&masked)?;
    let arena = Arena::new();
    let doc = parse_document(&arena, masked, &Options::default());
    let offsets: Vec<_> = std::iter::once(0)
        .chain(body.match_indices('\n').map(|(offset, _)| offset + 1))
        .collect();
    let mut headings = Vec::new();
    let mut candidates = Vec::new();
    let mut item_found = false;
    for node in doc.descendants().skip(1) {
        let data = node.data.borrow();
        let Some(&offset) = offsets.get(data.sourcepos.start.line.saturating_sub(1)) else {
            continue;
        };
        let top_level = node
            .parent()
            .is_some_and(|parent| std::ptr::eq(parent, doc));
        match &data.value {
            NodeValue::Heading(heading) if heading.level <= 2 => {
                if top_level {
                    headings.push(offset);
                }
                if heading.level == 2 {
                    // heading_text borrows descendant data, so release this AST borrow.
                    let atx = !heading.setext;
                    drop(data);
                    let title = crate::document_links::heading_text(node);
                    if names.contains(&title.trim()) {
                        candidates.push((offset, atx && top_level));
                    }
                }
            }
            NodeValue::Item(_) if bullet == Some(offset) => {
                item_found |= node
                    .parent()
                    .and_then(|list| list.parent())
                    .is_some_and(|parent| std::ptr::eq(parent, doc))
                    && data.sourcepos.start.column == 1
                    && line(masked, offset) == line(body, offset);
            }
            _ => {}
        }
    }
    let [(heading, true)] = candidates.as_slice() else {
        bail!(
            "Expected one unambiguous ## {} (or ## {}) heading",
            names[0],
            names[1]
        );
    };
    let end = headings
        .into_iter()
        .find(|at| at > heading)
        .unwrap_or(body.len());
    if let Some(bullet) = bullet {
        ensure!(
            item_found && *heading < bullet && bullet < end,
            "The new line is outside the target section"
        );
    }
    Ok(Section {
        heading: *heading,
        end,
    })
}

fn is_intention_path(relative: &str) -> bool {
    let parts: Vec<_> = relative.split('/').collect();
    matches!(parts.as_slice(), ["state", _, "@index.md"])
}

fn bound_path(root: &Path, relative: &str) -> Result<PathBuf> {
    validate_relative_path(relative).map_err(anyhow::Error::msg)?;
    let relative = Path::new(relative);
    ensure!(
        relative
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("md")),
        "Choose a Markdown note in this vault"
    );
    let parent = relative.parent().context("Missing destination folder")?;
    vault_mutation::validate_directories(root, parent, false)?;
    let boundaries = vault_descriptor::declared_boundaries(root);
    for ancestor in parent
        .ancestors()
        .filter(|path| !path.as_os_str().is_empty())
    {
        let directory = root.join(ancestor);
        let key = crate::vault::note_path(ancestor);
        ensure!(
            vault_descriptor::boundary_at(root, &boundaries, &key).is_none(),
            "The note is across a nested vault boundary"
        );
        for marker in [
            vault_descriptor::path(&directory),
            directory.join(".obsidian"),
        ] {
            match marker.symlink_metadata() {
                Ok(_) => bail!("The note is across a nested vault boundary"),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error).context("Cannot verify the nested vault boundary"),
            }
        }
    }
    let path = root.join(relative);
    let metadata = fs::symlink_metadata(&path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        ensure!(metadata.nlink() == 1, "The note has multiple hard links");
    }
    ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink() && path.canonicalize()? == path,
        "The note must be a regular file inside the vault"
    );
    Ok(path)
}

fn open_clean(root: &Path, drafts: &Path, relative: &str) -> Result<FileEditor> {
    let path = bound_path(root, relative)?;
    vault_descriptor::Lifecycle::check_ancestors(
        path.parent().context("Missing destination folder")?,
    )?;
    ensure!(
        !fs::metadata(root)?.permissions().readonly(),
        "This vault is read-only"
    );
    ensure!(
        !fs::metadata(&path)?.permissions().readonly(),
        "The note is read-only"
    );
    crate::file_editor::check_writable_in_place(&path)?;
    let editor = FileEditor::open(&path, drafts)?;
    ensure!(
        editor.path() == path,
        "The note location changed while opening"
    );
    ensure!(
        !editor.has_pending_revision(),
        "The note has unsaved edits. Open its source before using this action."
    );
    Ok(editor)
}

fn save(root: &Path, relative: &str, editor: &mut FileEditor, after: String) -> Result<()> {
    let _operation = okilum_write_authority::operation(root)?;
    ensure!(
        bound_path(root, relative)? == editor.path(),
        "The note location changed"
    );
    let _destination = okilum_write_authority::operation_for_path(editor.path())?;
    editor.set_text(after)?;
    match editor.save()? {
        crate::file_editor::Save::Saved => Ok(()),
        crate::file_editor::Save::Conflict => {
            bail!("The note changed during save. A recovery draft was kept.")
        }
    }
}

#[derive(Clone, Debug)]
pub struct Receipt {
    root: PathBuf,
    saved: Target,
    before: String,
}

impl Receipt {
    pub fn path(&self) -> &str {
        self.saved.path()
    }

    pub fn saved_revision(&self) -> &str {
        &self.saved.revision
    }

    pub fn undo(&self, root: &Path, drafts: &Path) -> Result<()> {
        ensure!(
            !root.symlink_metadata()?.file_type().is_symlink(),
            "Choose a real vault directory"
        );
        ensure!(
            root.canonicalize()? == self.root,
            "Undo belongs to another vault"
        );
        let mut editor = open_clean(&self.root, drafts, self.path())?;
        ensure!(
            revision(editor.text()) == self.saved.revision,
            "The note changed after this action. Undo kept the newer source."
        );
        save(&self.root, self.path(), &mut editor, self.before.clone())
    }
}

pub fn apply(
    root: &Path,
    drafts: &Path,
    target: &Target,
    action: Action,
    text: &str,
    now: DateTime<Utc>,
) -> Result<Receipt> {
    ensure!(
        !root.symlink_metadata()?.file_type().is_symlink(),
        "Choose a real vault directory"
    );
    let root = root.canonicalize()?;
    ensure!(
        !matches!(action, Action::AddIntention) || is_intention_path(target.path()),
        "Choose an area overview"
    );
    // Register the selected root before FileEditor resolves nested publishers.
    okilum_write_authority::Authority::for_root(&root)?;
    let mut editor = open_clean(&root, drafts, target.path())?;
    let before = editor.text().to_owned();
    target.verify(&before)?;
    let owner = match action {
        Action::DecideCase => now_feed::load_role_names(&root)
            .remove("owner")
            .unwrap_or_else(|| "owner".into()),
        Action::AddIntention => String::new(),
    };
    let after = action.plan(&before, text, &owner, now)?;
    let saved = Target::capture(target.path(), &after);
    save(&root, target.path(), &mut editor, after)?;
    Ok(Receipt {
        root,
        saved,
        before,
    })
}

#[cfg(test)]
mod tests;

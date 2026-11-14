//! Untitled new files kept as drafts (#1528), like Notepad++ and Zed.
//!
//! «New untitled file» needs no name and no folder. Its text is a real file
//! in Okilum's own state directory (`<state>/untitled/<id>/Untitled.md`),
//! opened in single-file mode, so the editor's autosave and recovery drafts
//! protect it exactly like any other file: a crash or kill loses nothing, and
//! quitting never asks «Save changes?». The draft reappears in Recent with an
//! «Unsaved» badge until Save puts it in a chosen folder. Save proposes the
//! folder of the file the draft was created from.
use super::*;
use serde::{Deserialize, Serialize};

/// Below the session directory; never inside a vault or an index.
const DIRECTORY: &str = "untitled";
/// The one document of a draft folder.
pub(super) const FILE: &str = "Untitled.md";
const META: &str = "draft.json";
/// Written when Save put the text and attachments elsewhere: a manifest of
/// exactly what was saved. It counts only while it still matches the folder.
const SAVED: &str = "saved";
/// How long a saved draft folder stays before it is removed.
const SAVED_LIFETIME: u64 = 600;
/// Recent shows at most this many drafts.
const LISTED: usize = 40;
/// An empty draft nobody typed into is removed after a day.
const EMPTY_DRAFT_LIFETIME: u64 = 86_400;
/// A draft folder with more files than this is never classified, only kept.
const MAX_ATTACHMENTS: usize = 512;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
struct Meta {
    /// The folder of the file the draft was created from: where Save starts.
    #[serde(default)]
    origin: Option<PathBuf>,
    #[serde(default)]
    created: u64,
}

/// One attachment of a draft: its path below the draft folder, size and digest.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Attachment {
    pub relative: PathBuf,
    pub size: u64,
    pub sha256: String,
}

/// What Save put elsewhere. The draft is retired only while its current text
/// and attachments are exactly these: newer edits from any window revive it.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
struct Manifest {
    text_sha256: String,
    attachments: Vec<Attachment>,
}

/// What a draft holds right now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Content {
    /// The newest text: the recovery journal when it is ahead of the file
    /// (a crash before the first autosave), else the file.
    Text(String),
    /// The file or its recovery data cannot be read: keep, never classify.
    Unknown,
}

/// One unsaved draft, as Recent lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Listed {
    pub file: PathBuf,
    pub label: String,
    pub modified: u64,
}

/// The person's home folder.
pub(super) fn home() -> Option<PathBuf> {
    #[cfg(windows)]
    let home = dirs::home_dir();
    #[cfg(not(windows))]
    let home = std::env::var_os("HOME").map(PathBuf::from);
    home.filter(|home| home.is_absolute())
}

fn drafts_directory(session: &Path) -> PathBuf {
    session.join(DIRECTORY)
}

/// The editor's recovery journals, as `Reader::open_source` uses them.
fn recovery_directory(session: &Path) -> PathBuf {
    session.join("editor-drafts")
}

fn seconds(time: std::time::SystemTime) -> u64 {
    time.duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

fn digest(bytes: &[u8]) -> String {
    use sha2::Digest as _;
    format!("{:x}", sha2::Sha256::digest(bytes))
}

/// Create an empty draft and return its file. The folder and its metadata are
/// synced before the path is handed out, so a crash right after leaves a
/// draft that Recent can list, never a dangling reference.
pub(super) fn create(session: &Path, origin: Option<&Path>) -> anyhow::Result<PathBuf> {
    let parent = drafts_directory(session);
    std::fs::create_dir_all(&parent)?;
    collect(session, reader_sidebar::now());
    let directory = parent.join(uuid::Uuid::new_v4().to_string());
    std::fs::create_dir(&directory)?;
    let meta = Meta {
        origin: origin
            .filter(|path| path.is_absolute())
            .map(Path::to_path_buf),
        created: reader_sidebar::now(),
    };
    write_new(&directory.join(META), &serde_json::to_vec(&meta)?)?;
    write_new(&directory.join(FILE), b"")?;
    #[cfg(unix)]
    {
        std::fs::File::open(&directory)?.sync_all()?;
        std::fs::File::open(&parent)?.sync_all()?;
    }
    Ok(directory.join(FILE))
}

/// Create `path` (never replace it) with `bytes`, synced.
fn write_new(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    use std::io::Write as _;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

/// The draft's newest text, consulting recovery before the file.
pub(super) fn content(session: &Path, folder: &Path) -> Content {
    let file = folder.join(FILE);
    match okilum_core::file_editor::FileEditor::unsaved_draft_text(
        &file,
        &recovery_directory(session),
    ) {
        Ok(Some(text)) => return Content::Text(text),
        Ok(None) => {}
        Err(_) => return Content::Unknown,
    }
    match std::fs::read_to_string(&file) {
        Ok(text) => Content::Text(text),
        Err(_) => Content::Unknown,
    }
}

/// Every file of the draft folder except its own bookkeeping and hidden
/// top-level temporaries: pasted images and files. `None` when the folder cannot be read completely.
pub(super) fn attachments(folder: &Path) -> Option<Vec<Attachment>> {
    let mut found = Vec::new();
    let mut pending = vec![PathBuf::new()];
    while let Some(relative) = pending.pop() {
        for entry in std::fs::read_dir(folder.join(&relative)).ok()? {
            let entry = entry.ok()?;
            let name = entry.file_name();
            let child = relative.join(&name);
            let kind = entry.file_type().ok()?;
            // Okilum's own bookkeeping and hidden temporaries at the top.
            if relative.as_os_str().is_empty()
                && ([FILE, META, SAVED].iter().any(|own| name == *own)
                    || name.to_string_lossy().starts_with('.'))
            {
                continue;
            }
            if kind.is_dir() {
                pending.push(child);
            } else {
                let bytes = std::fs::read(folder.join(&child)).ok()?;
                found.push(Attachment {
                    relative: child,
                    size: bytes.len() as u64,
                    sha256: digest(&bytes),
                });
            }
            if found.len() + pending.len() > MAX_ATTACHMENTS {
                return None;
            }
        }
    }
    found.sort();
    Some(found)
}

/// Whether an editor in any window still holds the draft. Unknown counts as live.
pub(super) fn live(session: &Path, folder: &Path) -> bool {
    okilum_core::file_editor::FileEditor::editor_active(
        &folder.join(FILE),
        &recovery_directory(session),
    )
    .unwrap_or(true)
}

/// The draft was saved elsewhere and has not changed since: its manifest
/// still matches its text (recovery included) and its attachments.
fn retired(session: &Path, folder: &Path) -> bool {
    let Ok(bytes) = std::fs::read(folder.join(SAVED)) else {
        return false;
    };
    let Ok(manifest) = serde_json::from_slice::<Manifest>(&bytes) else {
        return false;
    };
    let Content::Text(text) = content(session, folder) else {
        return false;
    };
    digest(text.as_bytes()) == manifest.text_sha256
        && attachments(folder).as_ref() == Some(&manifest.attachments)
}

/// Nothing was ever put in the draft: no text, no recovery, no attachment.
fn untouched(session: &Path, folder: &Path) -> bool {
    content(session, folder) == Content::Text(String::new())
        && attachments(folder).is_some_and(|found| found.is_empty())
}

/// Remove what can no longer hold anything unsaved: a draft retired for ten
/// minutes, or one left untouched for a day. A live editor, recovery data,
/// text, attachments or anything unreadable keeps the folder.
fn collect(session: &Path, now: u64) {
    let Ok(entries) = std::fs::read_dir(drafts_directory(session)) else {
        return;
    };
    for entry in entries.flatten().take(256) {
        let folder = entry.path();
        if !folder.is_dir() {
            continue;
        }
        // Held through classification and removal: no editor can take the
        // draft in between. Busy or unknown keeps the folder.
        let Ok(Some(_claim)) = okilum_core::file_editor::FileEditor::claim_idle(
            &folder.join(FILE),
            &recovery_directory(session),
        ) else {
            continue;
        };
        let age = |name: &str| {
            std::fs::symlink_metadata(folder.join(name))
                .and_then(|m| m.modified())
                .map(|t| now.saturating_sub(seconds(t)))
                .ok()
        };
        let expired = if folder.join(SAVED).exists() {
            age(SAVED).is_some_and(|age| age > SAVED_LIFETIME) && retired(session, &folder)
        } else {
            age(FILE).is_some_and(|age| age > EMPTY_DRAFT_LIFETIME) && untouched(session, &folder)
        };
        if expired {
            #[cfg(test)]
            BEFORE_REMOVAL.with(|hook| {
                if let Some(hook) = hook.borrow().as_ref() {
                    hook(&folder)
                }
            });
            let _ = std::fs::remove_dir_all(&folder);
        }
    }
}

/// A test hook given the draft folder about to be removed.
#[cfg(test)]
type RemovalHook = Option<Box<dyn Fn(&Path)>>;

#[cfg(test)]
thread_local! {
    /// Runs right before a draft folder is removed, with the claim still held.
    static BEFORE_REMOVAL: std::cell::RefCell<RemovalHook> =
        const { std::cell::RefCell::new(None) };
}

/// The draft folder `root` is, when it is one: a direct child of this
/// session's drafts directory. Both sides are compared canonically.
pub(super) fn draft_folder(session: &Path, root: &Path) -> Option<PathBuf> {
    let parent = drafts_directory(session).canonicalize().ok()?;
    let root = root.canonicalize().ok()?;
    (root.parent() == Some(parent.as_path())).then_some(root)
}

/// Where Save starts: the folder the draft came from, while it exists.
pub(super) fn origin(folder: &Path) -> Option<PathBuf> {
    let bytes = std::fs::read(folder.join(META)).ok()?;
    let meta: Meta = serde_json::from_slice(&bytes).ok()?;
    meta.origin.filter(|origin| origin.is_dir())
}

/// The label of a draft: its first non-empty line without heading marks.
pub(super) fn label(text: &str) -> Option<String> {
    let line = text
        .lines()
        .map(|line| line.trim_start_matches('#').trim())
        .find(|line| !line.is_empty())?;
    let mut label: String = line.chars().take(60).collect();
    if line.chars().count() > 60 {
        label.push('…');
    }
    Some(label)
}

/// A file name for Save: the label without characters no system accepts.
pub(super) fn suggested_name(text: &str) -> String {
    let stem: String = label(text)
        .unwrap_or_default()
        .trim_end_matches('…')
        .chars()
        .map(|c| {
            if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control() {
                '-'
            } else {
                c
            }
        })
        .collect();
    let stem = stem.trim().trim_matches('.').trim();
    format!("{}.md", if stem.is_empty() { "Untitled" } else { stem })
}

/// Drafts holding anything unsaved, newest first, bounded: text in the file
/// or only in recovery, or attachments. Retired drafts are left out.
pub(super) fn list(session: &Path) -> Vec<Listed> {
    let Ok(entries) = std::fs::read_dir(drafts_directory(session)) else {
        return Vec::new();
    };
    let mut listed: Vec<Listed> = entries
        .flatten()
        .take(256)
        .filter_map(|entry| {
            let folder = entry.path();
            let file = folder.join(FILE);
            let metadata = std::fs::symlink_metadata(&file).ok()?;
            if !metadata.is_file() || retired(session, &folder) || untouched(session, &folder) {
                return None;
            }
            let text = match content(session, &folder) {
                Content::Text(text) => text,
                Content::Unknown => String::new(),
            };
            Some(Listed {
                label: label(&text).unwrap_or_else(|| "Untitled".into()),
                modified: metadata.modified().map(seconds).unwrap_or_default(),
                file,
            })
        })
        .collect();
    listed.sort_by(|a, b| b.modified.cmp(&a.modified).then(a.file.cmp(&b.file)));
    listed.truncate(LISTED);
    listed
}

/// Characters a wiki target cannot carry, and that the attachment folder's
/// own name therefore never gets.
const WIKI_UNSAFE: &[char] = &['|', '#', '^', '[', ']', '\\', '\n', '\r'];

/// Percent-encoded in a rewritten Markdown destination: whatever would end
/// the destination, start a fragment or query, or be decoded differently.
const DESTINATION: &percent_encoding::AsciiSet = &percent_encoding::CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'%')
    .add(b'(')
    .add(b')')
    .add(b'<')
    .add(b'>')
    .add(b'?')
    .add(b'[')
    .add(b'\\')
    .add(b']')
    .add(b'`')
    .add(b'{')
    .add(b'|')
    .add(b'}');

/// The draft's attachments as Save carries them: a fresh folder beside the
/// saved note, every file keeping its name and path below it.
#[derive(Debug)]
pub(super) struct Carried {
    /// The folder created for them, if there were any.
    pub folder: Option<PathBuf>,
    /// The note text with its links to them pointing into that folder.
    pub text: String,
}

/// Why Save refused before writing anything; the draft is kept.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Refused(pub String);

/// The folder name for a note's attachments: `<stem> attachments`, without
/// characters a wiki link could not carry.
fn attachments_folder_name(stem: &str, n: usize) -> String {
    let stem: String = stem
        .chars()
        .map(|c| if WIKI_UNSAFE.contains(&c) { '-' } else { c })
        .collect();
    let stem = stem.trim();
    let stem = if stem.is_empty() { "Untitled" } else { stem };
    if n <= 1 {
        format!("{stem} attachments")
    } else {
        format!("{stem} attachments {n}")
    }
}

/// Carry the draft's attachments next to the saved note `note`: a fresh
/// folder (`<stem> attachments`, numbered only at folder level when taken),
/// every file under its own name and path, each verified by digest. Only the
/// links Okilum resolves to one of them are rewritten. Nothing is written
/// when a link cannot be rewritten safely; a failed copy removes the folder
/// this call created and nothing else.
pub(super) fn carry_attachments(
    draft: &Path,
    note: &Path,
    found: &[Attachment],
    text: &str,
) -> Result<Carried, Refused> {
    if found.is_empty() {
        return Ok(Carried {
            folder: None,
            text: text.to_owned(),
        });
    }
    let destination = note
        .parent()
        .ok_or_else(|| Refused("The saved file has no folder.".into()))?;
    let stem = note
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    for n in 1..1000 {
        let name = attachments_folder_name(&stem, n);
        let folder = destination.join(&name);
        if folder.exists() {
            continue;
        }
        let rewritten = rewrite_draft_links(text, draft, &name, found)?;
        match std::fs::create_dir(&folder) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(Refused(format!("The attachments folder: {error}"))),
        }
        let copied = (|| -> anyhow::Result<()> {
            for attachment in found {
                anyhow::ensure!(
                    attachment
                        .relative
                        .components()
                        .all(|c| matches!(c, std::path::Component::Normal(_))),
                    "Unexpected attachment path"
                );
                let bytes = std::fs::read(draft.join(&attachment.relative))?;
                anyhow::ensure!(
                    digest(&bytes) == attachment.sha256,
                    "An attachment changed while saving"
                );
                let target = folder.join(&attachment.relative);
                std::fs::create_dir_all(
                    target
                        .parent()
                        .ok_or_else(|| anyhow::anyhow!("Attachment has no folder"))?,
                )?;
                write_new(&target, &bytes)?;
                anyhow::ensure!(
                    std::fs::read(&target)? == bytes,
                    "An attachment copy could not be verified"
                );
            }
            Ok(())
        })();
        return match copied {
            Ok(()) => Ok(Carried {
                folder: Some(folder),
                text: rewritten,
            }),
            Err(error) => {
                // Only this call's fresh folder: nothing else was there.
                let _ = std::fs::remove_dir_all(&folder);
                Err(Refused(format!("{error:#}")))
            }
        };
    }
    Err(Refused("No free name for the attachments folder.".into()))
}

/// One link destination in the source: where it is, how it is written and
/// the path Okilum reads from it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Destination {
    range: std::ops::Range<usize>,
    wiki: bool,
    angled: bool,
    /// From the parser: why this destination cannot be rewritten in place
    /// (a reference-style usage, an escaped target).
    reason: Option<String>,
    /// The destination as the parser decoded it.
    url: String,
    definition: bool,
}

/// Every link destination: inline links and images, wiki links and embeds
/// (frontmatter too) from the parser, and reference definitions outside
/// code, front matter and containers the scan does not understand.
fn destinations(text: &str) -> Vec<Destination> {
    let bytes = text.as_bytes();
    let angled = |range: &std::ops::Range<usize>| {
        range.start > 0 && bytes[range.start - 1] == b'<' && bytes.get(range.end) == Some(&b'>')
    };
    let mut found: Vec<Destination> = okilum_core::link_candidates::syntax::targets(text)
        .into_iter()
        .map(|target| Destination {
            angled: !target.wiki && target.reason.is_none() && angled(&target.range),
            range: target.range,
            wiki: target.wiki,
            reason: target.reason,
            url: target.text,
            definition: false,
        })
        .collect();
    let body = okilum_core::render::without_frontmatter(text);
    let offset = text.len() - body.len();
    let definition =
        regex::Regex::new(r"^ {0,3}\[(?:[^\]\\\n]|\\.)+\]:[ \t]*(<[^<>\n]*>|[^ \t\n]+)")
            .expect("valid reference definition pattern");
    let fence = regex::Regex::new(r"^ {0,3}(`{3,}|~{3,})").expect("valid fence pattern");
    let mut open: Option<(char, usize)> = None;
    let mut at = offset;
    for line in body.split_inclusive('\n') {
        let start = at;
        at += line.len();
        if let Some(found_fence) = fence.captures(line).and_then(|c| c.get(1)) {
            let mark = found_fence.as_str();
            let (c, n) = (mark.chars().next().unwrap_or('`'), mark.len());
            match open {
                None => open = Some((c, n)),
                Some((open_c, open_n)) if open_c == c && n >= open_n => open = None,
                Some(_) => {}
            }
            continue;
        }
        if open.is_some() {
            continue;
        }
        if let Some(dest) = definition.captures(line).and_then(|c| c.get(1)) {
            let raw = dest.as_str();
            let (range, is_angled) = if raw.starts_with('<') {
                (start + dest.start() + 1..start + dest.end() - 1, true)
            } else {
                (start + dest.start()..start + dest.end(), false)
            };
            found.push(Destination {
                url: text[range.clone()].to_owned(),
                range,
                wiki: false,
                angled: is_angled,
                reason: None,
                definition: true,
            });
        }
    }
    found.sort_by_key(|d| d.range.start);
    found
}

/// The path a destination names, as Okilum reads it, without its fragment.
fn destination_path(text: &str, destination: &Destination) -> String {
    let raw = &text[destination.range.clone()];
    let raw = raw.split('#').next().unwrap_or(raw);
    if destination.wiki {
        raw.trim().to_owned()
    } else {
        okilum_core::document_links::decode(raw)
    }
}

/// A destination as CommonMark reads it: backslash-escaped punctuation and
/// numeric or common named character references decoded.
fn commonmark_unescape(raw: &str) -> String {
    let entity = regex::Regex::new(
        r"&(?:#([0-9]{1,7})|#[xX]([0-9a-fA-F]{1,6})|(amp|lt|gt|quot|apos|nbsp));",
    )
    .expect("valid entity pattern");
    let escaped = regex::Regex::new(r"\\([!-/:-@\[-`{-~])").expect("valid escape pattern");
    let text = escaped.replace_all(raw, "$1");
    entity
        .replace_all(&text, |c: &regex::Captures| {
            let code = if let Some(n) = c.get(1) {
                n.as_str().parse::<u32>().ok()
            } else if let Some(h) = c.get(2) {
                u32::from_str_radix(h.as_str(), 16).ok()
            } else {
                Some(match &c[3] {
                    "amp" => '&' as u32,
                    "lt" => '<' as u32,
                    "gt" => '>' as u32,
                    "quot" => '"' as u32,
                    "apos" => '\'' as u32,
                    _ => 0xa0,
                })
            };
            code.and_then(char::from_u32)
                .map(String::from)
                .unwrap_or_else(|| c[0].to_owned())
        })
        .into_owned()
}

/// Rewrite the links of `text` that Okilum's resolver resolves to one of the
/// draft's attachments, so they name it below `prefix` (the attachments
/// folder beside the saved note). Wiki links stay wiki links; Markdown
/// destinations are percent-encoded or keep their angle brackets. Prose,
/// code, web URLs and links to anything else keep their bytes. Any link to
/// an attachment that cannot be rewritten safely refuses the whole Save.
pub(super) fn rewrite_draft_links(
    text: &str,
    draft: &Path,
    prefix: &str,
    found: &[Attachment],
) -> Result<String, Refused> {
    let vault = okilum_core::Vault::scan(draft)
        .map_err(|error| Refused(format!("The draft folder could not be read: {error:#}")))?;
    let resolver = okilum_core::document_links::IndexedResolver::new(&vault);
    let carried = |candidate: &str| {
        found
            .iter()
            .find(|attachment| {
                attachment
                    .relative
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join("/")
                    == candidate
            })
            .map(|attachment| attachment.relative.clone())
    };
    let all = destinations(text);
    let mut edits = Vec::new();
    let mut expected = Vec::new();
    let mut rewritten = Vec::new();
    // The attachment a destination resolves to, if any. Several candidates
    // or a file Save does not carry refuse.
    let resolve = |target: &str, wiki: bool| -> Result<Option<PathBuf>, Refused> {
        let resolved = resolver.resolve(target, wiki, FILE);
        match (resolved.status, resolved.candidates.as_slice()) {
            ("attachment", [candidate]) => match carried(candidate) {
                Some(relative) => Ok(Some(relative)),
                None if candidate == FILE => Ok(None),
                None => Err(Refused(format!(
                    "The link «{target}» points to a file of the draft that Save does not carry."
                ))),
            },
            ("attachment", candidates) if candidates.len() > 1 => Err(Refused(format!(
                "The link «{target}» could mean several attachments."
            ))),
            _ => Ok(None),
        }
    };
    for destination in &all {
        let raw = &text[destination.range.clone()];
        // Reference usages and escaped targets are checked below.
        if destination.reason.is_some() {
            expected.push(None);
            continue;
        }
        let Some(relative) = resolve(raw, destination.wiki)? else {
            // CommonMark decodes entities and backslash escapes in a
            // destination; Okilum's resolver reads it raw. Where the two
            // disagree about pointing at an attachment, refuse.
            let unescaped = commonmark_unescape(raw);
            if !destination.wiki && unescaped != raw && resolve(&unescaped, false)?.is_some() {
                return Err(Refused(format!(
                    "The link «{raw}» to an attachment is written in a way Okilum cannot rewrite safely."
                )));
            }
            expected.push(Some((
                destination.wiki,
                destination_path(text, destination),
            )));
            continue;
        };
        let path = std::iter::once(prefix.to_owned())
            .chain(
                relative
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned()),
            )
            .collect::<Vec<_>>()
            .join("/");
        let fragment = raw.find('#').map_or("", |at| &raw[at..]);
        let replacement = if destination.wiki {
            if path.contains(WIKI_UNSAFE) {
                return Err(Refused(format!(
                    "The attachment «{path}» cannot be named in a wiki link."
                )));
            }
            format!("{path}{fragment}")
        } else if destination.angled {
            if path.contains(['<', '>', '\n', '\r', '\\']) {
                return Err(Refused(format!(
                    "The attachment «{path}» cannot be named in this link."
                )));
            }
            format!("{path}{fragment}")
        } else {
            format!(
                "{}{fragment}",
                percent_encoding::utf8_percent_encode(&path, DESTINATION)
            )
        };
        if destination.definition {
            rewritten.push(relative);
        }
        expected.push(Some((destination.wiki, path)));
        edits.push((destination.range.clone(), replacement));
    }
    // A reference-style usage reaches its attachment through a definition,
    // which must have been rewritten above; one the scan did not find (in a
    // quote, a list…) or an escaped target the parser cannot round-trip
    // refuses the Save.
    for destination in all.iter().filter(|d| d.reason.is_some()) {
        // Read as written and as CommonMark decodes it (entities, escapes).
        let unescaped = commonmark_unescape(&destination.url);
        let resolved = match resolve(&destination.url, destination.wiki)? {
            Some(relative) => Some(relative),
            None if !destination.wiki && unescaped != destination.url => {
                resolve(&unescaped, false)?
            }
            None => None,
        };
        if let Some(relative) = resolved {
            if !rewritten.contains(&relative) {
                return Err(Refused(format!(
                    "The link «{}» to an attachment is written in a way Okilum cannot rewrite safely.",
                    &text[destination.range.clone()]
                )));
            }
        }
    }
    let mut out = text.to_owned();
    for (range, replacement) in edits.into_iter().rev() {
        out.replace_range(range, &replacement);
    }
    // Proof: the parser reads exactly the intended paths from the result,
    // and every reference usage is still one.
    let reread = destinations(&out);
    let same = reread.len() == expected.len()
        && reread.iter().zip(&expected).all(|(got, want)| match want {
            Some((wiki, path)) => {
                got.reason.is_none()
                    && (got.wiki, destination_path(&out, got)) == (*wiki, path.clone())
            }
            None => got.reason.is_some(),
        });
    if !same {
        return Err(Refused(
            "The rewritten links did not read back as intended.".into(),
        ));
    }
    Ok(out)
}

/// Bring the draft file to the saved text, so retirement can prove nothing in
/// the draft is newer. Written only when the disk still holds `expected` (what
/// this window last knew): anything else is an edit made elsewhere while
/// saving, kept as it is. Rechecked right before the atomic replace.
pub(super) fn sync_draft_file(
    folder: &Path,
    saved: &str,
    expected: &str,
) -> anyhow::Result<DraftFile> {
    let file = folder.join(FILE);
    let disk = std::fs::read_to_string(&file)?;
    if disk == saved {
        return Ok(DraftFile::Same);
    }
    if disk != expected {
        return Ok(DraftFile::ChangedElsewhere);
    }
    let temporary = folder.join(format!(".{FILE}.{}", uuid::Uuid::new_v4()));
    write_new(&temporary, saved.as_bytes())?;
    if std::fs::read_to_string(&file)? != expected {
        let _ = std::fs::remove_file(&temporary);
        return Ok(DraftFile::ChangedElsewhere);
    }
    if let Err(error) = std::fs::rename(&temporary, &file) {
        let _ = std::fs::remove_file(&temporary);
        return Err(error.into());
    }
    Ok(DraftFile::Written)
}

/// What `sync_draft_file` did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DraftFile {
    Same,
    Written,
    /// The draft changed outside this window: both are kept, nothing retired.
    ChangedElsewhere,
}

/// Retire a saved draft: only when no editor holds it and its current text and
/// attachments are exactly what was saved. Otherwise it stays an unsaved draft
/// in Recent; nothing is removed here. Only a draft folder of this session.
pub(super) fn retire(
    session: &Path,
    folder: &Path,
    saved_text: &str,
    saved_attachments: &[Attachment],
) -> anyhow::Result<bool> {
    let folder = draft_folder(session, folder)
        .ok_or_else(|| anyhow::anyhow!("Not an untitled draft of this session"))?;
    if live(session, &folder)
        || content(session, &folder) != Content::Text(saved_text.to_owned())
        || attachments(&folder).as_deref() != Some(saved_attachments)
    {
        return Ok(false);
    }
    let manifest = Manifest {
        text_sha256: digest(saved_text.as_bytes()),
        attachments: saved_attachments.to_vec(),
    };
    std::fs::write(folder.join(SAVED), serde_json::to_vec(&manifest)?)?;
    Ok(true)
}

impl Reader {
    /// The draft folder of the document on screen, if it is an untitled draft.
    pub(super) fn current_draft(&self) -> Option<PathBuf> {
        if !self.single_file || self.current_rel != FILE {
            return None;
        }
        draft_folder(self.session_directory.as_deref()?, &self.vault_root)
    }

    /// The folder a new draft remembers for Save: the open file's folder.
    fn draft_origin(&self) -> Option<PathBuf> {
        if let Some(folder) = self.current_draft() {
            return origin(&folder);
        }
        if self.vault_root.as_os_str().is_empty() {
            return None;
        }
        let document = self.vault_root.join(self.selected_file());
        let folder = if self.selected_file().is_empty() {
            self.vault_root.clone()
        } else {
            document.parent()?.to_path_buf()
        };
        folder.is_dir().then_some(folder)
    }

    /// «New untitled file»: a draft with no name, opened for typing. In a
    /// single-file window it replaces the file on screen (no tabs: Recent
    /// brings the file back); a vault window keeps its vault and the draft
    /// gets its own window.
    pub(super) fn new_untitled(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = self.session_directory.clone() else {
            reader_toast::error(
                "Draft storage is unavailable, so no untitled file was created.",
                window,
                cx,
            );
            return;
        };
        let origin = self.draft_origin();
        let in_place = self.single_file;
        let created = cx
            .background_executor()
            .spawn(async move { create(&session, origin.as_deref()).map(|file| (file, session)) });
        cx.spawn_in(window, async move |this, cx| {
            let created = created.await;
            let _ = this.update_in_own_window(cx, |this, window, cx| match created {
                Ok((file, session)) => {
                    if in_place {
                        this.close_recent_gallery(window, cx);
                        this.start_loading(
                            Opts {
                                open_path: Some(file),
                                single_file: true,
                                created_note: true,
                                ..Default::default()
                            },
                            window,
                            cx,
                        );
                    } else if let Err(error) = reader_open::open_window(
                        Opts {
                            open_path: Some(file),
                            single_file: true,
                            created_note: true,
                            session_directory: Some(session),
                            ..Default::default()
                        },
                        cx,
                    ) {
                        reader_open::show_error(error, cx);
                    }
                }
                Err(error) => {
                    diagnostic_error!("Could not create an untitled draft: {error:#}");
                    reader_toast::error("The untitled file couldn’t be created.", window, cx);
                }
            });
        })
        .detach();
    }

    /// Open a file Recent lists outside this window's vault: a draft or a file
    /// opened on its own. A single-file window switches to it; a vault window
    /// opens it in its own window (or focuses the window that has it).
    pub(super) fn open_recent_file(
        &mut self,
        file: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.single_file && self.vault_root.join(&self.current_rel) == file {
            return;
        }
        if self.single_file {
            self.start_loading(
                Opts {
                    open_path: Some(file),
                    single_file: true,
                    ..Default::default()
                },
                window,
                cx,
            );
        } else if let Err(error) = reader_open::open_window(
            Opts {
                open_path: Some(file),
                single_file: true,
                session_directory: self.session_directory.clone(),
                ..Default::default()
            },
            cx,
        ) {
            reader_open::show_error(error, cx);
        }
    }

    /// Another window of this app shows the same draft.
    fn draft_open_elsewhere(&self, folder: &Path, cx: &Context<Self>) -> bool {
        reader_open::root_open_elsewhere(folder, cx.entity_id(), cx)
    }

    /// Save on an untitled draft: choose its name and folder (proposed: the
    /// folder it was created from), write it and its attachments there
    /// without overwriting anything, show the saved file in this window and
    /// retire the draft once nothing in it is newer than what was saved.
    pub(super) fn save_draft_as(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(folder), Some(session)) = (self.current_draft(), self.session_directory.clone())
        else {
            return;
        };
        if self.draft_open_elsewhere(&folder, cx) {
            reader_toast::error(
                "This draft is also open in another window. Close it there, then Save here.",
                window,
                cx,
            );
            return;
        }
        // The newest text: this editor, else recovery ahead of the file.
        let contents = match self.source_text(cx) {
            Some(text) => text,
            None => match content(&session, &folder) {
                Content::Text(text) => text,
                Content::Unknown => {
                    reader_toast::error(
                        "This draft couldn’t be read completely, so it was not saved. It is kept.",
                        window,
                        cx,
                    );
                    return;
                }
            },
        };
        let directory = origin(&folder)
            .or_else(|| {
                home().map(|home| {
                    let documents = home.join("Documents");
                    if documents.is_dir() {
                        documents
                    } else {
                        home
                    }
                })
            })
            .unwrap_or_else(std::env::temp_dir);
        let name = suggested_name(&contents);
        let root = self.vault_root.clone();
        // Without an editor, the disk as Save found it is what this window knows.
        let disk_before = std::fs::read_to_string(folder.join(FILE)).unwrap_or_default();
        let picker = cx.prompt_for_new_path(&directory, Some(&name));
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(mut path))) = picker.await else {
                return;
            };
            if path.extension().is_none() {
                path.set_extension("md");
            }
            let _ = this.update_in_own_window(cx, |this, window, cx| {
                // The picker can outlive the draft on screen.
                if this.vault_root != root || this.current_draft().is_none() {
                    return;
                }
                if this.draft_open_elsewhere(&folder, cx) {
                    reader_toast::error(
                        "This draft is also open in another window. Close it there, then Save here.",
                        window,
                        cx,
                    );
                    return;
                }
                // Text typed while the picker was open is saved too. The
                // draft keeps its own text; only the saved copy may get links
                // rewritten to renamed attachments.
                let draft_text = this.source_text(cx).unwrap_or(contents);
                // What this window last knew on disk, before Save changes it.
                let expected = this.source_disk_base().unwrap_or(disk_before);
                let fail = |error: anyhow::Error, window: &mut Window, cx: &mut Context<Reader>| {
                    diagnostic_error!("Could not save the untitled draft: {error:#}");
                    reader_toast::error(
                        "The file couldn’t be saved there. Choose a new filename; existing files are never overwritten. The draft is kept.",
                        window,
                        cx,
                    );
                };
                let Some(found) = attachments(&folder) else {
                    fail(anyhow::anyhow!("draft attachments unreadable"), window, cx);
                    return;
                };
                if path.exists() {
                    return fail(anyhow::anyhow!("destination exists"), window, cx);
                }
                // Attachments go to a fresh folder beside the note under their
                // own names; only links Okilum resolves to them are rewritten.
                let carried = match carry_attachments(&folder, &path, &found, &draft_text) {
                    Ok(carried) => carried,
                    Err(Refused(reason)) => {
                        diagnostic_error!("Save of the untitled draft refused: {reason}");
                        reader_toast::error(
                            format!("Nothing was saved and the draft is kept. {reason}"),
                            window,
                            cx,
                        );
                        return;
                    }
                };
                if let Err(error) = reader_deleted_note::save_new_copy(&path, &carried.text) {
                    if let Some(created) = &carried.folder {
                        // Only the fresh folder this Save made.
                        let _ = std::fs::remove_dir_all(created);
                    }
                    return fail(error, window, cx);
                }
                // Its text is saved: the draft's editor has nothing left to
                // protect. The draft file gets the same text only while the
                // disk is still what this window knew, so the manifest below
                // can prove nothing in the draft is newer.
                this.source_saved_elsewhere(cx);
                this.remember_source_mode();
                this.editing = None;
                let retirable = match sync_draft_file(&folder, &draft_text, &expected) {
                    Ok(DraftFile::Same | DraftFile::Written) => true,
                    Ok(DraftFile::ChangedElsewhere) => {
                        reader_toast::error(
                            "The draft changed outside this window while saving. The saved file has this window’s text; the draft keeps the other changes and stays in Recent.",
                            window,
                            cx,
                        );
                        false
                    }
                    Err(error) => {
                        diagnostic_error!("Could not update the saved draft: {error:#}");
                        false
                    }
                };
                this.start_loading(
                    Opts {
                        open_path: Some(path.clone()),
                        single_file: true,
                        ..Default::default()
                    },
                    window,
                    cx,
                );
                let label = path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default();
                reader_toast::transient(format!("Saved as «{label}»"), window, cx);
                let history = this.session_directory.clone();
                if !retirable {
                    cx.notify();
                    return;
                }
                // The editor lock is released once its last journal write is
                // done; retire only then, and only if nothing changed.
                cx.spawn(async move |_, cx| {
                    for _ in 0..50 {
                        let (at_session, at_folder, text, found) =
                            (session.clone(), folder.clone(), draft_text.clone(), found.clone());
                        let outcome = cx
                            .background_executor()
                            .spawn(async move { retire(&at_session, &at_folder, &text, &found) })
                            .await;
                        match outcome {
                            Ok(true) => {
                                if let Some(history) = &history {
                                    let _ = reader_history::ReadingHistory::forget_root(
                                        history, &folder,
                                    );
                                }
                                return;
                            }
                            Ok(false) => {
                                cx.background_executor()
                                    .timer(Duration::from_millis(100))
                                    .await
                            }
                            Err(error) => {
                                diagnostic_error!("Could not retire the saved draft: {error:#}");
                                return;
                            }
                        }
                    }
                    diagnostic_error!("The saved draft changed or stayed open; it is kept");
                })
                .detach();
                cx.notify();
            });
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    use gpui::{size, TestAppContext, VisualTestContext};

    use okilum_core::file_editor::FileEditor;

    const LATER: u64 = EMPTY_DRAFT_LIFETIME * 2;

    #[test]
    fn draft_is_durable_listed_and_keeps_its_origin() {
        let temp = tempfile::tempdir().unwrap();
        let session = temp.path().join("state");
        let origin_folder = temp.path().join("docs");
        std::fs::create_dir_all(&origin_folder).unwrap();
        let file = create(&session, Some(&origin_folder)).unwrap();
        assert_eq!(file.file_name().unwrap(), FILE);
        assert!(file.is_file());
        let folder = file.parent().unwrap();
        assert_eq!(
            origin(folder).unwrap().canonicalize().unwrap(),
            origin_folder.canonicalize().unwrap()
        );
        assert!(draft_folder(&session, folder).is_some());
        // An untouched draft is not listed: nothing to protect yet.
        assert!(list(&session).is_empty());
        let text = "# Shopping list\n\nmilk\n";
        std::fs::write(&file, text).unwrap();
        let listed = list(&session);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].label, "Shopping list");
        assert_eq!(listed[0].file, file);
        assert!(!retire(&session, folder, "other text", &[]).unwrap());
        assert_eq!(
            list(&session).len(),
            1,
            "a mismatching Save retires nothing"
        );
        assert!(retire(&session, folder, text, &[]).unwrap());
        assert!(list(&session).is_empty(), "a saved draft leaves Recent");
        collect(&session, reader_sidebar::now());
        assert!(folder.exists(), "the window may still show it for a while");
        collect(&session, reader_sidebar::now() + SAVED_LIFETIME + 60);
        assert!(!folder.exists(), "a retired draft folder is removed later");
    }

    #[test]
    fn only_this_sessions_drafts_count_or_can_be_removed() {
        let temp = tempfile::tempdir().unwrap();
        let session = temp.path().join("state");
        let other = temp.path().join("notes");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join(FILE), "keep").unwrap();
        create(&session, None).unwrap();
        assert!(draft_folder(&session, &other).is_none());
        assert!(draft_folder(&session, &session.join(DIRECTORY)).is_none());
        assert!(retire(&session, &other, "keep", &[]).is_err());
        assert!(!other.join(SAVED).exists());
        assert!(
            other.join(FILE).is_file(),
            "a non-draft folder is never removed"
        );
    }

    #[test]
    fn untouched_drafts_expire_but_typed_ones_stay() {
        let temp = tempfile::tempdir().unwrap();
        let session = temp.path().join("state");
        let empty = create(&session, None).unwrap();
        let typed = create(&session, None).unwrap();
        std::fs::write(&typed, "words").unwrap();
        collect(&session, reader_sidebar::now());
        assert!(empty.exists(), "a fresh empty draft may still be on screen");
        collect(&session, reader_sidebar::now() + LATER);
        assert!(
            !empty.exists(),
            "an untouched draft is collected after a day"
        );
        assert!(typed.exists(), "a draft with text is never collected");
    }

    /// Review P1-1: a crash after the recovery journal was written but before
    /// the first autosave leaves `Untitled.md` empty. Recovery holds the text:
    /// Recent lists the draft under it, Save would use it, cleanup keeps it.
    #[test]
    fn crash_before_first_autosave_keeps_the_draft_from_recovery() {
        let temp = tempfile::tempdir().unwrap();
        let session = temp.path().join("state");
        let file = create(&session, None).unwrap();
        let folder = file.parent().unwrap().to_path_buf();
        {
            let mut editor = FileEditor::open(&file, &recovery_directory(&session)).unwrap();
            editor
                .queue_text("Before autosave\nmilk\n".into())
                .persist()
                .unwrap();
            // Crash: the editor goes away without saving the file.
        }
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "",
            "never autosaved"
        );
        assert_eq!(
            content(&session, &folder),
            Content::Text("Before autosave\nmilk\n".into())
        );
        let listed = list(&session);
        assert_eq!(listed.len(), 1, "Recent lists the recovered draft");
        assert_eq!(listed[0].label, "Before autosave");
        collect(&session, reader_sidebar::now() + LATER);
        assert!(file.exists(), "recovery data keeps the draft folder");
    }

    /// Review P1-2 / re-review 3: pasted images live below the draft folder.
    /// Save carries them into a fresh `<note> attachments` folder (numbered
    /// only at folder level when taken) under their own names, verified;
    /// attachments alone keep a draft listed and stop cleanup.
    #[test]
    fn attachments_keep_the_draft_and_travel_under_their_own_names() {
        let temp = tempfile::tempdir().unwrap();
        let session = temp.path().join("state");
        let file = create(&session, None).unwrap();
        let folder = file.parent().unwrap().to_path_buf();
        let image = Path::new("@Assets/Images/my pic (1).png");
        std::fs::create_dir_all(folder.join(image).parent().unwrap()).unwrap();
        std::fs::write(folder.join(image), b"draft image").unwrap();
        assert_eq!(
            list(&session).len(),
            1,
            "an attachment alone is unsaved work"
        );
        collect(&session, reader_sidebar::now() + LATER);
        assert!(folder.join(image).exists(), "cleanup keeps attachments");

        let text = "![[my pic (1).png]]\n";
        std::fs::write(&file, text).unwrap();
        let docs = temp.path().join("docs");
        std::fs::create_dir_all(docs.join("Note attachments")).unwrap();
        std::fs::write(docs.join("Note attachments/keep.txt"), b"theirs").unwrap();
        let found = attachments(&folder).unwrap();
        assert_eq!(found.len(), 1);
        let carried = carry_attachments(&folder, &docs.join("Note.md"), &found, text).unwrap();
        let fresh = docs.join("Note attachments 2");
        assert_eq!(carried.folder.as_deref(), Some(fresh.as_path()));
        assert_eq!(std::fs::read(fresh.join(image)).unwrap(), b"draft image");
        assert_eq!(
            carried.text,
            "![[Note attachments 2/@Assets/Images/my pic (1).png]]\n"
        );
        assert_eq!(
            std::fs::read(docs.join("Note attachments/keep.txt")).unwrap(),
            b"theirs",
            "a taken folder is never touched"
        );

        // An attachment added after Save was prepared keeps the draft.
        std::fs::write(folder.join("@Assets/Images/late.png"), b"late").unwrap();
        assert!(!retire(&session, &folder, text, &found).unwrap());
        assert_eq!(list(&session).len(), 1);
        std::fs::remove_file(folder.join("@Assets/Images/late.png")).unwrap();
        assert!(retire(&session, &folder, text, &found).unwrap());
        collect(&session, reader_sidebar::now() + SAVED_LIFETIME + 60);
        assert!(
            !folder.exists(),
            "removed only once everything was carried over"
        );
    }

    /// Review P1-3: Save in one window must not retire a draft another window
    /// still edits, and newer edits after Save revive it; cleanup keeps it.
    #[test]
    fn a_live_editor_or_newer_edits_keep_a_saved_draft() {
        let temp = tempfile::tempdir().unwrap();
        let session = temp.path().join("state");
        let file = create(&session, None).unwrap();
        let folder = file.parent().unwrap().to_path_buf();
        std::fs::write(&file, "v1").unwrap();
        let mut other = FileEditor::open(&file, &recovery_directory(&session)).unwrap();
        assert!(
            live(&session, &folder),
            "positive control: the lock is seen"
        );
        assert!(
            !retire(&session, &folder, "v1", &[]).unwrap(),
            "another window still edits it"
        );
        drop(other);
        assert!(!live(&session, &folder));
        assert!(retire(&session, &folder, "v1", &[]).unwrap());
        // The other window comes back and keeps typing after that Save.
        other = FileEditor::open(&file, &recovery_directory(&session)).unwrap();
        collect(&session, reader_sidebar::now() + SAVED_LIFETIME + 60);
        assert!(file.exists(), "a live editor keeps the folder");
        other.queue_text("v2".into()).persist().unwrap();
        drop(other);
        assert_eq!(list(&session).len(), 1, "newer edits revive the draft");
        collect(&session, reader_sidebar::now() + SAVED_LIFETIME + 60);
        assert!(file.exists(), "newer recovery text keeps the folder");
        std::fs::write(&file, "v3").unwrap();
        collect(&session, reader_sidebar::now() + LATER);
        assert!(file.exists(), "a newer file keeps the folder");
    }

    fn draft_with(files: &[&str]) -> (tempfile::TempDir, PathBuf, Vec<Attachment>) {
        let temp = tempfile::tempdir().unwrap();
        let folder = temp.path().join("draft");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join(FILE), "").unwrap();
        for file in files {
            let path = folder.join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, file.as_bytes()).unwrap();
        }
        let found = attachments(&folder).unwrap();
        (temp, folder, found)
    }

    /// Re-review 3: only links Okilum's resolver resolves to a draft
    /// attachment change: inline images (percent-encoded or in angle
    /// brackets, names with spaces and parentheses), reference definitions,
    /// wiki embeds with alias, root and subfolder files of the same name.
    /// Prose, code, web URLs and the reference usage keep their bytes.
    #[test]
    fn draft_links_are_rewritten_where_they_resolve_to_attachments() {
        let (_temp, folder, found) =
            draft_with(&["a.png", "sub/a.png", "my pic (1).png", "ref pic.png"]);
        let text = "\
I named it a.png yesterday, see `![[a.png]]`.\n\
![[a.png]] and ![[sub/a.png|small]]\n\
![x](my%20pic%20%281%29.png) and ![y](<my pic (1).png>)\n\
![z](sub/a.png \"title\")\n\
![r][pic]\n\
[web](https://example.com/a.png)\n\
\n\
[pic]: <ref pic.png> \"Ref\"\n";
        let rewritten = rewrite_draft_links(text, &folder, "Note attachments", &found).unwrap();
        assert_eq!(
            rewritten,
            "\
I named it a.png yesterday, see `![[a.png]]`.\n\
![[Note attachments/a.png]] and ![[Note attachments/sub/a.png|small]]\n\
![x](Note%20attachments/my%20pic%20%281%29.png) and ![y](<Note attachments/my pic (1).png>)\n\
![z](Note%20attachments/sub/a.png \"title\")\n\
![r][pic]\n\
[web](https://example.com/a.png)\n\
\n\
[pic]: <Note attachments/ref pic.png> \"Ref\"\n"
        );
        // A plain reference definition is percent-encoded like inline ones.
        let plain = "![r][pic]\n\n[pic]: my%20pic%20%281%29.png\n";
        assert_eq!(
            rewrite_draft_links(plain, &folder, "Note attachments", &found).unwrap(),
            "![r][pic]\n\n[pic]: Note%20attachments/my%20pic%20%281%29.png\n"
        );
    }

    /// Re-review 3: a link to an attachment that cannot be rewritten safely
    /// refuses the Save before anything is written; the draft is kept.
    #[test]
    fn unsafe_attachment_links_refuse_before_writing() {
        let (temp, folder, found) = draft_with(&["a.png"]);
        let docs = temp.path().join("docs");
        std::fs::create_dir_all(&docs).unwrap();
        for text in [
            // The definition sits in a quote the scan does not rewrite.
            "![q][qq]\n\n> [qq]: a.png\n",
            // An entity-encoded destination the parser cannot round-trip.
            "![e](a&#46;png)\n",
        ] {
            let refused = carry_attachments(&folder, &docs.join("Note.md"), &found, text);
            assert!(refused.is_err(), "{text:?} must refuse: {refused:?}");
            assert!(
                std::fs::read_dir(&docs).unwrap().next().is_none(),
                "nothing was written for {text:?}"
            );
        }
        // Positive control: the same attachment linked plainly is carried.
        let carried =
            carry_attachments(&folder, &docs.join("Note.md"), &found, "![[a.png]]\n").unwrap();
        assert_eq!(carried.text, "![[Note attachments/a.png]]\n");
    }

    /// Re-review P1-3: cleanup holds the editor lock from classification
    /// through removal, so no editor can take the draft in between.
    #[test]
    fn cleanup_holds_ownership_until_the_folder_is_gone() {
        let temp = tempfile::tempdir().unwrap();
        let session = temp.path().join("state");
        let file = create(&session, None).unwrap();
        let folder = file.parent().unwrap().to_path_buf();
        let recovery = recovery_directory(&session);
        let attempts = Rc::new(std::cell::RefCell::new(Vec::new()));
        BEFORE_REMOVAL.with(|hook| {
            let (attempts, recovery) = (attempts.clone(), recovery.clone());
            *hook.borrow_mut() = Some(Box::new(move |folder: &Path| {
                // Another window tries to open the draft for editing now.
                let refused = FileEditor::open(&folder.join(FILE), &recovery)
                    .err()
                    .map(|error| format!("{error:#}"));
                attempts.borrow_mut().push(refused);
            }));
        });
        collect(&session, reader_sidebar::now() + LATER);
        BEFORE_REMOVAL.with(|hook| *hook.borrow_mut() = None);
        let attempts = attempts.borrow();
        assert_eq!(attempts.len(), 1, "positive control: removal was reached");
        assert!(
            attempts[0]
                .as_deref()
                .is_some_and(|error| error.contains(okilum_core::file_editor::BEING_EDITED)),
            "no editor got in while cleanup decided: {attempts:?}"
        );
        assert!(!folder.exists());
    }

    /// Real time for the watcher and writers, test time for the Reader's
    /// timers, until `done` holds or ~15 s are up.
    fn settle(
        visual: &mut VisualTestContext,
        reader: &Entity<Reader>,
        what: &str,
        done: impl Fn(&Reader, &App) -> bool,
    ) {
        for _ in 0..150 {
            std::thread::sleep(Duration::from_millis(100));
            visual.executor().advance_clock(Duration::from_millis(300));
            visual.run_until_parked();
            if reader.read_with(visual, |reader, cx| done(reader, cx)) {
                return;
            }
        }
        let state = reader.read_with(visual, |r, _| {
            format!(
                "root={:?} rel={:?} single={} editing={} loading={:?} notice={:?}",
                r.vault_root,
                r.current_rel,
                r.single_file,
                r.editing.is_some(),
                r.loading.as_ref().map(|l| l.active),
                r.link_notice
            )
        });
        panic!("not reached after 15 s: {what}; {state}");
    }

    fn open_single<'a>(
        cx: &'a mut TestAppContext,
        file: &Path,
        session: &Path,
    ) -> (Entity<Reader>, &'a mut VisualTestContext) {
        let mut entity = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let reader = cx.new(|cx| {
                Reader::new(
                    Opts {
                        open_path: Some(file.to_path_buf()),
                        session_directory: Some(session.to_path_buf()),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            entity = Some(reader.clone());
            Root::new(reader, window, cx)
        });
        visual.simulate_resize(size(px(1400.), px(900.)));
        visual.run_until_parked();
        (entity.unwrap(), visual)
    }

    /// #1528 end to end: New untitled file needs no name; its text reaches
    /// Okilum's own storage while typing, so a kill loses nothing; after a
    /// restart Recent lists it with an «Unsaved» badge; Save proposes the
    /// folder it came from and the saved file replaces the draft.
    #[gpui::test]
    fn untitled_draft_survives_a_kill_and_saves_beside_its_source(cx: &mut TestAppContext) {
        cx.update(|cx| {
            cx.set_reduce_motion(true);
            crate::test_init(cx);
            bind_keys(cx);
        });
        let temp = tempfile::tempdir().unwrap();
        let docs = temp.path().join("docs");
        std::fs::create_dir_all(&docs).unwrap();
        let docs = docs.canonicalize().unwrap();
        std::fs::write(docs.join("Start.md"), "# Start\n").unwrap();
        let session = temp.path().join("state");

        let (reader, visual) = open_single(cx, &docs.join("Start.md"), &session);
        reader.update_in(visual, |r, window, cx| {
            assert!(r.single_file, "positive control: a file opened on its own");
            assert_eq!(r.current_rel, "Start.md");
            assert!(r.current_draft().is_none());
            r.new_untitled(window, cx);
        });
        settle(visual, &reader, "the draft is open for typing", |r, _| {
            r.current_draft().is_some() && r.editing.is_some()
        });
        let draft = reader.read_with(visual, |r, _| r.vault_root.join(FILE));
        assert!(draft.starts_with(session.canonicalize().unwrap()));
        assert_eq!(std::fs::read_to_string(&draft).unwrap(), "");
        visual.simulate_input("Shopping list\nmilk");
        visual.run_until_parked();
        reader.read_with(visual, |r, cx| {
            // Positive control: the keystrokes reached the draft's editor.
            assert_eq!(r.source_text(cx).as_deref(), Some("Shopping list\nmilk"));
        });
        // Nothing explicit: autosave alone must put the text on disk.
        for _ in 0..150 {
            if std::fs::read_to_string(&draft).unwrap_or_default() == "Shopping list\nmilk" {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
            visual.executor().advance_clock(Duration::from_millis(300));
            visual.run_until_parked();
        }
        assert_eq!(
            std::fs::read_to_string(&draft).unwrap(),
            "Shopping list\nmilk",
            "the text is durable before any close: a kill now loses nothing"
        );
        // Kill: the window goes away without Save or any question.
        reader.update_in(visual, |_, window, _| window.remove_window());
        visual.run_until_parked();
        // A killed process holds no editor lock: release this one's too.
        drop(reader);
        visual.run_until_parked();
        // The draft had a pasted image, which lives below the draft folder
        // until Save, and an embed of it.
        let folder = draft.parent().unwrap().to_path_buf();
        let image = Path::new("@Assets/Images/Pasted image 1.png");
        std::fs::create_dir_all(folder.join(image).parent().unwrap()).unwrap();
        std::fs::write(folder.join(image), b"pasted").unwrap();
        std::fs::write(&draft, "Shopping list\nmilk\n![[Pasted image 1.png]]\n").unwrap();

        // Restart on the original file.
        let (reader, visual) = open_single(cx, &docs.join("Start.md"), &session);
        reader.update_in(visual, |r, window, cx| r.toggle_recent_gallery(window, cx));
        let draft_path = draft.to_string_lossy().into_owned();
        settle(visual, &reader, "Recent lists the draft", |r, _| {
            r.recent_gallery_entries()
                .iter()
                .any(|entry| entry.path == draft_path && entry.unsaved)
        });
        reader.read_with(visual, |r, _| {
            let entries = r.recent_gallery_entries();
            assert_eq!(entries[0].path, draft_path, "unsaved drafts lead");
            assert_eq!(entries[0].label, "Shopping list");
            let start = docs.join("Start.md").to_string_lossy().into_owned();
            assert!(
                entries
                    .iter()
                    .any(|entry| entry.path == start && !entry.unsaved),
                "the file on screen is listed too: {entries:?}"
            );
        });
        visual.update(|window, _| window.refresh());
        visual.run_until_parked();
        assert!(visual.debug_bounds("recent-gallery-overlay").is_some());
        assert!(
            visual.debug_bounds("recent-gallery-unsaved-0").is_some(),
            "the draft's card carries the Unsaved badge"
        );
        // The gallery opened on the file on screen; the draft card arrived
        // from the background read before it, and selection stayed put.
        reader.read_with(visual, |r, cx| {
            let start = docs.join("Start.md").to_string_lossy().into_owned();
            assert_eq!(r.recent_switcher.gallery_selection(cx), Some(start));
        });
        visual.simulate_keystrokes("left");
        visual.run_until_parked();
        reader.read_with(visual, |r, cx| {
            assert_eq!(
                r.recent_switcher.gallery_selection(cx).as_deref(),
                Some(draft_path.as_str())
            );
        });
        visual.simulate_keystrokes("enter");
        settle(visual, &reader, "the draft is back on screen", |r, _| {
            r.current_draft().is_some() && r.note_source.contains("milk")
        });

        // Save: the dialog starts in the folder the draft came from.
        reader.update_in(visual, |r, window, cx| {
            r.focus_handle.focus(window, cx);
        });
        visual.simulate_keystrokes("secondary-s");
        visual.run_until_parked();
        assert!(visual.did_prompt_for_new_path(), "Save asks where");
        let proposed = Rc::new(std::cell::RefCell::new(None));
        let saved = docs.join("Shopping list.md");
        visual.simulate_new_path_selection({
            let (proposed, saved) = (proposed.clone(), saved.clone());
            move |directory| {
                *proposed.borrow_mut() = Some(directory.to_path_buf());
                Some(saved)
            }
        });
        settle(
            visual,
            &reader,
            "the saved file replaces the draft",
            |r, _| r.current_rel == "Shopping list.md" && r.current_draft().is_none(),
        );
        assert_eq!(proposed.borrow().as_deref(), Some(docs.as_path()));
        assert_eq!(
            std::fs::read_to_string(&saved).unwrap(),
            "Shopping list\nmilk\n![[Shopping list attachments/@Assets/Images/Pasted image 1.png]]\n",
            "the embed points into the attachments folder"
        );
        assert_eq!(
            std::fs::read(docs.join("Shopping list attachments").join(image)).unwrap(),
            b"pasted",
            "the attachment travels with the saved file under its own name"
        );
        settle(visual, &reader, "the saved draft leaves Recent", |_, _| {
            list(&session).is_empty()
        });
        collect(&session, reader_sidebar::now());
        assert!(folder.join(image).exists(), "kept for a while after Save");
        collect(&session, reader_sidebar::now() + SAVED_LIFETIME + 60);
        assert!(!folder.exists(), "removed once retired and expired");
        reader.read_with(visual, |r, _| assert!(r.single_file));
    }

    /// Re-review P1-2: an edit to the draft made outside this window while
    /// the Save dialog is open is never overwritten. Both are kept: the saved
    /// file has this window's text, the draft keeps the other edit and stays
    /// an unsaved draft in Recent.
    #[gpui::test]
    fn an_edit_elsewhere_during_save_keeps_both(cx: &mut TestAppContext) {
        cx.update(|cx| {
            cx.set_reduce_motion(true);
            crate::test_init(cx);
            bind_keys(cx);
        });
        let temp = tempfile::tempdir().unwrap();
        let docs = temp.path().join("docs");
        std::fs::create_dir_all(&docs).unwrap();
        let docs = docs.canonicalize().unwrap();
        let session = temp.path().join("state");
        let draft = create(&session, Some(&docs)).unwrap();
        std::fs::write(&draft, "Mine\n").unwrap();
        let (reader, visual) = open_single(cx, &draft, &session);
        settle(visual, &reader, "the draft is on screen", |r, _| {
            r.current_draft().is_some() && r.note_source.contains("Mine")
        });
        reader.update_in(visual, |r, window, cx| r.focus_handle.focus(window, cx));
        visual.simulate_keystrokes("secondary-s");
        visual.run_until_parked();
        assert!(visual.did_prompt_for_new_path(), "Save asks where");
        // Meanwhile something else writes the draft file.
        std::fs::write(&draft, "Theirs\n").unwrap();
        let saved = docs.join("Mine.md");
        visual.simulate_new_path_selection({
            let saved = saved.clone();
            move |_| Some(saved)
        });
        settle(visual, &reader, "the saved file is on screen", |r, _| {
            r.current_rel == "Mine.md"
        });
        assert_eq!(std::fs::read_to_string(&saved).unwrap(), "Mine\n");
        assert_eq!(
            std::fs::read_to_string(&draft).unwrap(),
            "Theirs\n",
            "the other edit is never overwritten"
        );
        for _ in 0..20 {
            std::thread::sleep(Duration::from_millis(50));
            visual.executor().advance_clock(Duration::from_millis(300));
            visual.run_until_parked();
        }
        let listed = list(&session);
        assert_eq!(listed.len(), 1, "the draft stays unsaved in Recent");
        assert_eq!(listed[0].label, "Theirs");
        collect(&session, reader_sidebar::now() + LATER);
        assert!(draft.exists());
    }

    /// In a single-file window New note proposes the open file's folder, not
    /// the tree selection (#1528); explicit destinations and New Folder keep
    /// the tree's.
    #[gpui::test]
    fn single_file_new_note_goes_beside_the_open_file(cx: &mut TestAppContext) {
        cx.update(|cx| {
            cx.set_reduce_motion(true);
            crate::test_init(cx);
            bind_keys(cx);
        });
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("folder");
        std::fs::create_dir_all(root.join("Child")).unwrap();
        std::fs::write(root.join("Start.md"), "# Start\n").unwrap();
        std::fs::write(root.join("Child/Next.md"), "# Next\n").unwrap();
        let (reader, visual) = open_single(cx, &root.join("Start.md"), &temp.path().join("state"));
        reader.update_in(visual, |r, window, cx| {
            assert!(r.single_file);
            r.load_quick_folder("Child".into(), cx);
            r.open_note("Child/Next.md", None, window, cx);
        });
        visual.run_until_parked();
        reader.update_in(visual, |r, window, cx| {
            assert_eq!(r.selected_file(), "Child/Next.md");
            // A real, unrelated selection is the positive control: without
            // the fix New note proposes the root instead of Child.
            r.tree.cursor = Some("Start.md".into());
            assert_eq!(r.tree.selected_creation_folder().as_deref(), Some(""));
            r.new_note(None, window, cx);
            assert_eq!(r.creation.as_ref().unwrap().folder, "Child");
            let input = r.creation.as_ref().unwrap().input.clone();
            input.update(cx, |input, cx| input.set_value("Sibling", window, cx));
        });
        visual.simulate_keystrokes("enter");
        visual.run_until_parked();
        assert!(root.join("Child/Sibling.md").is_file());
        assert!(!root.join("Sibling.md").exists());
        reader.update_in(visual, |r, window, cx| {
            assert!(r.single_file, "creating a file never enrolls a vault");
            r.new_note(Some(""), window, cx);
            assert_eq!(r.creation.as_ref().unwrap().folder, "");
            r.cancel_creation(window, cx);
            r.tree.cursor = Some("Start.md".into());
            r.new_folder(None, window, cx);
            assert_eq!(r.creation.as_ref().unwrap().folder, "");
            r.cancel_creation(window, cx);
        });
    }

    #[test]
    fn names_and_labels_come_from_the_first_line() {
        assert_eq!(label("\n\n## Plan: Q4 \nrest").as_deref(), Some("Plan: Q4"));
        assert_eq!(label("   \n"), None);
        assert_eq!(suggested_name("## Plan: Q4/5?\n"), "Plan- Q4-5-.md");
        assert_eq!(suggested_name(""), "Untitled.md");
        assert_eq!(suggested_name("..."), "Untitled.md");
        let long = "x".repeat(100);
        assert_eq!(suggested_name(&long), format!("{}.md", "x".repeat(60)));
    }
}

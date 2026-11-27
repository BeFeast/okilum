//! Source-bound link presentation (non-Reader core). The `impl Reader`
//! preparation jobs stay in `okilum-shell::prepared_links` (#3235, epic #3241).
use crate::prelude::*;
use okilum_core::document_links::{self, prepared::LinkState};
use std::collections::BTreeMap;

pub type States = Arc<BTreeMap<String, LinkState>>;
pub type SnippetLinks = Vec<(std::ops::Range<usize>, String)>;

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    static PAINTED_LINKS: std::cell::RefCell<Option<Vec<(String, bool, bool)>>> = const { std::cell::RefCell::new(None) };
}

#[cfg(any(test, feature = "test-support"))]
pub fn start_painted_links_trace() {
    PAINTED_LINKS.with(|trace| *trace.borrow_mut() = Some(Vec::new()));
}

#[cfg(any(test, feature = "test-support"))]
pub fn painted_links_snapshot() -> Vec<(String, bool, bool)> {
    PAINTED_LINKS.with(|trace| trace.borrow().clone().unwrap_or_default())
}

#[cfg(any(test, feature = "test-support"))]
pub fn clear_painted_links_trace() {
    PAINTED_LINKS.with(|trace| {
        if let Some(list) = trace.borrow_mut().as_mut() {
            list.clear();
        }
    });
}

#[cfg(any(test, feature = "test-support"))]
pub fn stop_painted_links_trace() {
    PAINTED_LINKS.with(|trace| *trace.borrow_mut() = None);
}

/// External protocol links have a visible destination, never an internal-note guess.
pub fn external_tooltip(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    match parsed.scheme() {
        "http" | "https" | "ftp" | "mailto" | "tel" => {
            // Display only: link targets and open/copy actions keep authored bytes.
            // Keep controls, invisible formatting (including bidi overrides),
            // and Unicode line separators escaped in the displayed address.
            use std::fmt::Write as _;
            static HIDDEN: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
                regex::Regex::new(r"[\p{Cc}\p{Cf}\p{Zl}\p{Zp}]").unwrap()
            });
            let decoded = document_links::decode(url);
            let shown = HIDDEN.replace_all(&decoded, |matched: &regex::Captures<'_>| {
                let mut escaped = String::new();
                for byte in matched[0].as_bytes() {
                    write!(escaped, "%{byte:02X}").expect("writing a String cannot fail");
                }
                escaped
            });
            Some(match parsed.host_str() {
                Some(host) => format!("{host}\n{shown}"),
                None => shown.into_owned(),
            })
        }
        _ => None,
    }
}

/// Preserve authored-link identity while stripping snippet Markdown. Markers
/// are temporary and collision-free; they never enter the displayed string.
pub fn snippet_links(source: &str, plain: &str) -> SnippetLinks {
    let markers: Vec<char> = ('\u{e000}'..='\u{f8ff}')
        .filter(|c| !source.contains(*c))
        .take(2)
        .collect();
    if markers.len() != 2 {
        return Vec::new();
    }
    let links: Vec<_> = okilum_core::document_links::parse(source)
        .into_iter()
        .filter(|l| !l.wiki && external_tooltip(&l.target).is_some())
        .collect();
    let mut marked = source.to_owned();
    for link in links.iter().rev() {
        marked.insert(link.range.end, markers[1]);
        marked.insert(link.range.start, markers[0]);
    }
    let stripped = okilum_core::render::strip_inline_markdown(&marked);
    let mut unmarked = String::new();
    let mut ranges = Vec::new();
    let mut start = None;
    for ch in stripped.chars() {
        if ch == markers[0] {
            start = Some(unmarked.len());
        } else if ch == markers[1] {
            if let Some(start) = start.take() {
                ranges.push(start..unmarked.len());
            }
        } else {
            unmarked.push(ch);
        }
    }
    if unmarked != okilum_core::render::strip_inline_markdown(source) || ranges.len() != links.len()
    {
        return Vec::new();
    }
    ranges
        .into_iter()
        .zip(links)
        .filter_map(|(range, link)| {
            // Truncation keeps a prefix; do not turn a partial label/ellipsis into
            // a clickable destination or decorate a separately formatted property.
            (plain.get(range.clone()) == unmarked.get(range.clone()) && range.end <= plain.len())
                .then_some((range, link.target))
        })
        .collect()
}

pub fn decorate_snippet(
    mut text: String,
    mut highlight: Option<std::ops::Range<usize>>,
    links: SnippetLinks,
) -> (String, Option<std::ops::Range<usize>>, SnippetLinks) {
    let original_highlight = highlight.clone();
    let mut decorated = Vec::new();
    let mut added = 0;
    for (range, url) in links {
        // Destination marks retain their established muted appearance.
        if original_highlight
            .as_ref()
            .is_some_and(|h| h.start < range.end && range.start < h.end)
        {
            continue;
        }
        let end = range.end + added;
        text.insert_str(end, " ↗");
        if let Some(h) = &mut highlight {
            if h.start >= end {
                h.start += " ↗".len();
                h.end += " ↗".len();
            }
        }
        decorated.push((range.start + added..end + " ↗".len(), url));
        added += " ↗".len();
    }
    (text, highlight, decorated)
}

pub fn presentation(
    url: &str,
    states: &BTreeMap<String, LinkState>,
) -> gpui_component::text::LinkPresentation {
    use gpui_component::text::LinkPresentation;
    if let Some(tooltip) = external_tooltip(url) {
        return LinkPresentation {
            external: true,
            tooltip: Some(tooltip.into()),
            ..Default::default()
        };
    }
    let fallback = LinkState::unknown();
    let state = states.get(url).unwrap_or(&fallback);
    let mut style = gpui::HighlightStyle::default();
    if state.status.is_missing() {
        style.color = Some(brand::reader_palette_current().missing_link);
        style.underline = Some(gpui::UnderlineStyle {
            thickness: px(1.),
            wavy: true,
            ..Default::default()
        });
    }
    let presentation = LinkPresentation {
        style,
        tooltip: Some(
            match state
                .action_url
                .as_deref()
                .and_then(|u| u.strip_prefix("okilum://missing-file/"))
            {
                Some(path) => format!("File not found\n{}", document_links::decode(path)).into(),
                None => state.reason.clone().into(),
            },
        ),
        inert: state.status.is_missing()
            && state.status != document_links::prepared::LinkStatus::MissingFile,
        ..Default::default()
    };
    #[cfg(any(test, feature = "test-support"))]
    PAINTED_LINKS.with(|painted| {
        if let Some(painted) = &mut *painted.borrow_mut() {
            painted.push((
                url.into(),
                presentation.inert,
                presentation
                    .style
                    .underline
                    .is_some_and(|underline| underline.wavy),
            ));
        }
    });
    presentation
}

pub struct PreparedDocument {
    pub canonical_source: Option<Arc<str>>,
    pub encoding_damaged: bool,
    /// Shipped content with no on-disk note, even when a view path is assigned.
    pub built_in: bool,
    pub source: String,
    pub original: Option<String>,
    pub identities: Vec<document_links::prepared::LinkIdentity>,
    /// Leading frontmatter YAML for the Properties view (#386).
    pub frontmatter: Option<String>,
    /// The «On this page» headings of `source`, when they were prepared off
    /// the UI thread; a 10 MiB note takes over 100 ms to scan (#3073).
    pub outline: Option<Vec<document_links::HeadingEntry>>,
}

impl From<okilum_core::render::ReaderDocument> for PreparedDocument {
    fn from(document: okilum_core::render::ReaderDocument) -> Self {
        Self {
            canonical_source: (!document.encoding_damaged).then_some(document.canonical_source),
            encoding_damaged: document.encoding_damaged,
            built_in: false,
            source: document.rendered,
            original: Some(document.original_body),
            identities: document.links,
            frontmatter: document.frontmatter,
            outline: None,
        }
    }
}

impl PreparedDocument {
    pub fn html(vault: &Vault, rel: &str, source: Option<String>) -> anyhow::Result<Self> {
        let source = match source {
            Some(text) => okilum_core::vault::DisplayNote {
                text,
                encoding_damaged: vault.note_encoding_damaged(rel),
            },
            None => vault.read_note_display(rel)?,
        };
        Ok(Self {
            encoding_damaged: source.encoding_damaged,
            built_in: false,
            source: okilum_core::render::render_html_from_source(
                vault,
                rel,
                &source.text,
                "InspiredGitHub",
            )?,
            canonical_source: None,
            original: None,
            identities: Vec::new(),
            frontmatter: None,
            outline: None,
        })
    }
}

pub fn managed_states(preview: &serde_json::Value, current: bool) -> States {
    let mut states = BTreeMap::new();
    if !current || preview["prepared_links_version"] != 1 {
        return Arc::new(states);
    }
    if let Some(links) = preview["links"].as_array() {
        for link in links {
            if let (Some(url), Ok(state)) = (
                link["url"].as_str(),
                serde_json::from_value::<LinkState>(link["prepared"].clone()),
            ) {
                states
                    .entry(url.to_owned())
                    .and_modify(|old| {
                        if old != &state {
                            *old = LinkState::unknown();
                        }
                    })
                    .or_insert(state);
            }
        }
    }
    Arc::new(states)
}

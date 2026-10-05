//! Obsidian callouts: `> [!type]±? Title` followed by a quoted body (#46).
//!
//! The Markdown pipeline leaves a callout alone — it is a blockquote to every
//! parser we run, and comrak's `alerts` extension only knows GitHub's five
//! types. What lives here is the part every renderer needs and none should
//! re-derive: the header grammar, the type → kind aliasing table from the
//! Obsidian docs, and turning a blockquote's source back into the body's
//! Markdown so a client can render it with whatever it renders everything
//! else with. Fold state is parsed but ignored by v0 (rendered open).

/// The visual family a callout type maps onto. Obsidian ships ~20 type names
/// over a handful of looks; the aliases collapse here so a theme only needs
/// one colour and one glyph per kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CalloutKind {
    /// `note`
    Note,
    /// `info`
    Info,
    /// `todo`
    Todo,
    /// `tip`, `hint`, `important`
    Tip,
    /// `success`, `check`, `done`
    Success,
    /// `question`, `help`, `faq`
    Question,
    /// `warning`, `caution`, `attention`
    Warning,
    /// `failure`, `fail`, `missing`
    Failure,
    /// `danger`, `error`
    Danger,
    /// `bug`
    Bug,
    /// `example`
    Example,
    /// `quote`, `cite`
    Quote,
    /// `abstract`, `summary`, `tldr`
    Summary,
    /// Anything else: Obsidian renders an unknown type as a `note`.
    Unknown,
}

impl CalloutKind {
    /// Map a type name as written (any case) onto its kind, per the
    /// Obsidian callout docs. `important` is listed under tip by Obsidian;
    /// it keeps its own colour in the shell, so the shell reads the raw
    /// type name for that one distinction.
    pub fn from_type(t: &str) -> Self {
        match t.to_ascii_lowercase().as_str() {
            "note" => Self::Note,
            "info" => Self::Info,
            "todo" => Self::Todo,
            "tip" | "hint" | "important" => Self::Tip,
            "success" | "check" | "done" => Self::Success,
            "question" | "help" | "faq" => Self::Question,
            "warning" | "caution" | "attention" => Self::Warning,
            "failure" | "fail" | "missing" => Self::Failure,
            "danger" | "error" => Self::Danger,
            "bug" => Self::Bug,
            "example" => Self::Example,
            "quote" | "cite" => Self::Quote,
            "abstract" | "summary" | "tldr" => Self::Summary,
            _ => Self::Unknown,
        }
    }
}

/// `[!type]-` folds closed, `[!type]+` folds open, no sign means not foldable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fold {
    None,
    Open,
    Closed,
}

/// The header of a callout, parsed from its first line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CalloutHeader {
    /// The type as the author wrote it, lowercased (`warning`, `hint`).
    pub type_name: String,
    pub kind: CalloutKind,
    pub fold: Fold,
    /// The title as written after the marker, or the capitalised type name
    /// when the author gave none — Obsidian's default.
    pub title: String,
}

/// Parse a callout header from the first line of a blockquote, given with or
/// without its leading `>` markers. `None` when the line is not a callout.
///
/// The grammar is Obsidian's: `[!` + type + `]` + optional `-`/`+` + optional
/// title on the same line. The type is one word of letters, digits, `-` or
/// `_`; anything else (`[!]`, `[! warning]`, `[!a b]`) is plain text.
pub fn parse_header(line: &str) -> Option<CalloutHeader> {
    let t = strip_quote_markers(line).trim();
    let rest = t.strip_prefix("[!")?;
    let close = rest.find(']')?;
    let type_raw = &rest[..close];
    if type_raw.is_empty()
        || !type_raw
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
    {
        return None;
    }
    let mut after = &rest[close + 1..];
    let fold = if let Some(r) = after.strip_prefix('-') {
        after = r;
        Fold::Closed
    } else if let Some(r) = after.strip_prefix('+') {
        after = r;
        Fold::Open
    } else {
        Fold::None
    };
    // A title must be separated from the marker by whitespace: `[!note]x` is
    // not a callout in Obsidian either.
    if !after.is_empty() && !after.starts_with(char::is_whitespace) {
        return None;
    }
    let type_name = type_raw.to_ascii_lowercase();
    let title = match after.trim() {
        "" => capitalise(&type_name),
        s => s.to_string(),
    };
    Some(CalloutHeader {
        kind: CalloutKind::from_type(&type_name),
        type_name,
        fold,
        title,
    })
}

/// Rebuild the body Markdown of a callout from the blockquote's source: every
/// line after the header with one level of `>` quoting removed. Lazy
/// continuation lines (no `>`) come through unchanged, as CommonMark reads
/// them. Nested quotes, lists and fences keep their own markers, so a
/// Markdown renderer given the result sees exactly what Obsidian would.
/// Keep original line endings and trailing content whitespace: nested code
/// blocks use this body for exact Copy, not only for display.
pub fn body_from_source(blockquote: &str) -> String {
    let mut remaining = blockquote;
    let mut header = true;
    let mut out = String::with_capacity(blockquote.len());
    while !remaining.is_empty() {
        let content_end = remaining.find(['\r', '\n']).unwrap_or(remaining.len());
        let ending = &remaining[content_end..];
        let eol_len = if ending.starts_with("\r\n") {
            2
        } else if ending.is_empty() {
            0
        } else {
            1
        };
        if !header {
            out.push_str(strip_quote_markers(&remaining[..content_end]));
            out.push_str(&remaining[content_end..content_end + eol_len]);
        }
        header = false;
        remaining = &remaining[content_end + eol_len..];
    }
    if out.trim().is_empty() {
        String::new()
    } else {
        out
    }
}

/// One level of blockquote marker: up to three spaces, `>`, one optional
/// space. A line without a marker is returned as is.
fn strip_quote_markers(line: &str) -> &str {
    let indent = line.len() - line.trim_start_matches(' ').len();
    if indent > 3 {
        return line;
    }
    match line[indent..].strip_prefix('>') {
        Some(r) => r.strip_prefix(' ').unwrap_or(r),
        None => line,
    }
}

fn capitalise(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_with_default_title() {
        let h = parse_header("> [!warning]").unwrap();
        assert_eq!(h.type_name, "warning");
        assert_eq!(h.kind, CalloutKind::Warning);
        assert_eq!(h.fold, Fold::None);
        assert_eq!(h.title, "Warning");
    }

    #[test]
    fn header_with_title_and_fold() {
        let h = parse_header("> [!tip]- Read me later").unwrap();
        assert_eq!(h.kind, CalloutKind::Tip);
        assert_eq!(h.fold, Fold::Closed);
        assert_eq!(h.title, "Read me later");
        let h = parse_header("[!NOTE]+ Open").unwrap();
        assert_eq!(h.type_name, "note");
        assert_eq!(h.fold, Fold::Open);
        assert_eq!(h.title, "Open");
    }

    #[test]
    fn aliases_collapse_onto_kinds() {
        for (t, k) in [
            ("hint", CalloutKind::Tip),
            ("important", CalloutKind::Tip),
            ("check", CalloutKind::Success),
            ("done", CalloutKind::Success),
            ("fail", CalloutKind::Failure),
            ("missing", CalloutKind::Failure),
            ("error", CalloutKind::Danger),
            ("faq", CalloutKind::Question),
            ("tldr", CalloutKind::Summary),
            ("abstract", CalloutKind::Summary),
            ("cite", CalloutKind::Quote),
            ("caution", CalloutKind::Warning),
            ("custom-thing", CalloutKind::Unknown),
        ] {
            assert_eq!(parse_header(&format!("[!{t}]")).unwrap().kind, k, "{t}");
        }
    }

    #[test]
    fn not_a_callout() {
        for l in [
            "> plain quote",
            "> [!]",
            "> [! warning]",
            "> [!a b] x",
            "> [!note]x",
            "> [warning]",
            "> [!unclosed",
        ] {
            assert!(parse_header(l).is_none(), "{l:?}");
        }
    }

    #[test]
    fn body_keeps_nested_structure() {
        let src = "> [!tip] Title\n> First para.\n>\n> - one\n>   - nested\n> - two\n>\n> ```rust\n> let x = 1;\n> ```\n> > [!note] inner\n> > quoted";
        assert_eq!(
            body_from_source(src),
            "First para.\n\n- one\n  - nested\n- two\n\n```rust\nlet x = 1;\n```\n> [!note] inner\n> quoted"
        );
    }

    #[test]
    fn body_of_a_header_only_callout_is_empty() {
        assert_eq!(body_from_source("> [!warning]"), "");
        assert_eq!(body_from_source("> [!warning] t\n>"), "");
    }

    #[test]
    fn lazy_continuation_lines_pass_through() {
        assert_eq!(
            body_from_source("> [!note]\n> a\nb continues\n> c"),
            "a\nb continues\nc"
        );
    }
}

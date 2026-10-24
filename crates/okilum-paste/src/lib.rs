//! Pure clipboard converters for Okilum's smart paste (#1611).
//!
//! Everything here is a function of its arguments: no GPUI, no platform
//! clipboard, no file system and no network. The shell reads the clipboard,
//! picks a flavor and calls into this crate; it owns saving assets, toasts,
//! downloads and undo.
//!
//! Line breaks: a hard line break is written as two trailing spaces
//! (`"  \n"`). Okilum renders through comrak with `hardbreaks = false`, where a
//! bare newline is a soft break (rendered as a space); the two-space form is a
//! CommonMark `LineBreak` there and in Obsidian whatever its "strict line
//! breaks" setting is. Inside table cells the break is `<br>`.

mod css;
mod html;
mod links;
mod render;
mod text;

pub use html::html_to_markdown;
pub use links::{extract_title, link_over_selection, single_url, titled_link};
pub use render::fence_code;
pub use text::{adapt_to_context, guess_language, tsv_to_table, vscode_language};

/// HTML from the clipboard (`public.html`, or RTF converted to simple HTML).
#[derive(Clone, Copy, Debug)]
pub struct HtmlInput<'a> {
    pub html: &'a str,
    /// Page URL relative links resolve against (`org.chromium.source-url`).
    /// A `<base href>` in the HTML is resolved against it and wins.
    pub base_url: Option<&'a str>,
}

#[derive(Clone, PartialEq, Eq)]
pub enum AssetSource {
    /// An absolute http(s) URL; the image is already in the Markdown as a
    /// remote image and may be downloaded in the background.
    Remote(String),
    /// A `data:` URI, decoded.
    Data { mime: String, bytes: Vec<u8> },
    /// A `file://` image (Word's `msohtmlclip` temporary files).
    LocalFile(std::path::PathBuf),
}

impl std::fmt::Debug for AssetSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Remote(url) => f.debug_tuple("Remote").field(url).finish(),
            Self::Data { mime, bytes } => f
                .debug_struct("Data")
                .field("mime", mime)
                .field("bytes", &bytes.len())
                .finish(),
            Self::LocalFile(path) => f.debug_tuple("LocalFile").field(path).finish(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Asset {
    pub source: AssetSource,
    pub alt: String,
    /// Exact substring of [`Converted::markdown`] that stands for this image.
    /// `Remote`: the literal `![alt](url)` text (one entry per distinct text;
    /// it may occur more than once, replace every occurrence).
    /// `Data` / `LocalFile`: the unique token `![alt](okilum-paste-asset:N)`
    /// the caller MUST replace (with `![[saved name]]`) or remove before
    /// inserting.
    pub placeholder: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Converted {
    pub markdown: String,
    pub assets: Vec<Asset>,
    /// No formatting beyond paragraphs and line breaks: insert the plain
    /// flavor instead.
    pub plain: bool,
    /// Contains block-level output (heading, list, quote, code block, table,
    /// rule): it goes on its own lines.
    pub block: bool,
    /// The whole input was one code block (a `<pre>`, or text that is all
    /// monospace over two or more lines). `Some("")` when the language is
    /// unknown; `None` when the input was not a single code block.
    pub code_language: Option<String>,
    /// Additive (#1611): every visible character was monospace (a `<pre>`,
    /// `<code>`, or a monospace font such as Menlo). With one line the shell
    /// pastes plain text; with two or more `code_language` is set.
    pub monospace: bool,
}

impl Converted {
    /// Additive (#1611): the HTML was nothing but one image (an `<img>` copy
    /// from a browser). The shell then saves the clipboard's image data
    /// instead of fetching the URL.
    pub fn is_single_image(&self) -> bool {
        self.assets.len() == 1 && self.markdown == self.assets[0].placeholder
    }
}

/// What VS Code (and its forks: Cursor, Windsurf) put in `vscode-editor-data`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VsCodeHint {
    /// Fence language, already mapped (`typescriptreact` → `tsx`).
    pub language: Option<String>,
    /// The editor mode was Markdown or plain text: paste as text, no fence.
    pub is_text: bool,
}

/// Where the paste lands, as seen on the caret's line.
#[derive(Clone, Copy, Debug)]
pub struct InsertContext<'a> {
    pub line_prefix_before_caret: &'a str,
    pub rest_of_line: &'a str,
    pub in_code_block: bool,
}

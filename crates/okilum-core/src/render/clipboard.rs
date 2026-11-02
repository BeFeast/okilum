//! HTML for an already captured Read selection. No vault reads or asset fetching.
use anyhow::{bail, ensure, Result};

pub const MAX_SOURCE_BYTES: usize = 1024 * 1024;
pub const MAX_HTML_BYTES: usize = 8 * 1024 * 1024;

struct BoundedHtml(String);
impl std::fmt::Write for BoundedHtml {
    fn write_str(&mut self, text: &str) -> std::fmt::Result {
        if self.0.len().saturating_add(text.len()) > MAX_HTML_BYTES {
            return Err(std::fmt::Error);
        }
        self.0.push_str(text);
        Ok(())
    }
}

/// Reconstructed selection Markdown carries inline marks, including a partly
/// selected bold/link/code run. Preserve newlines and indentation verbatim.
pub fn html(markdown: &str) -> Result<String> {
    ensure!(
        markdown.len() <= MAX_SOURCE_BYTES,
        "Selection exceeds the 1 MiB formatted-copy limit"
    );
    let arena = comrak::Arena::new();
    let mut options = super::comrak_options();
    options.render.r#unsafe = false;
    // A selected horizontal rule is not YAML frontmatter.
    options.extension.front_matter_delimiter = None;
    let document = comrak::parse_document(&arena, markdown, &options);
    let mut out = BoundedHtml(String::new());
    if comrak::format_html(document, &options, &mut out).is_err() {
        bail!("Formatted selection exceeds the 8 MiB HTML limit");
    }
    // HTML fragments have no UI stylesheet or generated note wrapper.
    Ok(out.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fmt::Write as _;
    #[test]
    fn selection_markup_and_code_bytes_survive_html_conversion() {
        let out = html("**café 雪** *italic* [link](https://example.test) `a < b`\n\n```rs\n    let x = \"<&>\";\n```\n").unwrap();
        assert!(out.contains("<strong>café 雪</strong>") && out.contains("<em>italic</em>"));
        assert!(out.contains("href=\"https://example.test\""));
        assert!(out.contains("<code>a &lt; b</code>"));
        assert!(
            out.contains("    let x = ")
                && out.contains("&lt;&amp;&gt;")
                && out.contains(";\n</code></pre>")
        );
    }
    #[test]
    fn lists_and_horizontal_rules_are_formatted_without_raw_active_html() {
        let out = html("---\n\n- one\n  - **two**\n\n<script>alert(1)</script>\n\n[bad](javascript:alert(1))\n").unwrap();
        assert!(out.contains("<hr") && out.matches("<ul>").count() == 2);
        assert!(out.contains("<strong>two</strong>"));
        assert!(!out.contains("<script>") && !out.contains("href=\"javascript:"));
    }
    #[test]
    fn limits_refuse_oversized_input_and_output_with_positive_controls() {
        assert!(html(&"x".repeat(MAX_SOURCE_BYTES + 1)).is_err());
        assert_eq!(html("**ok**").unwrap(), "<p><strong>ok</strong></p>\n");
        let mut output = BoundedHtml(String::new());
        output.write_str(&"x".repeat(MAX_HTML_BYTES)).unwrap();
        assert!(output.write_str("x").is_err());
        assert_eq!(output.0.len(), MAX_HTML_BYTES);
    }
}

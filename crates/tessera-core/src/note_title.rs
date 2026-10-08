//! Display titles shared by reader labels and prepared embedded blocks.

pub fn from_bytes(head: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(head);
    let mut lines = text.lines();
    let mut frontmatter_title = None;
    if text.starts_with("---") {
        lines.next();
        for line in lines.by_ref() {
            if line.trim() == "---" {
                break;
            }
            if let Some(value) = line.strip_prefix("title:") {
                let value = value.trim().trim_matches(|c| c == '"' || c == '\'');
                if !value.is_empty() {
                    frontmatter_title = Some(value.to_owned());
                }
            }
        }
    }
    let mut fenced = false;
    // Stop at the first H1 instead of collecting the whole head.
    for line in lines {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if !fenced {
            if let Some(h1) = line.strip_prefix("# ") {
                let h1 = h1.trim();
                if !h1.is_empty() {
                    return Some(h1.to_owned());
                }
            }
        }
    }
    frontmatter_title
}

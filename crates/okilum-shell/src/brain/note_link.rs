//! Link syntax and exact existing-preview response checks; no filesystem reads.
use serde_json::Value;
use sha2::{Digest, Sha256};

pub(super) fn key(path: &str) -> String {
    path.to_lowercase().trim_end_matches(".md").to_owned()
}
pub(super) fn target(path: &str, sources: &[Value]) -> Result<String, String> {
    if !path.ends_with(".md")
        || path
            .chars()
            .any(|c| c.is_control() || "[]|#^\\()`<>%".contains(c))
        || path
            .split('/')
            .any(|s| s.is_empty() || s == "." || s == ".." || s.trim() != s)
    {
        return Err(
            "This filename cannot be inserted as an exact note link. Choose another note.".into(),
        );
    }
    let matches: std::collections::BTreeSet<_> = sources
        .iter()
        .filter_map(|s| s["path"].as_str())
        .filter(|p| key(p) == key(path))
        .collect();
    if matches.len() != 1 || !matches.contains(path) {
        return Err("The listed notes do not identify one exact destination. Refresh Sources and choose again.".into());
    }
    Ok(path.to_owned())
}
pub(super) fn insertion(target: &str, selected: &str) -> Result<String, String> {
    if selected.is_empty() {
        return Ok(format!("[[{target}]]"));
    }
    // The Reader trims aliases and interprets Markdown/HTML. Refuse syntax that
    // cannot retain the selected text as the ordinary visible label.
    if selected.trim() != selected
        || selected
            .chars()
            .any(|c| c.is_control() || "[]|\\`*_~<>&=".contains(c))
    {
        return Err("Select plain single-line text without Markdown syntax, or place the caret where the link should go.".into());
    }
    Ok(format!("[[{target}|{selected}]]"))
}
pub(super) fn probe(target: &str) -> String {
    format!("[[{target}]]\n")
}
pub(super) fn digest(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}
pub(super) fn verify(
    reply: &Value,
    source: &str,
    revision: &Value,
    target: &str,
) -> Result<(), String> {
    let valid = reply["path"] == source
        && &reply["revision"] == revision
        && reply["preview_revision"] == digest(probe(target).as_bytes())
        && reply["links"].as_array().is_some_and(|links| {
            links.len() == 1 && {
                let link = &links[0];
                link["target"] == target
                    && link["status"] == "resolved"
                    && link["candidates"]
                        .as_array()
                        .is_some_and(|c| c.len() == 1 && c[0]["path"] == target)
            }
        });
    if valid {
        Ok(())
    } else {
        Err("The note link could not be confirmed for this exact source and destination. Refresh Sources and choose again; your draft is unchanged.".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn qualified_targets_preserve_case_and_refuse_observed_collisions_and_syntax() {
        let list = vec![json!({"path":"a/Note.md"}), json!({"path":"b/Note.md"})];
        assert_eq!(target("a/Note.md", &list).unwrap(), "a/Note.md");
        for path in [
            "../a.md", "x//a.md", "a#b.md", "a^b.md", "a|b.md", "[a].md", "a\\b.md", " a.md",
            "a/ b.md", "a\nb.md",
        ] {
            assert!(target(path, &[json!({"path":path})]).is_err(), "{path:?}");
        }
        for collision in ["a/note.md", "a/Note.md.md"] {
            let mut list = list.clone();
            list.push(json!({"path":collision}));
            assert!(target("a/Note.md", &list).is_err());
        }
        assert!(target("missing.md", &list).is_err());
    }
    #[test]
    fn aliases_preserve_exact_plain_selection_and_refuse_lossy_display() {
        assert_eq!(insertion("x.md", "").unwrap(), "[[x.md]]");
        assert_eq!(
            insertion("x.md", "План 😀 (v2)").unwrap(),
            "[[x.md|План 😀 (v2)]]"
        );
        for alias in [
            " text", "text ", "a\nb", "a\r\nb", "[a]", "a|b", "*a*", "a_b", "<b>", "&amp;", "`a`",
            "a\\b",
        ] {
            assert!(insertion("x.md", alias).is_err(), "{alias:?}");
        }
    }
    #[test]
    fn preview_proof_binds_source_bytes_revision_and_exact_readable_destination() {
        let reply = json!({"path":"a.md","revision":"r1","preview_revision":digest(probe("dir/b.md").as_bytes()),"links":[{"target":"dir/b.md","status":"resolved","candidates":[{"path":"dir/b.md"}]}]});
        assert!(verify(&reply, "a.md", &json!("r1"), "dir/b.md").is_ok());
        for (pointer, value) in [
            ("/path", json!("other.md")),
            ("/revision", json!("r2")),
            ("/preview_revision", json!("wrong")),
            ("/links/0/target", json!("b.md")),
            ("/links/0/status", json!("ambiguous")),
            ("/links/0/candidates/0/path", json!("dir/B.MD")),
            ("/links/0/candidates", json!([])),
        ] {
            let mut changed = reply.clone();
            *changed.pointer_mut(pointer).unwrap() = value;
            assert!(
                verify(&changed, "a.md", &json!("r1"), "dir/b.md").is_err(),
                "{pointer}"
            );
        }
    }
}

//! Native reader preprocessing with caller-supplied draft bytes and root-bounded
//! attachment transport. No source writes, local file URLs, or remote fetches.
use crate::Runner;
use anyhow::{ensure, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Component, Path};
use tessera_core::{document_links, render, Vault};

const MAX_SOURCE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_ASSET_BYTES: u64 = 8 * 1024 * 1024;
const MAX_TOTAL_ASSET_BYTES: u64 = 32 * 1024 * 1024;

fn bounded_relative(root: &Path, candidate: &Path) -> Option<String> {
    let relative = candidate.strip_prefix(root).ok()?;
    let mut parts = Vec::new();
    for component in relative.components() {
        match component {
            Component::Normal(part) => parts.push(part.to_str()?.to_owned()),
            Component::CurDir => {}
            Component::ParentDir => {
                parts.pop()?;
            }
            _ => return None,
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("/"))
    }
}
fn media_type(path: &str) -> Option<&'static str> {
    match Path::new(path)
        .extension()?
        .to_str()?
        .to_ascii_lowercase()
        .as_str()
    {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        "svg" => Some("image/svg+xml"),
        "bmp" => Some("image/bmp"),
        _ => None,
    }
}

pub fn source_preview(runner: &Runner, path: &str, draft: Option<&str>) -> Result<Value> {
    let snapshot = runner.read_preview_source(path, MAX_SOURCE_BYTES)?;
    ensure!(
        draft.is_none_or(|s| s.len() <= (MAX_SOURCE_BYTES as usize).div_ceil(3) * 4),
        "draft exceeds preview byte budget"
    );
    let bytes = STANDARD
        .decode(draft.unwrap_or(&snapshot.content_base64))
        .context("invalid source base64")?;
    ensure!(
        bytes.len() as u64 <= MAX_SOURCE_BYTES,
        "draft exceeds preview byte budget"
    );
    let raw = std::str::from_utf8(&bytes)
        .context("preview requires valid UTF-8; original source bytes remain unchanged")?;
    let preview_revision = format!("sha256:{:x}", Sha256::digest(&bytes));
    // Metadata-only resolver performs no filesystem content reads. The source
    // store remains the only path to source and attachment bytes in this API.
    let vault = Vault::scan_metadata(runner.root())?;
    let text = render::preprocess(render::without_frontmatter(raw));
    let mut assets = BTreeMap::<String, Value>::new();
    let mut total_asset_bytes = 0u64;
    let mut asset_paths = BTreeMap::<String, String>::new();
    let images = render::rewrite_source_images_with(&text, |url| {
        // Preserve the reader's remote reference, but never fetch it here.
        if url.starts_with("https://") || url.starts_with("http://") {
            return None;
        }
        let loaded = (|| -> Option<String> {
            // Explicit file URLs and other schemes never reach a GUI filesystem.
            if url.contains("://") || url.starts_with("data:") {
                return None;
            }
            let target = url.replace("%20", " ");
            let candidate = vault.resolve_asset(&target, path)?;
            let relative = bounded_relative(runner.root(), &candidate)?;
            if let Some(url) = asset_paths.get(&relative) {
                return Some(url.clone());
            }
            let kind = media_type(&relative)?;
            let asset = runner
                .read_preview_source(
                    &relative,
                    MAX_ASSET_BYTES.min(MAX_TOTAL_ASSET_BYTES.saturating_sub(total_asset_bytes)),
                )
                .ok()?;
            let opaque = format!(
                "tessera-asset://{}",
                asset.revision.strip_prefix("sha256:")?
            );
            if !assets.contains_key(&opaque) {
                total_asset_bytes += STANDARD.decode(&asset.content_base64).ok()?.len() as u64;
            }
            asset_paths.insert(relative, opaque.clone());
            assets.entry(opaque.clone()).or_insert_with(
                || json!({"url":opaque,"content_base64":asset.content_base64,"media_type":kind}),
            );
            Some(opaque)
        })();
        Some(loaded.unwrap_or_else(|| "tessera-asset://unavailable".into()))
    });
    let mut preparation = document_links::prepared::LinkPreparation::new(&vault, path, |target| {
        let (content, revision) = if target == path {
            (raw.to_owned(), preview_revision.clone())
        } else {
            let snapshot = runner
                .read_preview_source(target, MAX_SOURCE_BYTES)
                .map_err(|_| "Document source is unavailable.".to_owned())?;
            let bytes = STANDARD
                .decode(&snapshot.content_base64)
                .map_err(|_| "Document source is unavailable.".to_owned())?;
            (
                String::from_utf8(bytes).map_err(|_| "Document source is not UTF-8.".to_owned())?,
                snapshot.revision,
            )
        };
        Ok(document_links::prepared::TargetSnapshot {
            revision,
            headings: document_links::HeadingInventory::new(&content),
            supports_setext: false,
            managed: Some(tessera_core::source_classifier::classify(
                &tessera_core::source_projection::Snapshot::new(target, 0, content.as_str()),
            )),
        })
    });
    let mut links = Vec::new();
    // Compatibility adapter only: this is the exact historical inline subset,
    // applied to a link already parsed by Comrak, never the syntax authority.
    let legacy_md = regex::Regex::new(r"^\[([^\]]+)\]\(([^) ]+?\.md)\)$")?;
    for link in document_links::parse(&images) {
        let (resolved, prepared) = preparation.link(&link.target, link.wiki);
        let candidates: Vec<_> = resolved
            .candidates
            .iter()
            .filter(|p| preparation.readable(p))
            .map(|p| json!({"title":vault.note_title(p),"path":p}))
            .collect();
        // Keep the pre-#314 join key for old clients and insertion probes.
        // Full interpretation is additive and consumed only by version-aware clients.
        let base = link.target.split('#').next().unwrap_or("").trim();
        let legacy_target = if link.wiki {
            base.to_owned()
        } else {
            legacy_md
                .captures(&images[link.range.clone()])
                .map(|c| c[2].trim_end_matches(".md").replace("%20", " "))
                .unwrap_or_else(|| base.trim_end_matches(".md").replace("%20", " "))
        };
        // Old rendered handlers ignore additive heading fields. New statuses
        // force their existing visible refusal instead of opening the note top.
        let status = match (resolved.status, resolved.heading.is_some()) {
            ("resolved", true) => "resolved_heading",
            ("ambiguous", true) => "ambiguous_heading",
            (status, _) => status,
        };
        let row = json!({"url":resolved.url,"target":legacy_target,"authored_target":link.target,"wiki":link.wiki,
            "status":status,"prepared":prepared,"candidates":candidates,"heading":resolved.heading,"reason":resolved.reason});
        if !links.contains(&row) {
            links.push(row);
        }
    }
    let markdown = render::rewrite_source_links(&images, &vault, path);
    let markdown = render::rewrite_highlights(&markdown);
    Ok(
        json!({"prepared_links_version":1,"document_links_version":1,"path":path,"revision":snapshot.revision,"preview_revision":preview_revision,"markdown":markdown,"assets":assets.into_values().collect::<Vec<_>>(),"links":links}),
    )
}

#[cfg(test)]
mod note_link_tests {
    use super::*;
    use crate::RunnerConfig;
    use tessera_core::source::WriteBoundary;

    #[test]
    fn note_link_probe_uses_actual_resolver_when_listed_case_changes_to_unlisted_extension() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("brain");
        let operational_dir = dir.path().join("runtime");
        std::fs::create_dir_all(root.join("records")).unwrap();
        std::fs::create_dir_all(root.join("a")).unwrap();
        std::fs::create_dir_all(root.join("b")).unwrap();
        std::fs::create_dir(&operational_dir).unwrap();
        std::fs::write(root.join("source.md"), "# Source\nUnchanged\n").unwrap();
        std::fs::write(root.join("a/target.md"), "# Other\n").unwrap();
        std::fs::write(root.join("b/target.md"), "# Chosen\n").unwrap();
        let runner = Runner::open(RunnerConfig {
            brain_id: uuid::Uuid::new_v4().to_string(),
            root: root.clone(),
            operational_dir,
            records_dir: "records".into(),
            boundary: WriteBoundary::Managed,
        })
        .unwrap();
        let before = runner.read_source("source.md").unwrap();
        let list = runner.source_list().unwrap();
        assert!(list.iter().any(|s| s["path"] == "b/target.md"));
        let probe = STANDARD.encode("[[b/target.md]]\n");
        let success = source_preview(&runner, "source.md", Some(&probe)).unwrap();
        assert_eq!(success["links"][0]["status"], "resolved");
        assert_eq!(success["links"][0]["candidates"][0]["path"], "b/target.md");
        // Change the selected filename after listing; there is exactly one
        // casefold candidate, so this proof is independent of directory order.
        std::fs::rename(root.join("b/target.md"), root.join("b/TARGET.MD")).unwrap();
        assert!(!runner
            .source_list()
            .unwrap()
            .iter()
            .any(|s| s["path"] == "b/TARGET.MD"));
        let changed = source_preview(&runner, "source.md", Some(&probe)).unwrap();
        assert_eq!(changed["links"][0]["status"], "resolved");
        assert_eq!(changed["links"][0]["target"], "b/target.md");
        assert_eq!(changed["links"][0]["candidates"][0]["path"], "b/TARGET.MD");
        assert_ne!(changed["links"][0]["candidates"][0]["path"], "b/target.md");
        assert_eq!(changed["revision"], before.revision);
        assert_eq!(
            runner.read_source("source.md").unwrap().content_base64,
            before.content_base64
        );
    }
}

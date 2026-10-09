//! Native reader preprocessing with caller-supplied draft bytes and root-bounded
//! attachment transport. No source writes, local file URLs, or remote fetches.
use crate::Runner;
use anyhow::{ensure, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use okilum_core::{document_links, render, Vault};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

const MAX_SOURCE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_ASSET_BYTES: u64 = 8 * 1024 * 1024;
const MAX_TOTAL_ASSET_BYTES: u64 = 32 * 1024 * 1024;

#[derive(Default)]
struct PreviewAssets {
    assets: BTreeMap<String, Value>,
    paths: BTreeMap<String, Option<(String, String)>>,
    total_bytes: u64,
}
impl PreviewAssets {
    fn load(&mut self, runner: &Runner, path: &str) -> Result<(String, String)> {
        if let Some(asset) = self.paths.get(path) {
            return asset.clone().context("Image unavailable in this preview");
        }
        self.paths.insert(path.into(), None);
        let kind = document_links::image_media_type(path)
            .context("This attachment format is not supported in managed preview.")?;
        let asset = runner.read_preview_source(
            path,
            MAX_ASSET_BYTES.min(MAX_TOTAL_ASSET_BYTES.saturating_sub(self.total_bytes)),
        )?;
        let opaque = format!(
            "okilum-asset://{}",
            asset
                .revision
                .strip_prefix("sha256:")
                .context("Invalid attachment revision")?
        );
        if !self.assets.contains_key(&opaque) {
            self.total_bytes += STANDARD.decode(&asset.content_base64)?.len() as u64;
        }
        self.assets.entry(opaque.clone()).or_insert_with(
            || json!({"url":opaque,"content_base64":asset.content_base64,"media_type":kind}),
        );
        let result = (opaque, asset.revision);
        self.paths.insert(path.into(), Some(result.clone()));
        Ok(result)
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
    let mut assets = PreviewAssets::default();
    let images = render::rewrite_source_images_with(&text, |target| {
        if target.starts_with("https://") || target.starts_with("http://") {
            return None;
        }
        let resolved = document_links::resolve(target, false, &vault, path);
        let loaded = (resolved.status == "attachment")
            .then(|| resolved.candidates.first())
            .flatten()
            .and_then(|path| assets.load(runner, path).ok());
        Some(
            loaded
                .map(|(url, _)| url)
                .unwrap_or_else(|| "okilum-asset://unavailable".into()),
        )
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
            managed: Some(okilum_core::source_classifier::classify(
                &okilum_core::source_projection::Snapshot::new(target, 0, content.as_str()),
            )),
        })
    });
    let mut links = Vec::new();
    // Compatibility adapter only: this is the exact historical inline subset,
    // applied to a link already parsed by Comrak, never the syntax authority.
    let legacy_md = regex::Regex::new(r"^\[([^\]]+)\]\(([^) ]+?\.md)\)$")?;
    for link in document_links::parse_in_vault(&images, &vault, path) {
        let (resolved, mut prepared) = preparation.link(&link.target, link.wiki);
        if resolved.status == "attachment" {
            let relative = &resolved.candidates[0];
            let loaded = assets.load(runner, relative);
            let (status, asset_url, asset_revision, reason) = match loaded {
                Ok((url, revision)) => ("attachment", Some(url), Some(revision), None),
                Err(_) => {
                    let reason = if document_links::image_media_type(relative).is_none() {
                        "This attachment format is not supported in managed preview."
                    } else {
                        "This image is unavailable or exceeds the preview limit. Refresh to try again."
                    };
                    prepared.status = document_links::prepared::LinkStatus::Unsupported;
                    prepared.reason = reason.into();
                    ("unsupported", None, None, Some(reason))
                }
            };
            prepared.target_revision = asset_revision.clone();
            let row = json!({"url":resolved.url,"target":link.target,"authored_target":link.target,
                "wiki":link.wiki,"status":status,"prepared":prepared,
                "candidates":[{"path":relative,"title":vault.note_title(relative)}],
                "asset_url":asset_url,"asset_revision":asset_revision,"reason":reason});
            if !links.contains(&row) {
                links.push(row);
            }
            continue;
        }
        if resolved.status == "ambiguous"
            && document_links::image_media_type(&document_links::decode(
                link.target.split('#').next().unwrap_or_default(),
            ))
            .is_some()
        {
            prepared.status = document_links::prepared::LinkStatus::Unsupported;
            prepared.reason =
                "Several images match. Use a source-relative or full vault path.".into();
            let row = json!({"url":resolved.url,"target":link.target,"authored_target":link.target,
                "wiki":link.wiki,"status":"unsupported","prepared":prepared,
                "candidates":[],"reason":prepared.reason});
            if !links.contains(&row) {
                links.push(row);
            }
            continue;
        }
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
        json!({"attachment_links_version":1,"prepared_links_version":1,"document_links_version":1,"path":path,"revision":snapshot.revision,"preview_revision":preview_revision,"markdown":markdown,"assets":assets.assets.into_values().collect::<Vec<_>>(),"links":links}),
    )
}

#[cfg(test)]
mod note_link_tests {
    use super::*;
    use crate::RunnerConfig;
    use okilum_core::source::WriteBoundary;

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

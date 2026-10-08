//! Opt-in desktop filesystem evidence, evaluated only in background preparation.
use super::prepared::{LinkState, LinkStatus};
use super::{decode, encode, resolve, ResolvedLink};
use crate::Vault;
use std::{
    io::ErrorKind,
    path::{Path, PathBuf},
};

pub(super) fn prepare(
    vault: &Vault,
    from: &str,
    target: &str,
    wiki: bool,
) -> Option<(ResolvedLink, LinkState)> {
    let raw = target.split('#').next()?;
    let file_uri = !wiki && raw.starts_with("file:");
    let path = if file_uri {
        let uri = url::Url::parse(raw).ok()?;
        if uri
            .host_str()
            .is_some_and(|h| h != "localhost" && !h.is_empty())
        {
            return Some(unavailable(target, "Network file links are unavailable."));
        }
        uri.to_file_path().ok()?
    } else {
        let decoded = if wiki {
            raw.trim().to_owned()
        } else {
            decode(raw)
        };
        if decoded.starts_with("//") || decoded.starts_with("\\\\") {
            return Some(unavailable(target, "Network file links are unavailable."));
        }
        if (decoded.contains(':') || decoded.contains('\\'))
            && !(cfg!(windows) && Path::new(&decoded).is_absolute())
        {
            return None;
        }
        PathBuf::from(decoded)
    };
    if path.as_os_str().is_empty() || path.to_string_lossy().contains('\0') {
        return None;
    }
    // Note/heading resolution remains authoritative. File URLs inside the vault
    // are translated back to ordinary document identity before preparation.
    let markdown = path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("md"));
    if markdown && !file_uri && (!path.is_absolute() || path.starts_with(&vault.root)) {
        return None;
    }
    if !markdown && path.extension().is_none() && !file_uri && !path.is_absolute() {
        return None;
    }
    let root = vault.root.canonicalize().ok()?;
    let direct = if path.is_absolute() {
        path.clone()
    } else if wiki || raw.starts_with('/') {
        root.join(&path)
    } else {
        root.join(Path::new(from).parent().unwrap_or(Path::new("")))
            .join(&path)
    };
    // Existing file URLs may use a different OS spelling of the same path
    // (notably C:\ vs the canonical \\?\ prefix on Windows).
    let canonical_relative = (file_uri || (markdown && path.is_absolute()))
        .then(|| direct.canonicalize().ok())
        .flatten()
        .and_then(|real| real.strip_prefix(&root).ok().map(Path::to_path_buf));
    let spelling = if let Some(relative) = canonical_relative {
        Some(format!(
            "/{}",
            relative
                .to_string_lossy()
                .replace(std::path::MAIN_SEPARATOR, "/")
        ))
    } else if file_uri {
        [&root, &vault.root].into_iter().find_map(|root| {
            direct.strip_prefix(root).ok().map(|p| {
                format!(
                    "/{}",
                    p.to_string_lossy().replace(std::path::MAIN_SEPARATOR, "/")
                )
            })
        })
    } else {
        Some(path.to_string_lossy().into_owned())
    };
    // This opt-in snapshot has no graph-only filesystem shortcut: the background
    // job verifies each distinct authored target, without work during paint.
    if let Some(spelling) = spelling {
        let spelling = if wiki { spelling } else { encode(&spelling) };
        let spelling = target
            .split_once('#')
            .map_or(spelling.clone(), |(_, heading)| {
                format!("{spelling}#{heading}")
            });
        let resolved = resolve(&spelling, wiki, vault, from);
        if matches!(resolved.status, "attachment" | "resolved" | "ambiguous") {
            let status = if resolved.status == "ambiguous" {
                LinkStatus::Ambiguous
            } else {
                LinkStatus::Resolved
            };
            let mut state = LinkState::new(
                status,
                if status == LinkStatus::Ambiguous {
                    "Several files match. Click to choose a destination."
                } else {
                    "Preview file"
                },
            );
            state.action_url = Some(resolved.url.clone());
            return Some((resolved, state));
        }
    }
    match std::fs::symlink_metadata(&direct) {
        Ok(_) => match direct.canonicalize() {
            Ok(real) if real.is_file() && !real.starts_with(&root) => {
                let url = format!("tessera://outside-file/{}", encode(&real.to_string_lossy()));
                Some(result(
                    "outside_file",
                    LinkStatus::Resolved,
                    "Open file",
                    url,
                ))
            }
            Ok(real) if real.is_file() && real.starts_with(&root) && !markdown => {
                let rel = real
                    .strip_prefix(&root)
                    .ok()?
                    .to_string_lossy()
                    .replace(std::path::MAIN_SEPARATOR, "/");
                let url = format!("tessera://attachment/{}", encode(&rel));
                let mut found = result("attachment", LinkStatus::Resolved, "Preview file", url);
                found.0.candidates.push(rel);
                Some(found)
            }
            _ => Some(unavailable(target, "File unavailable.")),
        },
        Err(e)
            if e.kind() == ErrorKind::NotFound
                && (path.is_absolute() || file_uri || vault.inventory_complete) =>
        {
            // A root compatibility candidate that exists but is inaccessible
            // blocks a false missing claim, just like the document resolver.
            let compatibility = root.join(&path);
            if !file_uri
                && !path.is_absolute()
                && !raw.starts_with("./")
                && !raw.starts_with("../")
                && !matches!(std::fs::symlink_metadata(&compatibility), Err(e) if e.kind() == ErrorKind::NotFound)
            {
                return Some(unavailable(target, "File unavailable."));
            }
            let url = format!(
                "tessera://missing-file/{}",
                encode(&direct.to_string_lossy())
            );
            Some(result(
                "unresolved",
                LinkStatus::MissingFile,
                "File not found",
                url,
            ))
        }
        _ => Some(unavailable(target, "File unavailable.")),
    }
}

fn result(
    status: &'static str,
    kind: LinkStatus,
    reason: &str,
    url: String,
) -> (ResolvedLink, LinkState) {
    let mut state = LinkState::new(kind, reason);
    state.action_url = Some(url.clone());
    (
        ResolvedLink {
            url,
            status,
            candidates: vec![],
            heading: None,
            reason: None,
        },
        state,
    )
}

fn unavailable(target: &str, reason: &str) -> (ResolvedLink, LinkState) {
    result(
        "unresolved",
        LinkStatus::Unknown,
        reason,
        format!("{}{}", crate::render::UNRESOLVED_SCHEME, encode(target)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document_links::prepared::{LinkPreparation, TargetSnapshot};

    #[test]
    fn local_file_links_cached_graph_finishes_background_file_verification() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("start.md"), "# Start").unwrap();
        std::fs::write(dir.path().join("asset.json"), "{}").unwrap();
        let mut vault = Vault::scan_metadata(dir.path()).unwrap();
        vault.graph_root = Some(dir.path().to_owned());
        let mut pending =
            LinkPreparation::new(&vault, "start.md", |_| -> Result<TargetSnapshot, String> {
                panic!()
            });
        assert_eq!(
            pending.link("asset.json", false).1.status,
            LinkStatus::Unknown,
            "cached graph baseline"
        );
        let mut background =
            LinkPreparation::new(&vault, "start.md", |_| -> Result<TargetSnapshot, String> {
                panic!()
            })
            .with_local_files();
        assert_eq!(
            background.link("asset.json", false).1.status,
            LinkStatus::Resolved
        );
        assert_eq!(
            background.link("missing.json", false).1.status,
            LinkStatus::MissingFile
        );
    }
}

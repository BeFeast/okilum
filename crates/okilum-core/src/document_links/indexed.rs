//! Rendering consumes captured identities. Filesystem verification belongs to
//! the separate, lazy/background LinkPreparation job, never this resolver.
use super::*;
use std::{borrow::Cow, collections::HashMap, path::Path};

pub struct IndexedResolver<'a> {
    vault: &'a Vault,
    assets: HashMap<String, Vec<String>>,
}
impl<'a> IndexedResolver<'a> {
    pub fn new(vault: &'a Vault) -> Self {
        let mut assets: HashMap<String, Vec<String>> = HashMap::new();
        for entry in &vault.entries {
            if entry.kind != crate::vault::EntryKind::Attachment
                || vault.graph_alias_path(Path::new(&entry.path))
            {
                continue;
            }
            let key = entry.path.to_lowercase();
            let parts: Vec<_> = key.split('/').collect();
            for i in 0..parts.len() {
                assets
                    .entry(parts[i..].join("/"))
                    .or_default()
                    .push(entry.path.clone());
            }
        }
        for paths in assets.values_mut() {
            paths.sort();
            paths.dedup();
        }
        Self { vault, assets }
    }

    pub(crate) fn vault_root(&self) -> &Path {
        &self.vault.root
    }

    pub fn parse(&self, source: &str, from: &str) -> Vec<ParsedLink> {
        // Unverified external paths cannot justify Obsidian's existing-file
        // fallback for otherwise invalid CommonMark destinations with spaces.
        super::paths::parse_with(source, |target| self.resolve(target, false, from), false)
    }

    /// Translate only known, lexical in-vault OS paths. Unknown absolute paths
    /// remain candidates for lazy outside-file verification. No canonicalize.
    fn path<'p>(&self, path: &'p str) -> Cow<'p, str> {
        if !Path::new(path).is_absolute() {
            return Cow::Borrowed(path);
        }
        for root in self
            .vault
            .graph_root
            .iter()
            .chain(std::iter::once(&self.vault.root))
        {
            if root.as_os_str().is_empty() {
                continue;
            }
            if let Ok(relative) = Path::new(path).strip_prefix(root) {
                if !self.vault.graph_alias_path(relative) {
                    return Cow::Owned(format!("/{}", crate::vault::note_path(relative)));
                }
            }
        }
        Cow::Borrowed(path)
    }

    fn asset(&self, path: &str, wiki: bool, from: &str) -> Resolution {
        let key = path.to_lowercase();
        let rooted = key.starts_with('/');
        let explicit = key.starts_with("./") || key.starts_with("../");
        let relative = if rooted || wiki && !explicit {
            self.vault.walk_relative(key.trim_start_matches('/'), "")
        } else {
            self.vault.walk_relative(&key, from)
        };
        let Some(relative) = relative else {
            return Resolution::Unresolved;
        };
        let exact = |key: &str| {
            let matches: Vec<_> = self
                .assets
                .get(key)
                .into_iter()
                .flatten()
                .filter(|path| path.eq_ignore_ascii_case(key))
                .cloned()
                .collect();
            match matches.as_slice() {
                [path] => Resolution::Resolved { path: path.clone() },
                [] => Resolution::Unresolved,
                _ => Resolution::Ambiguous {
                    candidates: matches,
                },
            }
        };
        let local = exact(&relative);
        if local != Resolution::Unresolved {
            return local;
        }
        if rooted
            || explicit
            || self.vault.indexed_occupied(&relative)
            || !self.vault.paths_complete()
        {
            return Resolution::Unresolved;
        }
        let root = exact(&key);
        if root != Resolution::Unresolved {
            return root;
        }
        if self.vault.indexed_occupied(&key) {
            return Resolution::Unresolved;
        }
        match self.assets.get(&key).map(Vec::as_slice).unwrap_or_default() {
            [path] => Resolution::Resolved { path: path.clone() },
            [] => Resolution::Unresolved,
            paths => Resolution::Ambiguous {
                candidates: paths.to_vec(),
            },
        }
    }

    pub fn resolve(&self, target: &str, wiki: bool, from: &str) -> ResolvedLink {
        let _phase = crate::open_diagnostics::phase("resolve_links");
        crate::open_diagnostics::count(
            if wiki {
                "wiki_resolutions"
            } else {
                "markdown_resolutions"
            },
            1,
        );
        if !wiki && is_external_url(target) {
            return ResolvedLink {
                url: target.into(),
                status: "external",
                candidates: vec![],
                heading: None,
                reason: None,
            };
        }
        let raw = target.split('#').next().unwrap_or(target);
        let raw = if wiki {
            raw.trim().to_owned()
        } else {
            decode(raw)
        };
        let path = self.path(&raw);
        let native_absolute = !wiki && Path::new(&raw).is_absolute();
        let in_root = self
            .vault
            .graph_root
            .iter()
            .chain(std::iter::once(&self.vault.root))
            .any(|root| !root.as_os_str().is_empty() && Path::new(&raw).starts_with(root));
        let attachment = (native_absolute || !path.contains([':', '\\']))
            && Path::new(path.as_ref())
                .extension()
                .is_some_and(|ext| !ext.eq_ignore_ascii_case("md"));
        let (resolution, heading) = if attachment {
            (self.asset(&path, wiki, from), None)
        } else {
            let normalized_target = if !wiki && path.as_ref() != raw {
                format!(
                    "{}{}",
                    encode(&path),
                    target
                        .split_once('#')
                        .map_or(String::new(), |(_, fragment)| format!("#{fragment}"))
                )
            } else {
                target.to_owned()
            };
            match destination(&normalized_target, wiki) {
                Destination::External(url) => {
                    return ResolvedLink {
                        url,
                        status: "external",
                        candidates: vec![],
                        heading: None,
                        reason: None,
                    }
                }
                Destination::Unsupported(_) if native_absolute && !in_root => {
                    return outside_file(&raw, None);
                }
                Destination::Unsupported(reason) => {
                    return ResolvedLink {
                        url: format!(
                            "okilum://unsupported/{}",
                            encode(&format!("{reason} Target: {target}"))
                        ),
                        status: "unsupported",
                        candidates: vec![],
                        heading: None,
                        reason: Some(reason),
                    }
                }
                Destination::Note { heading, .. } => {
                    let resolution = if path.is_empty() {
                        Resolution::Resolved { path: from.into() }
                    } else if wiki {
                        self.vault.resolve_from(&path, from)
                    } else {
                        self.vault.resolve_markdown_indexed(&path, from)
                    };
                    (resolution, heading)
                }
            }
        };
        // Known vault-root spellings retain index identity. Unknown absolute
        // paths are displayed without probing whether an OS file exists there.
        if !wiki
            && Path::new(&raw).is_absolute()
            && !in_root
            && resolution == Resolution::Unresolved
        {
            return outside_file(&raw, heading);
        }
        if attachment {
            match resolution {
                Resolution::Resolved { path } => ResolvedLink {
                    url: format!("okilum://attachment/{}", encode(&path)),
                    status: "attachment",
                    candidates: vec![path],
                    heading: None,
                    reason: None,
                },
                Resolution::Ambiguous { candidates } => ResolvedLink {
                    url: format!("{}{}", render::AMBIGUOUS_SCHEME, encode(target)),
                    status: "ambiguous",
                    candidates,
                    heading: None,
                    reason: None,
                },
                Resolution::Unresolved => ResolvedLink {
                    url: format!("{}{}", render::UNRESOLVED_SCHEME, encode(target)),
                    status: "unresolved",
                    candidates: vec![],
                    heading: None,
                    reason: None,
                },
            }
        } else {
            resolved_note(resolution, target, wiki, heading)
        }
    }
}

fn outside_file(path: &str, heading: Option<String>) -> ResolvedLink {
    ResolvedLink {
        url: format!("okilum://outside-file/{}", encode(path)),
        status: "outside_file",
        candidates: vec![],
        heading,
        reason: Some("File verification is deferred."),
    }
}

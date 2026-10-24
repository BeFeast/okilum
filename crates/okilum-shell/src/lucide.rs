//! The complete Lucide icon set, as T3 Code offers it for project icons
//! (#1627): lucide-static at the version T3 ships, every icon and alias
//! (T3 stores aliases such as `globe-2`), with Lucide's search tags.
//!
//! The set is one gzip'd text file built by `scripts/lucide/bundle.py`
//! (about 110 KB in the binary). It is unpacked the first time an icon is
//! drawn or searched. Icons are served to GPUI as `lucide/<name>.svg`.
use std::{collections::BTreeMap, io::Read as _, sync::OnceLock};

/// The lucide-static release bundled; T3 Code's web app uses the same.
pub const VERSION: &str = "0.564.0";
/// Asset path prefix: `lucide/<name>.svg`.
pub const ASSET_PREFIX: &str = "lucide/";

const BUNDLE: &[u8] = include_bytes!("../assets/lucide/lucide.tsv.gz");
const SVG_OPEN: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">"#;

struct Icon {
    /// Its own name; aliases point at it.
    name: String,
    body: String,
    tags: Vec<String>,
}

#[derive(Default)]
struct Catalog {
    icons: Vec<Icon>,
    /// Every name, aliases included, to its icon.
    index: BTreeMap<String, usize>,
    /// Every name in name order.
    names: Vec<String>,
}

fn parse(text: &str) -> Catalog {
    let mut catalog = Catalog::default();
    let mut aliases = Vec::new();
    for line in text.lines() {
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        let mut fields = line.splitn(3, '\t');
        let (Some(name), Some(body)) = (fields.next(), fields.next()) else {
            continue;
        };
        if let Some(target) = body.strip_prefix('=') {
            aliases.push((name.to_owned(), target.to_owned()));
            continue;
        }
        let tags = fields
            .next()
            .unwrap_or_default()
            .split(',')
            .filter(|tag| !tag.is_empty())
            .map(str::to_owned)
            .collect();
        catalog.index.insert(name.to_owned(), catalog.icons.len());
        catalog.icons.push(Icon {
            name: name.to_owned(),
            body: body.to_owned(),
            tags,
        });
    }
    for (alias, target) in aliases {
        if let Some(&icon) = catalog.index.get(&target) {
            catalog.index.insert(alias, icon);
        }
    }
    catalog.names = catalog.index.keys().cloned().collect();
    catalog
}

fn catalog() -> &'static Catalog {
    static CATALOG: OnceLock<Catalog> = OnceLock::new();
    CATALOG.get_or_init(|| {
        let mut text = String::new();
        match flate2::read::GzDecoder::new(BUNDLE).read_to_string(&mut text) {
            Ok(_) => parse(&text),
            Err(error) => {
                eprintln!("okilum: the bundled Lucide icons are unreadable: {error}");
                Catalog::default()
            }
        }
    })
}

/// Every Lucide name T3 accepts (icons and aliases), in name order.
pub fn names() -> &'static [String] {
    &catalog().names
}

pub fn contains(name: &str) -> bool {
    catalog().index.contains_key(name)
}

/// Lucide's search tags of an icon; none for an alias or an unknown name,
/// so a tag search lists each icon once, under its own name.
pub fn tags(name: &str) -> &'static [String] {
    let catalog = catalog();
    match catalog.index.get(name).map(|&i| &catalog.icons[i]) {
        Some(icon) if icon.name == name => &icon.tags,
        _ => &[],
    }
}

/// The SVG of a Lucide icon or alias.
pub fn svg(name: &str) -> Option<Vec<u8>> {
    let catalog = catalog();
    let icon = &catalog.icons[*catalog.index.get(name)?];
    Some(format!("{SVG_OPEN}{}</svg>", icon.body).into_bytes())
}

/// The asset at `lucide/<name>.svg`, for the app's asset source.
pub fn load(path: &str) -> Option<Vec<u8>> {
    let name = path.strip_prefix(ASSET_PREFIX)?.strip_suffix(".svg")?;
    svg(name)
}

/// Where GPUI finds an icon: `lucide/<name>.svg`.
pub fn asset_path(name: &str) -> String {
    format!("{ASSET_PREFIX}{name}.svg")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_whole_set_is_bundled_with_aliases_and_tags() {
        let names = names();
        assert!(names.len() >= 1900, "{} names", names.len());
        assert!(names.windows(2).all(|pair| pair[0] < pair[1]));
        // Names T3 projects use, aliases among them.
        for name in [
            "palette",
            "brain-cog",
            "globe-2",
            "music",
            "music-2",
            "sparkles",
            "video",
            "briefcase-medical",
            "wallet-minimal",
            "calculator",
            "code-2",
            "layers-3",
            "folder",
        ] {
            assert!(contains(name), "{name}");
            let svg = String::from_utf8(svg(name).unwrap()).unwrap();
            assert!(svg.starts_with("<svg xmlns=\"http://www.w3.org/2000/svg\""));
            assert!(svg.contains("stroke=\"currentColor\""), "{name}");
            assert!(svg.ends_with("</svg>"));
            assert!(svg.len() > SVG_OPEN.len() + 20, "{name} has a body");
            assert_eq!(svg.matches('<').count(), svg.matches('>').count(), "{svg}");
        }
        assert_eq!(svg("globe-2"), svg("earth"), "an alias draws its icon");
        assert!(svg("not-an-icon").is_none());
        assert_eq!(load("lucide/music.svg"), svg("music"));
        assert_eq!(load("icons/music.svg"), None);
        assert_eq!(asset_path("music"), "lucide/music.svg");
        assert!(tags("music").iter().any(|tag| tag == "note"));
        assert!(
            tags("globe-2").is_empty(),
            "an alias has no tags of its own"
        );
        let musical: Vec<_> = names.iter().filter(|name| name.contains("mus")).collect();
        assert_eq!(
            musical,
            [
                "file-music",
                "keyboard-music",
                "list-music",
                "music",
                "music-2",
                "music-3",
                "music-4"
            ],
            "T3 finds these seven for \"mus\""
        );
    }
}

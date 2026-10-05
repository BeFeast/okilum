//! Read-only Excalidraw scene decoding, independent of the desktop renderer.
mod lz;
mod vector;
pub use vector::VectorScene;

use anyhow::{ensure, Context, Result};
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;

pub const MAX_SOURCE_BYTES: usize = 32 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Element {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    #[serde(default)]
    pub angle: f64,
    #[serde(default)]
    pub is_deleted: bool,
    #[serde(flatten)]
    pub properties: BTreeMap<String, Value>,
}
impl Element {
    pub fn number(&self, key: &str, default: f64) -> f64 {
        self.properties
            .get(key)
            .and_then(Value::as_f64)
            .filter(|v| v.is_finite())
            .unwrap_or(default)
    }
    pub fn string<'a>(&'a self, key: &str, default: &'a str) -> &'a str {
        self.properties
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or(default)
    }
    pub fn points(&self) -> Vec<[f64; 2]> {
        self.properties
            .get("points")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|p| Some([p.get(0)?.as_f64()?, p.get(1)?.as_f64()?]))
            .collect()
    }
}

#[derive(Clone, Debug)]
pub struct Scene {
    /// Exact scene data for the explicit Copy scene / Open in Excalidraw action.
    pub json: Value,
    pub elements: Vec<Element>,
    /// Obsidian image references live outside the compressed scene.
    pub embedded_files: BTreeMap<String, String>,
}

pub fn is_drawing(path: &str) -> bool {
    let path = path.to_ascii_lowercase();
    path.ends_with(".excalidraw") || path.ends_with(".excalidraw.md")
}

impl Scene {
    pub fn parse(source: &str) -> Result<Self> {
        ensure!(
            source.len() <= MAX_SOURCE_BYTES,
            "Drawing file is too large"
        );
        let source = source.trim_start_matches('\u{feff}');
        let mut embedded_files = BTreeMap::new();
        let json_text = if source.trim_start().starts_with('{') {
            source.to_owned()
        } else {
            let mut drawing = false;
            let mut embedded = false;
            let mut encoding = None;
            let mut body = String::new();
            let mut closed = false;
            for line in source.lines() {
                let line = line.trim();
                if let Some(kind) = encoding {
                    if line == "```" {
                        closed = true;
                        break;
                    }
                    body.push_str(line);
                    if kind == "json" {
                        body.push('\n');
                    }
                    continue;
                }
                if line == "## Drawing" {
                    drawing = true;
                    embedded = false;
                    continue;
                }
                if line == "## Embedded Files" {
                    embedded = true;
                    continue;
                }
                if embedded {
                    if let Some((id, target)) = line.split_once(": ") {
                        if let Some(target) =
                            target.strip_prefix("[[").and_then(|s| s.strip_suffix("]]"))
                        {
                            embedded_files.insert(id.to_owned(), target.to_owned());
                        }
                    }
                }
                if drawing && (line == "```json" || line == "```compressed-json") {
                    encoding = line.strip_prefix("```");
                }
            }
            ensure!(closed, "Missing or truncated Drawing JSON block");
            if encoding == Some("compressed-json") {
                lz::decompress(&body)?
            } else {
                body
            }
        };
        let json: Value = serde_json::from_str(&json_text).context("Invalid Excalidraw JSON")?;
        ensure!(
            json.get("type").and_then(Value::as_str) == Some("excalidraw"),
            "Not an Excalidraw scene"
        );
        let raw = json
            .get("elements")
            .and_then(Value::as_array)
            .context("Drawing has no element array")?;
        ensure!(raw.len() <= 10_000, "Drawing has too many elements");
        let mut elements = Vec::new();
        for raw in raw {
            if raw.get("isDeleted").and_then(Value::as_bool) == Some(true) {
                continue;
            }
            let e: Element =
                serde_json::from_value(raw.clone()).context("Invalid drawing element")?;
            ensure!(
                [e.x, e.y, e.width, e.height, e.angle]
                    .iter()
                    .all(|n| n.is_finite() && n.abs() <= 1_000_000.),
                "Invalid drawing geometry"
            );
            ensure!(
                e.width >= 0. && e.height >= 0.,
                "Negative drawing dimensions"
            );
            if let Some(points) = e.properties.get("points") {
                let points = points.as_array().context("Invalid drawing points")?;
                ensure!(
                    points.len() <= 20_000
                        && points
                            .iter()
                            .all(|p| p.as_array().is_some_and(|p| p.len() == 2
                                && p.iter().all(|v| v
                                    .as_f64()
                                    .is_some_and(|v| v.is_finite() && v.abs() <= 1_000_000.)))),
                    "Invalid drawing points"
                );
            }
            elements.push(e);
        }
        Ok(Self {
            json,
            elements,
            embedded_files,
        })
    }
    pub fn background(&self) -> &str {
        self.json
            .pointer("/appState/viewBackgroundColor")
            .and_then(Value::as_str)
            .unwrap_or("#ffffff")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const SCENE: &str = include_str!("../../tests/fixtures/excalidraw/elements.excalidraw");
    #[test]
    fn plain_and_obsidian_encodings_preserve_the_same_scene() {
        let plain = Scene::parse(SCENE).unwrap();
        let markdown = Scene::parse(&format!(
            "---\nexcalidraw-plugin: parsed\n---\n## Drawing\n```json\n{SCENE}\n```\n"
        ))
        .unwrap();
        let compressed = Scene::parse(include_str!(
            "../../tests/fixtures/excalidraw/elements.excalidraw.md"
        ))
        .unwrap();
        assert_eq!(plain.json, markdown.json);
        assert_eq!(plain.json, compressed.json);
        assert_eq!(plain.elements.len(), 9);
        assert_eq!(
            compressed.embedded_files.get("fixture-image").unwrap(),
            "assets/example.png"
        );
        assert_eq!(plain.background(), "#fefefe");
    }
    #[test]
    fn corrupt_input_is_an_error_and_empty_scene_is_valid() {
        for source in [
            "",
            "## Drawing\n```compressed-json\n!bad\n```",
            "## Drawing\n```json\n{}",
            r#"{"type":"other","elements":[]}"#,
        ] {
            assert!(Scene::parse(source).is_err());
        }
        assert!(Scene::parse(r#"{"type":"excalidraw","elements":[]}"#)
            .unwrap()
            .elements
            .is_empty());
        assert!(is_drawing("diagram.EXCALIDRAW.md"));
        assert!(!is_drawing("diagram.md"));
    }
    #[test]
    fn invalid_geometry_and_truncated_compression_are_rejected() {
        let mut value: Value = serde_json::from_str(SCENE).unwrap();
        value["elements"][0]["width"] = (-1).into();
        assert!(Scene::parse(&value.to_string()).is_err());
        assert!(lz::decompress("N4I").is_err());
    }
}

#[cfg(test)]
mod embed_tests {
    #[test]
    fn embeds_resolve_plugin_notes_and_keep_size_and_code_boundaries() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("drawing.excalidraw.md"), "# Drawing").unwrap();
        std::fs::write(temp.path().join("note.md"), "# Note").unwrap();
        let vault = crate::Vault::scan(temp.path()).unwrap();
        let source = "![[drawing.excalidraw|300x200]]\n\n`![[drawing.excalidraw]]`";
        let prepared = crate::render::preprocess(source);
        assert!(prepared.contains("tessera-drawing-size:300x200"));
        assert!(prepared.contains("`![[drawing.excalidraw]]`"));
        let rewritten = crate::render::rewrite_source_images(&prepared, &vault, "note.md");
        assert!(rewritten.contains("file://"), "{rewritten}");
        assert!(rewritten.contains("\"tessera-drawing-size:300x200\""));
        assert!(rewritten.contains("`![[drawing.excalidraw]]`"));
        assert!(rewritten.contains("drawing.excalidraw.md"));
        std::fs::write(
            temp.path().join("note.md"),
            "![[drawing.excalidraw.md|300]]",
        )
        .unwrap();
        let document = crate::render::reader_document(&vault, "note.md").unwrap();
        assert!(document.rendered.contains("file://"));
        assert!(!document.rendered.contains(crate::render::EMBED_LANG));
        let missing = crate::render::rewrite_source_images(
            &crate::render::preprocess("![[missing.excalidraw]]"),
            &vault,
            "note.md",
        );
        assert!(missing.contains("tessera-drawing-unavailable:"));
    }
}

//! Bounded, read-only JSON Canvas 1.0 model and shared viewport geometry.
use anyhow::{ensure, Result};
use serde::Deserialize;
use std::collections::HashSet;

pub const MAX_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_NODES: usize = 4000;
pub const MAX_EDGES: usize = 8000;

#[derive(Clone, Debug, Deserialize)]
pub struct Scene {
    #[serde(default)]
    pub nodes: Vec<Node>,
    #[serde(default)]
    pub edges: Vec<Edge>,
}
#[derive(Clone, Debug, Deserialize)]
pub struct Node {
    pub id: String,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub color: Option<String>,
    #[serde(flatten)]
    pub content: Content,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Content {
    Text {
        text: String,
    },
    File {
        file: String,
        subpath: Option<String>,
    },
    Link {
        url: String,
    },
    Group {
        label: Option<String>,
        background: Option<String>,
    },
    #[serde(other)]
    Unsupported,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Edge {
    pub id: String,
    pub from_node: String,
    pub to_node: String,
    pub from_side: Option<Side>,
    pub to_side: Option<Side>,
    pub from_end: Option<String>,
    pub to_end: Option<String>,
    pub color: Option<String>,
    pub label: Option<String>,
}
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    Top,
    Right,
    Bottom,
    Left,
}

impl Scene {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        ensure!(bytes.len() <= MAX_BYTES, "Canvas exceeds 8 MiB");
        let scene: Self = serde_json::from_slice(bytes)?;
        ensure!(scene.nodes.len() <= MAX_NODES, "Canvas exceeds 4000 cards");
        ensure!(scene.edges.len() <= MAX_EDGES, "Canvas exceeds 8000 edges");
        let mut ids = HashSet::new();
        for node in &scene.nodes {
            ensure!(
                !node.id.is_empty() && ids.insert(&node.id),
                "Duplicate or empty Canvas card ID: {}",
                node.id
            );
            ensure!(
                [node.x, node.y, node.width, node.height]
                    .iter()
                    .all(|v| v.is_finite() && v.abs() <= 1_000_000.)
                    && node.width > 0.
                    && node.height > 0.,
                "Invalid Canvas card geometry: {}",
                node.id
            );
        }
        let mut edges = HashSet::new();
        for edge in &scene.edges {
            ensure!(
                !edge.id.is_empty() && edges.insert(&edge.id),
                "Duplicate or empty Canvas edge ID: {}",
                edge.id
            );
        }
        Ok(scene)
    }

    pub fn bounds(&self) -> [f32; 4] {
        if self.nodes.is_empty() {
            return [0., 0., 1., 1.];
        }
        self.nodes.iter().fold(
            [
                f32::INFINITY,
                f32::INFINITY,
                f32::NEG_INFINITY,
                f32::NEG_INFINITY,
            ],
            |b, n| {
                [
                    b[0].min(n.x),
                    b[1].min(n.y),
                    b[2].max(n.x + n.width),
                    b[3].max(n.y + n.height),
                ]
            },
        )
    }

    pub fn missing_edges(&self) -> usize {
        let ids: HashSet<_> = self.nodes.iter().map(|n| n.id.as_str()).collect();
        self.edges
            .iter()
            .filter(|e| !ids.contains(e.from_node.as_str()) || !ids.contains(e.to_node.as_str()))
            .count()
    }
}
impl Node {
    pub fn anchor(&self, side: Option<Side>) -> [f32; 2] {
        match side {
            Some(Side::Top) => [self.x + self.width / 2., self.y],
            Some(Side::Right) => [self.x + self.width, self.y + self.height / 2.],
            Some(Side::Bottom) => [self.x + self.width / 2., self.y + self.height],
            Some(Side::Left) => [self.x, self.y + self.height / 2.],
            None => [self.x + self.width / 2., self.y + self.height / 2.],
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Viewport {
    pub scale: f32,
    pub x: f32,
    pub y: f32,
}
impl Default for Viewport {
    fn default() -> Self {
        Self {
            scale: 1.,
            x: 0.,
            y: 0.,
        }
    }
}
impl Viewport {
    pub fn fit(bounds: [f32; 4], width: f32, height: f32) -> Self {
        let scale = ((width - 48.).max(1.) / (bounds[2] - bounds[0]).max(1.))
            .min((height - 48.).max(1.) / (bounds[3] - bounds[1]).max(1.))
            .min(1.);
        Self {
            scale,
            x: (width - (bounds[2] + bounds[0]) * scale) / 2.,
            y: (height - (bounds[3] + bounds[1]) * scale) / 2.,
        }
    }
    pub fn point(self, point: [f32; 2]) -> [f32; 2] {
        [
            point[0] * self.scale + self.x,
            point[1] * self.scale + self.y,
        ]
    }
    pub fn zoom(&mut self, factor: f32, anchor: [f32; 2]) {
        let old = self.scale;
        self.scale = (old * factor).clamp(0.0001, 8.);
        self.x = anchor[0] - (anchor[0] - self.x) * self.scale / old;
        self.y = anchor[1] - (anchor[1] - self.y) * self.scale / old;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const SAMPLE: &[u8] = include_bytes!("../tests/fixtures/viewers/canvas/read-only.canvas");

    #[test]
    fn json_canvas_sample_preserves_card_types_edges_and_negative_geometry() {
        let scene = Scene::parse(SAMPLE).unwrap();
        assert_eq!(scene.nodes.len(), 5);
        assert_eq!(scene.edges.len(), 2);
        assert!(matches!(scene.nodes[0].content, Content::Group { .. }));
        assert!(matches!(scene.nodes[1].content, Content::Text { .. }));
        assert!(matches!(scene.nodes[2].content, Content::File { .. }));
        assert!(matches!(scene.nodes[3].content, Content::Link { .. }));
        assert_eq!(scene.bounds(), [-440., -240., 440., 300.]);
        assert_eq!(scene.missing_edges(), 0);
        assert_eq!(scene.nodes[1].anchor(Some(Side::Right)), [-80., -90.]);
    }

    #[test]
    fn malformed_duplicate_and_unbounded_scenes_fail_but_missing_edges_stay_visible() {
        assert!(Scene::parse(b"not JSON").is_err());
        assert!(Scene::parse(&vec![b' '; MAX_BYTES + 1]).is_err());
        let mut value: serde_json::Value = serde_json::from_slice(SAMPLE).unwrap();
        value["nodes"][1]["id"] = value["nodes"][0]["id"].clone();
        assert!(Scene::parse(&serde_json::to_vec(&value).unwrap()).is_err());
        value = serde_json::from_slice(SAMPLE).unwrap();
        value["nodes"][1]["width"] = (-1).into();
        assert!(Scene::parse(&serde_json::to_vec(&value).unwrap()).is_err());
        value = serde_json::from_slice(SAMPLE).unwrap();
        value["edges"][0]["toNode"] = "missing-id".into();
        assert_eq!(
            Scene::parse(&serde_json::to_vec(&value).unwrap())
                .unwrap()
                .missing_edges(),
            1
        );
        value["nodes"][1]["type"] = "future-card".into();
        assert!(matches!(
            Scene::parse(&serde_json::to_vec(&value).unwrap())
                .unwrap()
                .nodes[1]
                .content,
            Content::Unsupported
        ));
    }

    #[test]
    fn fit_contains_the_scene_and_zoom_keeps_the_pointer_anchor() {
        let scene = Scene::parse(SAMPLE).unwrap();
        let mut view = Viewport::fit(scene.bounds(), 900., 650.);
        for node in &scene.nodes {
            let top = view.point([node.x, node.y]);
            let bottom = view.point([node.x + node.width, node.y + node.height]);
            assert!(top[0] >= 23. && top[1] >= 23.);
            assert!(bottom[0] <= 877. && bottom[1] <= 627.);
        }
        let anchor = view.point([-120., 75.]);
        view.zoom(1.7, anchor);
        let after = view.point([-120., 75.]);
        assert!((after[0] - anchor[0]).abs() < 0.001);
        assert!((after[1] - anchor[1]).abs() < 0.001);
        assert_eq!(Scene::parse(br#"{}"#).unwrap().bounds(), [0., 0., 1., 1.]);
    }
}

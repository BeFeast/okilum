//! Read-only note graph for the rows selected by a view query.
use super::*;
use okilum_core::{records::Projection, view::Section};
use std::collections::BTreeSet;

const COLUMNS: usize = 3;
const NODE_WIDTH: f32 = 132.;
const NODE_HEIGHT: f32 = 36.;
const COLUMN_STEP: f32 = 166.;
const ROW_STEP: f32 = 72.;

pub(super) struct Graph {
    nodes: Vec<Node>,
    edges: Arc<[(usize, usize)]>,
    ambiguous: Vec<String>,
}

struct Node {
    path: String,
    title: String,
}

impl Graph {
    pub(super) fn build(section: &Section, projection: &Projection, vault: &Vault) -> Self {
        // The query owns filtering, including links at depth two and the output
        // budget. Only its returned source notes can become graph nodes.
        let mut nodes = Vec::new();
        let mut indices = BTreeMap::new();
        for row in &section.rows {
            let Some(note) = projection.note(&row.path) else {
                continue;
            };
            let entry = indices.entry(row.path.as_str());
            if let std::collections::btree_map::Entry::Vacant(entry) = entry {
                entry.insert(nodes.len());
                let title = match note.fields.get("title") {
                    Some(okilum_core::records::Value::String(title)) => title.clone(),
                    _ => Vault::title_of(&row.path),
                };
                nodes.push(Node {
                    path: row.path.clone(),
                    title,
                });
            }
        }

        let mut edges = BTreeSet::new();
        let mut ambiguities = BTreeMap::<_, BTreeSet<_>>::new();
        if !nodes.is_empty() {
            for target in &vault.notes {
                for link in vault.backlinks(&target.path) {
                    let Some(&source) = indices.get(link.path.as_str()) else {
                        continue;
                    };
                    if link.ambiguous {
                        // Include candidates outside the result as well. Even
                        // one visible candidate cannot make a link resolved.
                        ambiguities
                            .entry((source, link.link))
                            .or_default()
                            .insert(target.path.as_str());
                    } else if let Some(&destination) = indices.get(target.path.as_str()) {
                        edges.insert((source, destination));
                    }
                }
            }
        }
        let ambiguous = ambiguities
            .into_iter()
            .map(|((source, link), candidates)| {
                let title = &nodes[source].title;
                let paths = candidates.into_iter().collect::<Vec<_>>().join(", ");
                format!("Ambiguous link in {title} · {link} → {paths}")
            })
            .collect();
        Self {
            nodes,
            edges: edges.into_iter().collect::<Vec<_>>().into(),
            ambiguous,
        }
    }

    pub(super) fn render(
        &self,
        reader: &WeakEntity<Reader>,
        root: &Path,
        cx: &App,
    ) -> impl IntoElement {
        let colors = brand::palette(cx);
        if self.nodes.is_empty() {
            return div()
                .text_color(colors.text_muted)
                .child("No source notes to graph.");
        }
        let columns = self.nodes.len().min(COLUMNS);
        let rows = self.nodes.len().div_ceil(COLUMNS);
        let width = NODE_WIDTH + (columns - 1) as f32 * COLUMN_STEP;
        let mut scene = div()
            .relative()
            .w(px(width))
            .h(px(rows as f32 * ROW_STEP))
            .flex_shrink_0()
            .child(edges(self.edges.clone(), colors.accent));
        for (index, node) in self.nodes.iter().enumerate() {
            let [x, y] = position(index);
            let reader = reader.clone();
            let root = root.to_path_buf();
            let path = node.path.clone();
            scene = scene.child(
                div()
                    .absolute()
                    .left(px(x))
                    .top(px(y))
                    .w(px(NODE_WIDTH))
                    .h(px(NODE_HEIGHT))
                    .child(
                        Button::new(SharedString::from(node.path.clone()))
                            .small()
                            .ghost()
                            .size_full()
                            .justify_start()
                            .border_1()
                            .border_color(colors.border_subtle)
                            .bg(colors.surface_raised)
                            .tooltip(format!("Open {}", node.path))
                            .accessibility_label(node.title.clone())
                            .child(
                                h_flex()
                                    .w_full()
                                    .min_w_0()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        div()
                                            .size(px(11.))
                                            .flex_shrink_0()
                                            .rounded_full()
                                            .bg(colors.accent),
                                    )
                                    .child(div().min_w_0().truncate().child(node.title.clone())),
                            )
                            .on_click(move |_, window, cx| {
                                let _ = reader.update(cx, |this, cx| {
                                    if this.vault_root == root {
                                        this.open_note_at(&path, None, None, window, cx);
                                    }
                                });
                            }),
                    ),
            );
        }
        div()
            .w_full()
            .min_w_0()
            .child(
                div()
                    .id("view-graph-scroll")
                    .w_full()
                    .min_w_0()
                    .overflow_x_scroll()
                    .restrict_scroll_to_axis()
                    .child(scene),
            )
            .children(self.ambiguous.iter().map(|warning| {
                div()
                    .text_sm()
                    .text_color(colors.text_muted)
                    .whitespace_normal()
                    .child(warning.clone())
            }))
    }
}

fn position(index: usize) -> [f32; 2] {
    [
        (index % COLUMNS) as f32 * COLUMN_STEP,
        3. + (index / COLUMNS) as f32 * ROW_STEP,
    ]
}

fn edges(edges: Arc<[(usize, usize)]>, accent: Hsla) -> impl IntoElement {
    gpui::canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            let at = |x, y| point(bounds.origin.x + px(x), bounds.origin.y + px(y));
            for &(source, destination) in edges.iter() {
                let [sx, sy] = position(source);
                let [dx, dy] = position(destination);
                let mut path = gpui::PathBuilder::stroke(px(2.));
                let (tip, direction) = if (sy == dy) && source.abs_diff(destination) == 1 {
                    let right = sx < dx;
                    let (from, to) = if right {
                        (sx + NODE_WIDTH, dx - 1.)
                    } else {
                        (sx, dx + NODE_WIDTH + 1.)
                    };
                    let y = sy + NODE_HEIGHT * if right { 0.4 } else { 0.6 };
                    path.move_to(at(from, y));
                    path.line_to(at(to, y));
                    ([to, y], [if right { 1. } else { -1. }, 0.])
                } else {
                    let upward = sy > dy;
                    let from_x = sx + NODE_WIDTH * 0.6;
                    let to_x = dx + NODE_WIDTH * 0.4;
                    let from_y = if upward { sy } else { sy + NODE_HEIGHT };
                    let to_y = if sy >= dy {
                        dy + NODE_HEIGHT + 1.
                    } else {
                        dy - 1.
                    };
                    let bend = if sy == dy {
                        from_y + 28.
                    } else {
                        (from_y + to_y) / 2.
                    };
                    path.move_to(at(from_x, from_y));
                    path.cubic_bezier_to(at(to_x, to_y), at(from_x, bend), at(to_x, bend));
                    ([to_x, to_y], [0., if sy >= dy { -1. } else { 1. }])
                };
                for side in [-1., 1.] {
                    path.move_to(at(tip[0], tip[1]));
                    path.line_to(at(
                        tip[0] - direction[0] * 6. + direction[1] * 3. * side,
                        tip[1] - direction[1] * 6. + direction[0] * 3. * side,
                    ));
                }
                if let Ok(path) = path.build() {
                    window.paint_path(path, accent);
                }
            }
        },
    )
    .absolute()
    .size_full()
}

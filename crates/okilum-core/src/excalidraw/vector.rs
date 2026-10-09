//! Generate a native-renderable SVG display list directly from scene elements.
use super::{Element, Scene};
use anyhow::{bail, ensure, Result};
use euclid::default::Point2D;
use roughr::{
    core::{FillStyle, OpSet, OpSetType, OpType, Options},
    generator::Generator,
};
use std::fmt::Write;

pub struct VectorScene {
    pub svg: String,
    pub width: f64,
    pub height: f64,
    pub warnings: Vec<String>,
}

fn xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
fn color(text: &str) -> Option<palette::Srgba> {
    if text == "transparent" {
        return None;
    }
    let hex = text.strip_prefix('#')?;
    let value = u32::from_str_radix(hex, 16).ok()?;
    let (r, g, b) = match hex.len() {
        3 => (
            ((value >> 8) & 15) * 17,
            ((value >> 4) & 15) * 17,
            (value & 15) * 17,
        ),
        6 => ((value >> 16) & 255, (value >> 8) & 255, value & 255),
        _ => return None,
    };
    Some(palette::Srgba::new(
        r as f32 / 255.,
        g as f32 / 255.,
        b as f32 / 255.,
        1.,
    ))
}
fn css(c: Option<palette::Srgba>) -> String {
    c.map(|c| {
        format!(
            "#{:02x}{:02x}{:02x}",
            (c.red * 255.).round() as u8,
            (c.green * 255.).round() as u8,
            (c.blue * 255.).round() as u8
        )
    })
    .unwrap_or_else(|| "none".into())
}
fn stroke(e: &Element) -> String {
    css(color(e.string("strokeColor", "#1e1e1e")))
}

impl Scene {
    /// The caller resolves image IDs to safe local data URLs. No network or
    /// filesystem URLs from scene JSON are copied to the display list.
    pub fn vectors(&self, mut image: impl FnMut(&str) -> Option<String>) -> Result<VectorScene> {
        let mut bounds = [
            f64::INFINITY,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NEG_INFINITY,
        ];
        for e in &self.elements {
            ensure!(
                e.width <= 20_000. && e.height <= 20_000.,
                "Drawing element is too large to render"
            );
            let center = [e.x + e.width / 2., e.y + e.height / 2.];
            let points = if matches!(e.kind.as_str(), "line" | "arrow" | "freedraw") {
                e.points()
                    .into_iter()
                    .map(|p| [e.x + p[0], e.y + p[1]])
                    .collect::<Vec<_>>()
            } else {
                vec![
                    [e.x, e.y],
                    [e.x + e.width, e.y],
                    [e.x, e.y + e.height],
                    [e.x + e.width, e.y + e.height],
                ]
            };
            for p in points {
                let dx = p[0] - center[0];
                let dy = p[1] - center[1];
                let x = center[0] + dx * e.angle.cos() - dy * e.angle.sin();
                let y = center[1] + dx * e.angle.sin() + dy * e.angle.cos();
                bounds[0] = bounds[0].min(x);
                bounds[1] = bounds[1].min(y);
                bounds[2] = bounds[2].max(x);
                bounds[3] = bounds[3].max(y);
            }
        }
        if !bounds[0].is_finite() {
            bounds = [0., 0., 320., 180.];
        }
        let padding = 24.;
        let width = (bounds[2] - bounds[0] + padding * 2.).max(1.);
        let height = (bounds[3] - bounds[1] + padding * 2.).max(1.);
        ensure!(
            width <= 200_000. && height <= 200_000.,
            "Drawing viewport is too large"
        );
        let mut svg = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="{width}" height="{height}" viewBox="{} {} {width} {height}">"#,
            bounds[0] - padding,
            bounds[1] - padding
        );
        let dark = self
            .json
            .pointer("/appState/theme")
            .and_then(|v| v.as_str())
            == Some("dark");
        let frames = self
            .elements
            .iter()
            .enumerate()
            .filter(|(_, e)| e.kind == "frame")
            .map(|(i, e)| (e.id.as_str(), (i, e)))
            .collect::<std::collections::BTreeMap<_, _>>();
        for (i, frame) in frames.values() {
            write!(svg, r#"<defs><clipPath id="frame-{i}"><rect x="{}" y="{}" width="{}" height="{}" transform="rotate({} {} {})"/></clipPath></defs>"#,frame.x,frame.y,frame.width,frame.height,frame.angle.to_degrees(),frame.x+frame.width/2.,frame.y+frame.height/2.).unwrap();
        }
        write!(
            svg,
            r#"<rect x="{}" y="{}" width="{width}" height="{height}" fill="{}"/>"#,
            bounds[0] - padding,
            bounds[1] - padding,
            css(color(self.background()))
        )
        .unwrap();
        if dark {
            svg.push_str(r#"<defs><filter id="scene-dark" color-interpolation-filters="sRGB"><feColorMatrix type="matrix" values="-0.86 0 0 0 0.93 0 -0.86 0 0 0.93 0 0 -0.86 0 0.93 0 0 0 1 0"/><feColorMatrix type="hueRotate" values="180"/></filter></defs><g filter="url(#scene-dark)">"#);
        }
        let mut warnings = Vec::new();
        let generator = Generator::default();
        for e in &self.elements {
            let frame = frames.get(e.string("frameId", ""));
            if let Some((i, _)) = frame {
                write!(svg, r#"<g clip-path="url(#frame-{i})">"#).unwrap();
            }
            let opacity = (e.number("opacity", 100.) / 100.).clamp(0., 1.);
            write!(
                svg,
                r#"<g opacity="{opacity}" transform="translate({} {}) rotate({} {} {})">"#,
                e.x,
                e.y,
                e.angle.to_degrees(),
                e.width / 2.,
                e.height / 2.
            )
            .unwrap();
            match e.kind.as_str() {
                "text" => text(&mut svg, e),
                "image" => {
                    if let Some(url) = image(e.string("fileId", "")) {
                        ensure!(
                            url.starts_with("data:image/"),
                            "Drawing image must be local data"
                        );
                        let scale = e.properties.get("scale").and_then(|v| v.as_array());
                        let flip_x = scale
                            .and_then(|v| v.first())
                            .and_then(|v| v.as_f64())
                            .is_some_and(|v| v < 0.);
                        let flip_y = scale
                            .and_then(|v| v.get(1))
                            .and_then(|v| v.as_f64())
                            .is_some_and(|v| v < 0.);
                        write!(svg,r#"<image width="{}" height="{}" preserveAspectRatio="none" transform="translate({} {}) scale({} {})" xlink:href="{}"/>"#,e.width,e.height,if flip_x{e.width}else{0.},if flip_y{e.height}else{0.},if flip_x{-1}else{1},if flip_y{-1}else{1},xml(&url)).unwrap();
                    } else {
                        warnings.push(format!(
                            "Image unavailable: {}",
                            e.string("fileId", "unknown")
                        ));
                        write!(svg,r##"<rect width="{}" height="{}" fill="none" stroke="#868e96" stroke-dasharray="6 4"/><text x="8" y="20" font-family="sans-serif" font-size="14" fill="#868e96">Image unavailable</text>"##,e.width,e.height).unwrap();
                    }
                }
                "frame" => {
                    write!(svg,r##"<rect width="{}" height="{}" rx="8" fill="none" stroke="#868e96" stroke-width="1"/>"##,e.width,e.height).unwrap();
                    if let Some(name) = e.properties.get("name").and_then(|v| v.as_str()) {
                        write!(svg,r##"<text y="-8" font-family="sans-serif" font-size="14" fill="#868e96">{}</text>"##,xml(name)).unwrap();
                    }
                }
                "freedraw" => freedraw(&mut svg, e),
                "rectangle" | "ellipse" | "diamond" | "line" | "arrow" => {
                    shape(&mut svg, e, &generator)?
                }
                other => {
                    warnings.push(format!("Unsupported drawing element: {other}"));
                }
            }
            svg.push_str("</g>");
            if frame.is_some() {
                svg.push_str("</g>");
            }
            ensure!(
                svg.len() <= 64 * 1024 * 1024,
                "Drawing display list is too large"
            );
        }
        if dark {
            svg.push_str("</g>");
        }
        svg.push_str("</svg>");
        Ok(VectorScene {
            svg,
            width,
            height,
            warnings,
        })
    }
}

fn text(svg: &mut String, e: &Element) {
    let font = match e.number("fontFamily", 1.) as u32 {
        1 => "Virgil",
        2 => "Liberation Sans",
        3 => "Cascadia Code",
        5 => "Excalifont",
        6 => "Nunito",
        _ => "Excalifont",
    };
    let size = e.number("fontSize", 20.).clamp(1., 1000.);
    let line_height = e.number("lineHeight", 1.25).clamp(0.5, 4.);
    let (x, anchor) = match e.string("textAlign", "left") {
        "center" => (e.width / 2., "middle"),
        "right" => (e.width, "end"),
        _ => (0., "start"),
    };
    let content = e.string("text", "");
    // Excalidraw stores the already-wrapped lines and final bound-text position.
    for (i, line) in content.split('\n').enumerate() {
        let y = size * 0.8 + i as f64 * size * line_height;
        write!(svg,r#"<text x="{x}" y="{y}" font-family="{font}" font-size="{size}" text-anchor="{anchor}" fill="{}" xml:space="preserve">{}</text>"#,stroke(e),xml(line)).unwrap();
    }
}

fn shape(svg: &mut String, e: &Element, g: &Generator) -> Result<()> {
    let width = e.number("strokeWidth", 1.).clamp(0.1, 100.);
    let options = Some(Options {
        seed: Some(e.number("seed", 1.) as u64),
        roughness: Some(e.number("roughness", 1.).clamp(0., 3.) as f32),
        stroke: color(e.string("strokeColor", "#1e1e1e")),
        stroke_width: Some(width as f32),
        fill: color(e.string("backgroundColor", "transparent")),
        fill_style: Some(match e.string("fillStyle", "hachure") {
            "solid" => FillStyle::Solid,
            "cross-hatch" => FillStyle::CrossHatch,
            _ => FillStyle::Hachure,
        }),
        hachure_gap: Some((width * 4.).max(4.) as f32),
        fill_weight: Some(width as f32 / 2.),
        ..Default::default()
    });
    let points = e
        .points()
        .into_iter()
        .map(|p| Point2D::new(p[0], p[1]))
        .collect::<Vec<_>>();
    let drawable = match e.kind.as_str() {
        "rectangle" if e.properties.get("roundness").is_some_and(|v| !v.is_null()) => {
            let r = (e.width.min(e.height) * 0.25).min(32.);
            g.path(format!("M {r} 0 H {} Q {} 0 {} {r} V {} Q {} {} {} {} H {r} Q 0 {} 0 {} V {r} Q 0 0 {r} 0 Z",e.width-r,e.width,e.width,e.height-r,e.width,e.height,e.width-r,e.height,e.height,e.height-r),&options)
        }
        "rectangle" => g.rectangle(0., 0., e.width, e.height, &options),
        "ellipse" => g.ellipse(e.width / 2., e.height / 2., e.width, e.height, &options),
        "diamond" => g.polygon(
            &[
                Point2D::new(e.width / 2., 0.),
                Point2D::new(e.width, e.height / 2.),
                Point2D::new(e.width / 2., e.height),
                Point2D::new(0., e.height / 2.),
            ],
            &options,
        ),
        "line" | "arrow" => {
            if points.len() < 2 {
                return Ok(());
            }
            if e.properties.get("roundness").is_some_and(|v| !v.is_null()) && points.len() > 2 {
                g.curve(&points, &options)
            } else {
                g.linear_path(&points, false, &options)
            }
        }
        _ => bail!("Invalid shape"),
    };
    let dash = match e.string("strokeStyle", "solid") {
        "dashed" => format!(r#" stroke-dasharray="{} {}""#, width * 8., width * 8.),
        "dotted" => format!(r#" stroke-dasharray="{} {}""#, width, width * 5.),
        _ => String::new(),
    };
    // roughr 0.14's ops_to_path writes Move as L, producing invalid SVG.
    // Use its geometry operations directly; Okilum owns display-list encoding.
    for set in &drawable.sets {
        let options = &drawable.options;
        let (stroke, stroke_width, fill) = match set.op_set_type {
            OpSetType::Path => (options.stroke, options.stroke_width.unwrap_or(1.), None),
            OpSetType::FillPath => (None, 0., options.fill),
            OpSetType::FillSketch => (options.fill, options.fill_weight.unwrap_or(0.5), None),
        };
        write!(svg,r#"<path d="{}" stroke="{}" stroke-width="{stroke_width}" fill="{}" stroke-linecap="round" stroke-linejoin="round"{dash}/>"#,ops_path(set),css(stroke),css(fill)).unwrap();
    }
    if e.kind == "arrow" && points.len() >= 2 {
        arrowhead(
            svg,
            e,
            e.string("startArrowhead", ""),
            points[0],
            points[1],
            width,
        );
        arrowhead(
            svg,
            e,
            if e.properties
                .get("endArrowhead")
                .is_some_and(|v| v.is_null())
            {
                ""
            } else {
                e.string("endArrowhead", "arrow")
            },
            points[points.len() - 1],
            points[points.len() - 2],
            width,
        );
    }
    Ok(())
}

fn ops_path(set: &OpSet<f64>) -> String {
    let mut path = String::new();
    for op in &set.ops {
        let d = &op.data;
        match op.op {
            OpType::Move => write!(path, "M {} {} ", d[0], d[1]),
            OpType::LineTo => write!(path, "L {} {} ", d[0], d[1]),
            OpType::BCurveTo => write!(
                path,
                "C {} {} {} {} {} {} ",
                d[0], d[1], d[2], d[3], d[4], d[5]
            ),
        }
        .unwrap();
    }
    path
}

fn arrowhead(
    svg: &mut String,
    e: &Element,
    kind: &str,
    tip: Point2D<f64>,
    previous: Point2D<f64>,
    width: f64,
) {
    if kind.is_empty() {
        return;
    }
    let angle = (tip.y - previous.y).atan2(tip.x - previous.x).to_degrees();
    let length = (width * 4. + 8.).min((tip - previous).length() * 0.5);
    write!(svg,r#"<g transform="translate({} {}) rotate({angle})" stroke="{}" stroke-width="{width}" stroke-linecap="round" stroke-linejoin="round">"#,tip.x,tip.y,stroke(e)).unwrap();
    match kind {
        "dot" | "circle" | "circle_outline" => {
            write!(
                svg,
                r#"<circle r="{}" fill="{}"/>"#,
                length / 2.,
                if kind.ends_with("outline") {
                    "none".into()
                } else {
                    stroke(e)
                }
            )
            .unwrap();
        }
        "bar" => {
            write!(
                svg,
                r#"<path d="M 0 {} L 0 {}" fill="none"/>"#,
                -length / 2.,
                length / 2.
            )
            .unwrap();
        }
        "diamond" | "diamond_outline" => {
            write!(
                svg,
                r#"<path d="M 0 0 L {} {} L {} 0 L {} {} Z" fill="{}"/>"#,
                -length / 2.,
                -length / 3.,
                -length,
                -length / 2.,
                length / 3.,
                if kind.ends_with("outline") {
                    "none".into()
                } else {
                    stroke(e)
                }
            )
            .unwrap();
        }
        _ => {
            let close = if kind.starts_with("triangle") {
                " Z"
            } else {
                ""
            };
            write!(
                svg,
                r#"<path d="M {} {} L 0 0 L {} {}{close}" fill="{}"/>"#,
                -length,
                -length / 2.,
                -length,
                length / 2.,
                if kind == "triangle" {
                    stroke(e)
                } else {
                    "none".into()
                }
            )
            .unwrap();
        }
    }
    svg.push_str("</g>");
}

fn freedraw(svg: &mut String, e: &Element) {
    let points = e.points();
    if points.is_empty() {
        return;
    }
    let width = e.number("strokeWidth", 1.).clamp(0.1, 100.) * 1.5;
    if points.len() == 1 {
        write!(
            svg,
            r#"<circle cx="{}" cy="{}" r="{width}" fill="{}"/>"#,
            points[0][0],
            points[0][1],
            stroke(e)
        )
        .unwrap();
        return;
    }
    let mut d = format!("M {} {}", points[0][0], points[0][1]);
    for p in points.iter().skip(1) {
        write!(d, " L {} {}", p[0], p[1]).unwrap();
    }
    write!(svg,r#"<path d="{d}" fill="none" stroke="{}" stroke-width="{width}" stroke-linejoin="round" stroke-linecap="round"/>"#,stroke(e)).unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;
    const FIXTURE: &str = include_str!("../../tests/fixtures/excalidraw/elements.excalidraw");
    #[test]
    fn each_supported_element_emits_native_vectors_with_stable_seed() {
        let scene = Scene::parse(FIXTURE).unwrap();
        for element in &scene.elements {
            let mut single = scene.clone();
            single.elements = vec![element.clone()];
            let first = single.vectors(|_| None).unwrap();
            let second = single.vectors(|_| None).unwrap();
            assert_eq!(first.svg, second.svg, "{}", element.kind);
            assert!(first.width > 0. && first.height > 0.);
            match element.kind.as_str() {
                "text" => assert!(first.svg.contains("Привет") && first.svg.contains("Excalifont")),
                "image" => {
                    assert!(first.svg.contains("Image unavailable") && first.warnings.len() == 1)
                }
                "frame" => assert!(first.svg.contains("rx=\"8\"")),
                _ => assert!(first.svg.contains("<path"), "{}", element.kind),
            }
        }
        let a = scene.vectors(|_| None).unwrap();
        let mut changed = scene.clone();
        changed.elements[0]
            .properties
            .insert("seed".into(), 999.into());
        assert_ne!(a.svg, changed.vectors(|_| None).unwrap().svg);
    }
    #[test]
    fn text_is_escaped_rotation_and_styles_are_preserved() {
        let mut scene = Scene::parse(FIXTURE).unwrap();
        let element = &mut scene.elements[0];
        element.angle = std::f64::consts::FRAC_PI_2;
        element.properties.insert("opacity".into(), 25.into());
        element
            .properties
            .insert("strokeStyle".into(), "dashed".into());
        let text = scene
            .elements
            .iter_mut()
            .find(|e| e.kind == "text")
            .unwrap();
        text.properties
            .insert("text".into(), "<script>& unsafe\"".into());
        let output = scene.vectors(|_| None).unwrap();
        assert!(output.svg.contains("rotate(90"));
        assert!(output.svg.contains("opacity=\"0.25\""));
        assert!(output.svg.contains("stroke-dasharray"));
        assert!(output.svg.contains("&lt;script&gt;&amp; unsafe&quot;"));
        assert!(!output.svg.contains("<script>"));
        assert!(scene
            .vectors(|_| Some("https://example.com/tracker.png".into()))
            .is_err());
    }
}

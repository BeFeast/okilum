//! The render-scale reading (#3244), free of GPUI so the feedback payload
//! tests in okilum-http can include it.
use serde::Serialize;

/// One reading of a window's render scale. Flattened into feedback metadata,
/// so every field name is a metadata key.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub(crate) struct Snapshot {
    /// GPUI's device scale: OS display scale × UI zoom.
    pub render_scale: f32,
    pub ui_zoom: f32,
    /// GPUI's logical viewport in UI pixels.
    pub viewport: String,
    pub display: Option<String>,
    pub screen_backing_scale: Option<f32>,
    pub window_backing_scale: Option<f32>,
    /// `false` when `[NSWindow screen]` was nil at the time of reading.
    pub window_on_screen: Option<bool>,
    /// Native outer frame in screen points (macOS) or GPUI window bounds.
    pub window_frame: Option<String>,
    pub layer_contents_scale: Option<f32>,
    pub layer_size: Option<String>,
    pub drawable_size: Option<String>,
    /// Whether the drawable is the layer size × contents scale; when it is
    /// not, the compositor stretches the rendered frame.
    pub drawable_matches_layer: Option<bool>,
}

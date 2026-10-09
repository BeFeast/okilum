//! Build-only rasterization of the approved app icon; no application startup.
use anyhow::{ensure, Context, Result};
use gpui::{size, DevicePixels, SvgRenderer, SvgSize};
use std::{path::PathBuf, sync::Arc};

fn main() -> Result<()> {
    let output = PathBuf::from(
        std::env::args_os()
            .nth(1)
            .context("output directory required")?,
    );
    std::fs::create_dir_all(&output)?;
    let renderer = SvgRenderer::new(Arc::new(()));
    let svg = renderer.parse_svg(include_bytes!("../assets/brand/app-icon-light.svg"))?;
    for pixels in [16, 32, 64, 128, 256, 512, 1024] {
        let dimensions = size(DevicePixels(pixels), DevicePixels(pixels));
        // Full-color renderer, not GPUI's icon alpha-mask path.
        let image = renderer.render_parsed(&svg, SvgSize::ExactSize(dimensions))?;
        ensure!(image.size(0) == dimensions, "unexpected raster dimensions");
        let mut rgba = image.as_bytes(0).context("missing raster frame")?.to_vec();
        ensure!(rgba.len() == pixels as usize * pixels as usize * 4);
        // RenderImage exposes straight BGRA; render_parsed already unpremultiplies.
        for pixel in rgba.as_chunks_mut::<4>().0 {
            pixel.swap(0, 2);
        }
        std::fs::write(output.join(format!("{pixels}.rgba")), rgba)?;
    }
    Ok(())
}

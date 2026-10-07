//! The PDF engine seam (#477): the only module that knows about hayro.
//!
//! The viewer sees page sizes and BGRA page bitmaps, nothing else. If hayro
//! fails an acceptance gate, a different engine replaces this module behind
//! the same surface (docs/research/477-inline-pdf.md §6).
use hayro::hayro_interpret::InterpreterSettings;
use hayro::hayro_syntax::{DecryptionError, LoadPdfError, Pdf};
use hayro::vello_cpu::color::palette::css::WHITE;
use hayro::vello_cpu::peniko::ImageAlphaType;
use hayro::{PixmapSettings, RenderCache, RenderSettings};
use std::panic::{catch_unwind, AssertUnwindSafe};

/// Bitmaps never exceed this many device pixels on a side. A larger zoom is
/// shown scaled up rather than rendered bigger (tiles are a later slice).
pub(crate) const MAX_SIDE: u32 = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OpenError {
    /// Encrypted with a password the reader does not have.
    Locked,
    /// Not a PDF, damaged, empty, or something the engine cannot read.
    Unreadable,
}

/// A page's displayed size in PDF points, rotation and crop box applied.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct PageSize {
    pub width: f32,
    pub height: f32,
}

/// An opaque page bitmap in GPUI's byte order (BGRA, 8 bits per channel).
pub(crate) struct Bitmap {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}

pub(crate) struct PdfDocument {
    pdf: Pdf,
    sizes: Vec<PageSize>,
}

impl PdfDocument {
    /// Parses the document. A malformed file that makes the engine panic is
    /// reported as unreadable rather than taking the reader down.
    pub(crate) fn open(bytes: Vec<u8>) -> Result<Self, OpenError> {
        catch_unwind(AssertUnwindSafe(|| {
            let pdf = Pdf::new(bytes).map_err(|error| match error {
                LoadPdfError::Decryption(DecryptionError::PasswordProtected) => OpenError::Locked,
                _ => OpenError::Unreadable,
            })?;
            let sizes: Vec<PageSize> = pdf
                .pages()
                .iter()
                .map(|page| {
                    let (width, height) = page.render_dimensions();
                    PageSize { width, height }
                })
                .collect();
            if sizes.is_empty()
                || sizes
                    .iter()
                    .any(|s| !(s.width.is_finite() && s.height.is_finite()))
                || sizes.iter().any(|s| s.width <= 0. || s.height <= 0.)
            {
                return Err(OpenError::Unreadable);
            }
            Ok(Self { pdf, sizes })
        }))
        .unwrap_or(Err(OpenError::Unreadable))
    }

    pub(crate) fn sizes(&self) -> &[PageSize] {
        &self.sizes
    }

    /// A renderer keeps the engine's per-document caches (fonts, outlines).
    /// It is not `Send`: one per worker thread.
    pub(crate) fn renderer(&self) -> PageRenderer<'_> {
        PageRenderer {
            document: self,
            cache: RenderCache::new(),
            settings: InterpreterSettings::default(),
        }
    }
}

/// Device-pixel size of a page rendered `px_width` wide, clamped so neither
/// side exceeds [`MAX_SIDE`] and the aspect ratio holds.
pub(crate) fn bitmap_size(page: PageSize, px_width: u32) -> (u32, u32) {
    let width = px_width.clamp(1, MAX_SIDE);
    let height = page.height * width as f32 / page.width;
    if height <= MAX_SIDE as f32 {
        return (width, (height.round() as u32).clamp(1, MAX_SIDE));
    }
    let width = page.width * MAX_SIDE as f32 / page.height;
    ((width.round() as u32).clamp(1, MAX_SIDE), MAX_SIDE)
}

pub(crate) struct PageRenderer<'a> {
    document: &'a PdfDocument,
    cache: RenderCache<'a>,
    settings: InterpreterSettings,
}

impl PageRenderer<'_> {
    /// Rasterizes one page on white. `None` when the page cannot be drawn;
    /// the rest of the document stays readable.
    pub(crate) fn render(&self, index: usize, px_width: u32) -> Option<Bitmap> {
        let size = *self.document.sizes.get(index)?;
        let (width, height) = bitmap_size(size, px_width);
        // hayro truncates `size * scale`; aim at the pixel centre so the
        // bitmap lands on the size computed above.
        let x_scale = (width as f32 + 0.5) / size.width;
        let y_scale = (height as f32 + 0.5) / size.height;
        catch_unwind(AssertUnwindSafe(|| {
            let page = self.document.pdf.pages().get(index)?;
            let pixmap = hayro::render(
                page,
                &self.cache,
                &self.settings,
                &RenderSettings::default(),
                &PixmapSettings {
                    x_scale,
                    y_scale,
                    bg_color: WHITE,
                },
            );
            let (width, height) = (u32::from(pixmap.width()), u32::from(pixmap.height()));
            // White background: every pixel is opaque, so premultiplied and
            // straight alpha are the same bytes and no conversion is needed.
            let mut bgra = pixmap.take_rgba8(ImageAlphaType::AlphaPremultiplied);
            for pixel in bgra.chunks_exact_mut(4) {
                pixel.swap(0, 2);
            }
            Some(Bitmap {
                width,
                height,
                bgra,
            })
        }))
        .ok()
        .flatten()
        .filter(|b| b.width > 0 && b.height > 0)
    }
}

#[cfg(test)]
pub(crate) mod fixture {
    //! Writes small, valid PDFs for tests, so no large binary is committed.

    /// One page per entry: (width, height, fill RGB). Each page is filled
    /// with its colour and carries a line of Helvetica text, which hayro
    /// draws with its embedded standard fonts.
    pub(crate) fn pdf(pages: &[(f32, f32, [f32; 3])]) -> Vec<u8> {
        let mut objects: Vec<String> = vec![
            "<< /Type /Catalog /Pages 2 0 R >>".into(),
            String::new(),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into(),
        ];
        let mut kids = Vec::new();
        for (index, (width, height, [r, g, b])) in pages.iter().enumerate() {
            let page_id = objects.len() + 1;
            let content_id = page_id + 1;
            kids.push(format!("{page_id} 0 R"));
            objects.push(format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {width} {height}] \
                 /Resources << /Font << /F1 3 0 R >> >> /Contents {content_id} 0 R >>"
            ));
            let stream = format!(
                "{r} {g} {b} rg 0 0 {width} {height} re f \
                 BT /F1 24 Tf 0 0 0 rg 36 36 Td (Page {}) Tj ET",
                index + 1
            );
            objects.push(format!(
                "<< /Length {} >>\nstream\n{stream}\nendstream",
                stream.len()
            ));
        }
        objects[1] = format!(
            "<< /Type /Pages /Kids [{}] /Count {} >>",
            kids.join(" "),
            pages.len()
        );
        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (index, body) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", index + 1).as_bytes());
        }
        let xref = out.len();
        out.extend_from_slice(
            format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
        );
        for offset in offsets {
            out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                objects.len() + 1
            )
            .as_bytes(),
        );
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    fn pixel(bitmap: &Bitmap, x: u32, y: u32) -> [u8; 4] {
        let at = ((y * bitmap.width + x) * 4) as usize;
        bitmap.bgra[at..at + 4].try_into().unwrap()
    }

    #[test]
    fn renders_pages_at_the_requested_width_in_bgra() {
        let bytes = fixture::pdf(&[(600., 800., [0., 0., 1.]), (800., 400., [1., 0., 0.])]);
        let document = PdfDocument::open(bytes).unwrap();
        assert_eq!(
            document.sizes(),
            &[
                PageSize {
                    width: 600.,
                    height: 800.
                },
                PageSize {
                    width: 800.,
                    height: 400.
                }
            ]
        );
        let renderer = document.renderer();
        let blue = renderer.render(0, 300).unwrap();
        assert_eq!((blue.width, blue.height), (300, 400));
        assert_eq!(blue.bgra.len(), 300 * 400 * 4);
        // BGRA: blue is byte 0. A page left white would fail both pages.
        assert_eq!(pixel(&blue, 150, 100), [255, 0, 0, 255]);
        let red = renderer.render(1, 400).unwrap();
        assert_eq!((red.width, red.height), (400, 200));
        assert_eq!(pixel(&red, 200, 50), [0, 0, 255, 255]);
        // The text line is drawn: black ink near the bottom-left corner.
        let ink = (0..red.height)
            .flat_map(|y| (0..red.width).map(move |x| (x, y)))
            .filter(|&(x, y)| pixel(&red, x, y)[..3].iter().all(|&c| c < 96))
            .count();
        assert!(ink > 50, "standard-font text renders: {ink} dark pixels");
        assert!(renderer.render(2, 400).is_none());
    }

    #[test]
    fn bitmap_size_keeps_aspect_and_bounds() {
        let letter = PageSize {
            width: 612.,
            height: 792.,
        };
        assert_eq!(bitmap_size(letter, 1224), (1224, 1584));
        let (w, h) = bitmap_size(letter, 100_000);
        assert!(w <= MAX_SIDE && h == MAX_SIDE, "{w}x{h}");
        let strip = PageSize {
            width: 100.,
            height: 20_000.,
        };
        let (w, h) = bitmap_size(strip, 800);
        assert_eq!(h, MAX_SIDE);
        assert!((1..800).contains(&w));
        assert_eq!(bitmap_size(letter, 0).0, 1);
    }

    #[test]
    fn password_protected_files_are_locked() {
        let locked = include_bytes!("../tests/fixtures/pdf/locked.pdf").to_vec();
        assert_eq!(PdfDocument::open(locked).err(), Some(OpenError::Locked));
        // Positive control: the same document without encryption opens.
        let plain = include_bytes!("../tests/fixtures/thumbnail/page.pdf").to_vec();
        assert_eq!(PdfDocument::open(plain).unwrap().sizes().len(), 2);
    }

    #[test]
    fn broken_and_empty_files_are_unreadable() {
        assert_eq!(
            PdfDocument::open(b"not a PDF".to_vec()).err(),
            Some(OpenError::Unreadable)
        );
        assert_eq!(
            PdfDocument::open(Vec::new()).err(),
            Some(OpenError::Unreadable)
        );
        assert_eq!(
            PdfDocument::open(fixture::pdf(&[])).err(),
            Some(OpenError::Unreadable)
        );
        // Positive control: the same writer with one page opens.
        assert!(PdfDocument::open(fixture::pdf(&[(100., 100., [1., 1., 1.])])).is_ok());
    }
}

//! Application icons for «Open in ▸» (#1400). Loaded once per run on a
//! background thread; building the menu only reads the finished cache and
//! shows a neutral glyph until then or when an icon cannot be read.
use super::*;
use std::sync::Once;

// Pure file parsing: its tests run on every platform.
#[cfg(any(target_os = "linux", test))]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
mod desktop;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;

type Cache = Vec<(&'static str, Option<Arc<RenderImage>>)>;

static START: Once = Once::new();
static CACHE: OnceLock<Cache> = OnceLock::new();

pub(super) fn warm() {
    START.call_once(|| {
        let spawned = std::thread::Builder::new()
            .name("editor-icons".into())
            .spawn(|| {
                let _ = CACHE.set(collect(editors(), load));
            });
        if let Err(error) = spawned {
            log::warn!("editor icons are not loaded: {error}");
        }
    });
}

/// The editor's icon once loaded; `None` means «show the fallback».
pub(super) fn get(id: &str) -> Option<Arc<RenderImage>> {
    // A menu opened before startup warmed the cache still starts the load.
    warm();
    lookup(CACHE.get()?, id)
}

fn collect(editors: &[Editor], load: impl Fn(&Editor) -> Option<Arc<RenderImage>>) -> Cache {
    editors.iter().map(|e| (e.id, load(e))).collect()
}

fn lookup(cache: &Cache, id: &str) -> Option<Arc<RenderImage>> {
    cache.iter().find(|(key, _)| *key == id)?.1.clone()
}

/// Decode PNG/TIFF/… bytes into a menu-sized image.
#[cfg_attr(windows, allow(dead_code))]
fn raster(bytes: &[u8]) -> Option<Arc<RenderImage>> {
    let mut rgba = image::load_from_memory(bytes)
        .ok()?
        .thumbnail(64, 64)
        .into_rgba8();
    // RenderImage stores straight BGRA.
    for pixel in rgba.pixels_mut() {
        pixel.0.swap(0, 2);
    }
    Some(Arc::new(RenderImage::new(vec![image::Frame::new(rgba)])))
}

#[cfg(target_os = "linux")]
fn load(editor: &Editor) -> Option<Arc<RenderImage>> {
    let known = KNOWN.iter().find(|k| k.id == editor.id)?;
    let path = desktop::resolve(known.desktop, &desktop::Search::current())?;
    let bytes = std::fs::read(&path).ok()?;
    if path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("svg"))
    {
        let renderer = SvgRenderer::new(Arc::new(()));
        let svg = renderer.parse_svg(&bytes).ok()?;
        renderer
            .render_parsed(
                &svg,
                SvgSize::ExactSize(size(DevicePixels(64), DevicePixels(64))),
            )
            .ok()
    } else {
        raster(&bytes)
    }
}

#[cfg(target_os = "macos")]
fn load(editor: &Editor) -> Option<Arc<RenderImage>> {
    macos::load(&editor.icon_path)
}

#[cfg(windows)]
fn load(editor: &Editor) -> Option<Arc<RenderImage>> {
    windows::load(&editor.icon_path)
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn load(_: &Editor) -> Option<Arc<RenderImage>> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn editor(id: &'static str) -> Editor {
        Editor {
            id,
            label: id,
            program: "/bin/x".into(),
            icon_path: "/bin/x".into(),
            prefix: vec![],
            line: LineArg::None,
        }
    }

    fn png() -> Vec<u8> {
        let mut bytes = Vec::new();
        image::RgbaImage::from_pixel(128, 128, image::Rgba([10, 20, 200, 255]))
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .unwrap();
        bytes
    }

    #[test]
    fn each_editor_gets_its_own_icon_or_the_fallback() {
        let image = raster(&png()).expect("positive control: a PNG decodes");
        let cache = collect(&[editor("zed"), editor("cursor")], |e| {
            (e.id == "zed").then(|| image.clone())
        });
        assert!(lookup(&cache, "zed").is_some_and(|i| Arc::ptr_eq(&i, &image)));
        assert!(lookup(&cache, "cursor").is_none(), "unreadable: fallback");
        assert!(lookup(&cache, "vscode").is_none(), "not detected: fallback");
    }

    #[test]
    fn raster_is_menu_sized_bgra_and_rejects_garbage() {
        let image = raster(&png()).unwrap();
        assert_eq!(image.size(0), size(DevicePixels(64), DevicePixels(64)));
        // Blue in RGBA lands in the first byte of BGRA.
        assert_eq!(&image.as_bytes(0).unwrap()[..4], &[200, 20, 10, 255]);
        assert!(raster(b"not an image").is_none());
    }
}

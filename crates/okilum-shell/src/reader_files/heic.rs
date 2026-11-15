//! Read-only HEIC/HEIF decoding shared by file previews and Markdown embeds.
//! The Linux AppImage carries its decoder; native packages use libheif's CLI.
use super::*;
use std::process::{Command, Stdio};
use std::time::Instant;

pub(super) struct HeicImage;

impl Asset for HeicImage {
    type Source = PathBuf;
    type Output = Result<Arc<RenderImage>, ImageCacheError>;

    fn load(
        path: PathBuf,
        cx: &mut App,
    ) -> impl std::future::Future<Output = Self::Output> + Send + 'static {
        let renderer = cx.svg_renderer();
        cx.background_executor().spawn(async move {
            let result = decode(&path)
                .and_then(|png| Image::from_bytes(ImageFormat::Png, png).to_image_data(renderer));
            result.map_err(Into::into)
        })
    }
}

#[cfg(target_os = "linux")]
fn linux_tool(executable: &Path, tool: &str) -> PathBuf {
    // Prefer our sibling even when the host has a different libheif version.
    // linuxdeploy gives this executable its own relative library search path.
    let bundled = executable.with_file_name(tool);
    if bundled.is_file() {
        bundled
    } else {
        PathBuf::from(tool)
    }
}

fn decode(path: &Path) -> anyhow::Result<Vec<u8>> {
    // Absolute input and a private working directory keep filenames (including
    // spaces, Unicode, leading '-' and libheif's multi-image suffixes) literal.
    let path = path.canonicalize()?;
    let file = std::fs::File::open(path)?;
    anyhow::ensure!(
        file.metadata()?.len() <= 50 * 1024 * 1024,
        "HEIC image exceeds the 50 MiB preview limit"
    );
    let directory = tempfile::tempdir()?;
    // Decode a bounded snapshot, so a concurrent save cannot change the bytes
    // mid-decode and the external tool never receives a writable source path.
    use std::io::Read as _;
    let mut source = Vec::new();
    file.take(50 * 1024 * 1024 + 1).read_to_end(&mut source)?;
    anyhow::ensure!(
        source.len() <= 50 * 1024 * 1024,
        "HEIC image exceeds the 50 MiB preview limit"
    );
    let snapshot = directory.path().join("source.heic");
    std::fs::write(&snapshot, source)?;
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = Command::new("/usr/bin/sips");
        command
            .args(["-s", "format", "png"])
            .arg(&snapshot)
            .args(["--out", "preview.png"]);
        command
    };
    #[cfg(target_os = "linux")]
    let mut command = {
        let executable = std::env::current_exe()?;
        let mut command = Command::new(linux_tool(&executable, "prlimit"));
        // Limits apply before libheif parses any metadata or HEVC bitstream.
        // The CLI remains isolated even if native decoding aborts on bad input.
        command
            .args([
                "--as=1073741824",
                "--fsize=268435456",
                "--cpu=20",
                "--core=0",
                "--",
            ])
            .arg(linux_tool(&executable, "heif-convert"))
            .arg(&snapshot)
            .arg("preview.png");
        command
    };
    command.current_dir(directory.path());
    run_decoder(&mut command, Duration::from_secs(30))?;
    let png = directory.path().join("preview.png");
    #[cfg(target_os = "linux")]
    let png = if png.is_file() {
        png
    } else {
        // libheif numbers output when the container has multiple top-level
        // images. Show the first still, never a depth/auxiliary image.
        directory.path().join("preview-1.png")
    };
    let file = std::fs::File::open(png)?;
    anyhow::ensure!(
        file.metadata()?.len() <= 256 * 1024 * 1024,
        "HEIC preview is too large"
    );
    let mut png = Vec::new();
    file.take(256 * 1024 * 1024 + 1).read_to_end(&mut png)?;
    anyhow::ensure!(png.len() <= 256 * 1024 * 1024, "HEIC preview is too large");
    let (width, height) =
        image::ImageReader::with_format(std::io::Cursor::new(&png), image::ImageFormat::Png)
            .into_dimensions()?;
    anyhow::ensure!(
        width > 0
            && height > 0
            && width <= 16384
            && height <= 16384
            && u64::from(width) * u64::from(height) <= 64_000_000,
        "HEIC dimensions exceed the preview limit"
    );
    Ok(png)
}

fn run_decoder(command: &mut Command, budget: Duration) -> anyhow::Result<()> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let deadline = Instant::now() + budget;
    let result = (|| loop {
        if let Some(status) = child.try_wait()? {
            anyhow::ensure!(status.success(), "Cannot decode HEIC image");
            return Ok(());
        }
        let left = deadline.saturating_duration_since(Instant::now());
        anyhow::ensure!(!left.is_zero(), "HEIC image decoding timed out");
        std::thread::sleep(left.min(Duration::from_millis(25)));
    })();
    if result.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    const FIXTURE: &[u8] =
        include_bytes!("../../../okilum-core/tests/fixtures/viewers/images/still.heic");

    #[test]
    fn issue2984_real_decoder_reads_hevc_without_changing_the_source() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("- café 'photo'.HEIF");
        std::fs::write(&path, FIXTURE).unwrap();
        let png = decode(&path).unwrap();
        let image = image::load_from_memory_with_format(&png, image::ImageFormat::Png)
            .unwrap()
            .to_rgb8();
        assert_eq!(image.dimensions(), (64, 48));
        let red = image.get_pixel(8, 8).0;
        let green = image.get_pixel(56, 40).0;
        assert!(red[0] > 180 && red[1] < 80 && red[2] < 80, "{red:?}");
        assert!(
            green[1] > 180 && green[0] < 80 && green[2] < 80,
            "{green:?}"
        );
        assert_eq!(std::fs::read(&path).unwrap(), FIXTURE);
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
        std::fs::write(&path, b"broken HEIC").unwrap();
        assert!(
            decode(&path).is_err(),
            "Bad input must not reuse a stale PNG"
        );
        std::fs::write(&path, FIXTURE).unwrap();
        assert!(decode(&path).is_ok(), "A repaired file must decode again");
    }

    #[test]
    fn issue2984_oversized_source_is_rejected_before_decoding() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("oversized.heic");
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(50 * 1024 * 1024 + 1).unwrap();
        let error = decode(&path).unwrap_err();
        assert!(error.to_string().contains("50 MiB"));
        assert_eq!(file.metadata().unwrap().len(), 50 * 1024 * 1024 + 1);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn issue2984_multi_image_container_uses_first_still() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("multiple.heic");
        std::fs::write(
            &path,
            include_bytes!("../../../okilum-core/tests/fixtures/viewers/images/multiple.heic"),
        )
        .unwrap();
        let actual = image::load_from_memory(&decode(&path).unwrap())
            .unwrap()
            .to_rgba8();
        std::fs::write(&path, FIXTURE).unwrap();
        let expected = image::load_from_memory(&decode(&path).unwrap())
            .unwrap()
            .to_rgba8();
        assert_eq!(actual, expected);
    }

    #[test]
    fn issue2984_decoder_failure_and_timeout_are_errors() {
        assert!(run_decoder(
            Command::new("/bin/sh").args(["-c", "exit 7"]),
            Duration::from_secs(1)
        )
        .is_err());
        let start = Instant::now();
        let error = run_decoder(
            Command::new("/bin/sh").args(["-c", "exec sleep 10"]),
            Duration::from_millis(50),
        )
        .unwrap_err();
        assert!(error.to_string().contains("timed out"));
        assert!(start.elapsed() < Duration::from_secs(2));
        run_decoder(
            Command::new("/bin/sh").args(["-c", "exit 0"]),
            Duration::from_secs(1),
        )
        .unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn issue2984_bundled_decoder_wins_over_the_host() {
        let directory = tempfile::tempdir().unwrap();
        let executable = directory.path().join("okilum");
        assert_eq!(
            linux_tool(&executable, "heif-convert"),
            PathBuf::from("heif-convert")
        );
        let bundled = directory.path().join("heif-convert");
        std::fs::write(&bundled, b"bundled decoder").unwrap();
        assert_eq!(linux_tool(&executable, "heif-convert"), bundled);
    }
}

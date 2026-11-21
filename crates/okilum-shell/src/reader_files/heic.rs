//! Read-only HEIC/HEIF decoding shared by file previews and Markdown embeds.
//!
//! Every platform decodes in a separate, bounded child process with the
//! system's own codec: macOS `sips` (ImageIO), Linux libheif's `heif-convert`
//! (bundled in the AppImage, from PATH otherwise) and Windows WIC through
//! PowerShell/WPF, which needs Microsoft's HEIF and HEVC extensions (#2177).
//! A missing decoder is reported in words the reader can act on.
use super::*;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Instant;

#[cfg(any(target_os = "linux", test))]
mod primary;
#[cfg(windows)]
mod windows;

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

/// Why the platform could not decode at all, independent of the file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
enum Missing {
    LinuxTool = 1,
    LinuxCodec = 2,
    WindowsCodec = 3,
}

/// The last decoder verdict. A missing codec is a property of the system, so
/// the most recent decode that reached a decoder decides the fallback text.
static MISSING: AtomicU8 = AtomicU8::new(0);

// macOS always has `sips`; only Linux and Windows report a missing decoder.
#[cfg_attr(target_os = "macos", allow(dead_code))]
#[derive(Debug)]
struct MissingDecoder(Missing);

impl std::fmt::Display for MissingDecoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(message(Some(self.0)))
    }
}

impl std::error::Error for MissingDecoder {}

fn message(missing: Option<Missing>) -> &'static str {
    match missing {
        None => "This image couldn’t be read.",
        Some(Missing::LinuxTool) => {
            "HEIC/HEIF images need libheif’s heif-convert (package libheif-examples or libheif)."
        }
        Some(Missing::LinuxCodec) => {
            "This HEIC/HEIF image needs a libheif decoder plugin for its codec (HEVC: libheif-plugin-libde265)."
        }
        Some(Missing::WindowsCodec) => {
            "HEIC/HEIF images need Microsoft’s HEIF Image Extensions and HEVC Video Extensions from the Microsoft Store."
        }
    }
}

/// Fallback text for a HEIC preview that failed: names the missing system
/// decoder when that is the cause, otherwise the ordinary unreadable message.
pub(super) fn unreadable_message() -> &'static str {
    message(match MISSING.load(Ordering::Relaxed) {
        1 => Some(Missing::LinuxTool),
        2 => Some(Missing::LinuxCodec),
        3 => Some(Missing::WindowsCodec),
        _ => None,
    })
}

#[cfg(target_os = "linux")]
fn linux_tool(
    executable: &Path,
    tool: &str,
    search: Option<std::ffi::OsString>,
) -> Option<PathBuf> {
    // Prefer our sibling even when the host has a different libheif version.
    // linuxdeploy gives this executable its own relative library search path.
    let bundled = executable.with_file_name(tool);
    if bundled.is_file() {
        return Some(bundled);
    }
    std::env::split_paths(&search?)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(tool))
        .find(|candidate| candidate.is_file())
}

/// libheif names an absent codec plugin; anything else is a file problem.
#[cfg(any(target_os = "linux", test))]
fn linux_codec_missing(stderr: &str) -> bool {
    stderr.contains("Unsupported codec") || stderr.contains("No decoding plugin")
}

/// ISO-BMFF files start with an `ftyp` box. Anything else is not HEIF, so a
/// codec error on such input never blames the system decoder.
fn looks_like_heif(source: &[u8]) -> bool {
    source.len() >= 16
        && &source[4..8] == b"ftyp"
        && u32::from_be_bytes([source[0], source[1], source[2], source[3]]) >= 16
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
    anyhow::ensure!(looks_like_heif(&source), "Not a HEIC/HEIF file");
    // libheif's converter writes every top-level image; keep only the
    // primary (#2177). sips and WIC already decode the primary image.
    #[cfg(target_os = "linux")]
    primary::primary_only(&mut source);
    let snapshot = directory.path().join("source.heic");
    std::fs::write(&snapshot, source)?;
    let result = run_platform_decoder(directory.path(), &snapshot);
    match &result {
        Ok(()) => MISSING.store(0, Ordering::Relaxed),
        Err(error) => match error.downcast_ref::<MissingDecoder>() {
            Some(MissingDecoder(missing)) => MISSING.store(*missing as u8, Ordering::Relaxed),
            None if error.downcast_ref::<DecoderFailed>().is_some() => {
                MISSING.store(0, Ordering::Relaxed)
            }
            None => {}
        },
    }
    result?;
    let png = directory.path().join("preview.png");
    #[cfg(target_os = "linux")]
    let png = if png.is_file() {
        png
    } else {
        // libheif numbers output when the container still has several
        // top-level images: `primary_only` could not read the item list.
        // Show the first still, never a depth/auxiliary image.
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

#[cfg(target_os = "macos")]
fn run_platform_decoder(directory: &Path, snapshot: &Path) -> anyhow::Result<()> {
    let mut command = Command::new("/usr/bin/sips");
    command
        .args(["-s", "format", "png"])
        .arg(snapshot)
        .args(["--out", "preview.png"])
        .current_dir(directory)
        .stderr(Stdio::null());
    run_decoder(&mut command, Duration::from_secs(30))
}

#[cfg(target_os = "linux")]
fn run_platform_decoder(directory: &Path, snapshot: &Path) -> anyhow::Result<()> {
    let executable = std::env::current_exe()?;
    let search = std::env::var_os("PATH");
    let Some(converter) = linux_tool(&executable, "heif-convert", search.clone()) else {
        return Err(MissingDecoder(Missing::LinuxTool).into());
    };
    let prlimit =
        linux_tool(&executable, "prlimit", search).unwrap_or_else(|| PathBuf::from("prlimit"));
    let mut command = Command::new(prlimit);
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
        .arg(converter)
        .arg(snapshot)
        .arg("preview.png")
        .current_dir(directory);
    // A file, not a pipe: a chatty decoder can never block on a full pipe.
    let log = directory.join("decoder.log");
    command.stderr(std::fs::File::create(&log)?);
    let result = run_decoder(&mut command, Duration::from_secs(30));
    if result.is_err() {
        use std::io::Read as _;
        let mut text = String::new();
        if let Ok(file) = std::fs::File::open(&log) {
            let _ = file.take(16 * 1024).read_to_string(&mut text);
        }
        if linux_codec_missing(&text) {
            return Err(MissingDecoder(Missing::LinuxCodec).into());
        }
    }
    result
}

#[cfg(windows)]
fn run_platform_decoder(directory: &Path, _snapshot: &Path) -> anyhow::Result<()> {
    windows::decode(directory)
}

/// The decoder ran and exited unsuccessfully (or was killed).
#[derive(Debug)]
struct DecoderFailed {
    // Windows maps its codec-missing exit code; elsewhere only tests read it.
    #[cfg_attr(not(windows), allow(dead_code))]
    code: Option<i32>,
    timed_out: bool,
}

impl std::fmt::Display for DecoderFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.timed_out {
            f.write_str("HEIC image decoding timed out")
        } else {
            f.write_str("Cannot decode HEIC image")
        }
    }
}

impl std::error::Error for DecoderFailed {}

fn run_decoder(command: &mut Command, budget: Duration) -> anyhow::Result<()> {
    command.stdin(Stdio::null()).stdout(Stdio::null());
    #[cfg(windows)]
    let (mut child, _job) = windows::spawn_limited(command)?;
    #[cfg(not(windows))]
    let mut child = command.spawn()?;
    let deadline = Instant::now() + budget;
    let result = (|| loop {
        if let Some(status) = child.try_wait()? {
            if status.success() {
                return Ok(());
            }
            return Err(DecoderFailed {
                code: status.code(),
                timed_out: false,
            }
            .into());
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(DecoderFailed {
                code: None,
                timed_out: true,
            }
            .into());
        }
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

    fn assert_still_pixels(png: &[u8]) {
        let image = image::load_from_memory_with_format(png, image::ImageFormat::Png)
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
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn issue2984_real_decoder_reads_hevc_without_changing_the_source() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("- café 'photo'.HEIF");
        std::fs::write(&path, FIXTURE).unwrap();
        assert_still_pixels(&decode(&path).unwrap());
        assert_eq!(std::fs::read(&path).unwrap(), FIXTURE);
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
        std::fs::write(&path, b"broken HEIC").unwrap();
        assert!(
            decode(&path).is_err(),
            "Bad input must not reuse a stale PNG"
        );
        std::fs::write(&path, FIXTURE).unwrap();
        assert!(decode(&path).is_ok(), "A repaired file must decode again");
        assert_eq!(unreadable_message(), message(None));
    }

    /// The hosted Windows runner has no HEIF/HEVC Store extensions, so CI sets
    /// `OKILUM_HEIC_EXPECT=missing` and pins the fallback; with the extensions
    /// installed (`decoded`) the same control checks real pixels.
    #[cfg(windows)]
    #[test]
    fn issue2177_windows_wic_decodes_or_names_the_missing_extensions() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("- café 'photo'.HEIF");
        std::fs::write(&path, FIXTURE).unwrap();
        let expect = std::env::var("OKILUM_HEIC_EXPECT").unwrap_or_default();
        match decode(&path) {
            Ok(png) => {
                assert_ne!(expect, "missing", "expected the missing-codec fallback");
                assert_still_pixels(&png);
                assert_eq!(unreadable_message(), message(None));
            }
            Err(error) => {
                assert_ne!(expect, "decoded", "{error:#}");
                assert!(
                    matches!(
                        error.downcast_ref::<MissingDecoder>(),
                        Some(MissingDecoder(Missing::WindowsCodec))
                    ),
                    "a valid HEIC must decode or name the missing codec: {error:#}"
                );
                assert!(unreadable_message().contains("HEIF Image Extensions"));
            }
        }
        assert_eq!(std::fs::read(&path).unwrap(), FIXTURE);
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    #[test]
    fn issue2177_non_heif_bytes_never_reach_a_decoder() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("fake.heic");
        std::fs::write(&path, b"broken HEIC, longer than sixteen bytes").unwrap();
        let error = decode(&path).unwrap_err();
        assert!(
            error.to_string().contains("Not a HEIC/HEIF file"),
            "{error}"
        );
        assert!(looks_like_heif(FIXTURE));
        assert!(!looks_like_heif(&FIXTURE[..15]));
        let mut zero_length = FIXTURE.to_vec();
        zero_length[..4].copy_from_slice(&[0, 0, 0, 0]);
        assert!(!looks_like_heif(&zero_length));
    }

    #[test]
    fn issue2177_missing_decoder_messages_name_the_component() {
        assert!(message(Some(Missing::LinuxTool)).contains("heif-convert"));
        assert!(message(Some(Missing::LinuxCodec)).contains("libheif-plugin-libde265"));
        assert!(message(Some(Missing::WindowsCodec)).contains("HEVC Video Extensions"));
        assert!(linux_codec_missing(
            "Could not decode image: Unsupported feature: Unsupported codec\n"
        ));
        assert!(!linux_codec_missing(
            "Could not read HEIF/AVIF file: Invalid input: No 'ftyp' box\n"
        ));
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

    const MULTIPLE: &[u8] =
        include_bytes!("../../../okilum-core/tests/fixtures/viewers/images/multiple.heic");
    const PRIMARY_SECOND: &[u8] =
        include_bytes!("../../../okilum-core/tests/fixtures/viewers/images/primary-second.heic");

    fn changed_bytes(before: &[u8], after: &[u8]) -> Vec<usize> {
        assert_eq!(before.len(), after.len());
        (0..before.len())
            .filter(|&i| before[i] != after[i])
            .collect()
    }

    #[test]
    fn issue2177_primary_only_hides_every_other_item() {
        // One flag bit per non-primary item, nothing else.
        for source in [MULTIPLE, PRIMARY_SECOND] {
            let mut hidden = source.to_vec();
            assert!(primary::primary_only(&mut hidden));
            let changed = changed_bytes(source, &hidden);
            assert_eq!(changed.len(), 1, "exactly the other item's flags byte");
            assert_eq!(hidden[changed[0]], source[changed[0]] | 1);
        }
        // The two fixtures differ only in `pitm`, so they hide different items.
        let (mut first, mut second) = (MULTIPLE.to_vec(), PRIMARY_SECOND.to_vec());
        primary::primary_only(&mut first);
        primary::primary_only(&mut second);
        assert_ne!(
            changed_bytes(MULTIPLE, &first),
            changed_bytes(PRIMARY_SECOND, &second)
        );
        let mut single = FIXTURE.to_vec();
        assert!(primary::primary_only(&mut single));
        assert_eq!(single, FIXTURE, "a single image stays byte-identical");
        // Malformed structure: left for libheif to judge, never rewritten.
        let mut truncated = MULTIPLE[..MULTIPLE.len() / 2].to_vec();
        let copy = truncated.clone();
        assert!(!primary::primary_only(&mut truncated));
        assert_eq!(truncated, copy);
        let mut overrun = MULTIPLE.to_vec();
        overrun[..4].copy_from_slice(&u32::MAX.to_be_bytes());
        let copy = overrun.clone();
        assert!(!primary::primary_only(&mut overrun));
        assert_eq!(overrun, copy);
    }

    /// `primary-second.heic` is `multiple.heic` with `pitm` naming the second
    /// still (the top/bottom reversal). Without `primary_only`, libheif's
    /// first output is the first still and this fails.
    #[cfg(target_os = "linux")]
    #[test]
    fn issue2177_multi_image_container_shows_the_primary() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("primary-second.heic");
        std::fs::write(&path, PRIMARY_SECOND).unwrap();
        let image =
            image::load_from_memory_with_format(&decode(&path).unwrap(), image::ImageFormat::Png)
                .unwrap()
                .to_rgb8();
        assert_eq!(image.dimensions(), (64, 48));
        let red = image.get_pixel(8, 40).0;
        let green = image.get_pixel(56, 8).0;
        assert!(red[0] > 180 && red[1] < 80 && red[2] < 80, "{red:?}");
        assert!(
            green[1] > 180 && green[0] < 80 && green[2] < 80,
            "{green:?}"
        );
        assert_eq!(std::fs::read(&path).unwrap(), PRIMARY_SECOND);
    }

    #[cfg(unix)]
    #[test]
    fn issue2984_decoder_failure_and_timeout_are_errors() {
        let error = run_decoder(
            Command::new("/bin/sh").args(["-c", "exit 7"]),
            Duration::from_secs(1),
        )
        .unwrap_err();
        assert!(matches!(
            error.downcast_ref::<DecoderFailed>(),
            Some(DecoderFailed { code: Some(7), .. })
        ));
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

    /// Exercises the suspended spawn, Job Object assignment and resume path.
    #[cfg(windows)]
    #[test]
    fn issue2177_windows_job_child_failure_and_timeout_are_errors() {
        let cmd = std::env::var_os("ComSpec").unwrap_or_else(|| "cmd.exe".into());
        let error = run_decoder(
            Command::new(&cmd).args(["/d", "/c", "exit 7"]),
            Duration::from_secs(10),
        )
        .unwrap_err();
        assert!(matches!(
            error.downcast_ref::<DecoderFailed>(),
            Some(DecoderFailed { code: Some(7), .. })
        ));
        let start = Instant::now();
        let error = run_decoder(
            Command::new(&cmd).args(["/d", "/c", "ping -n 30 127.0.0.1 >NUL"]),
            Duration::from_millis(500),
        )
        .unwrap_err();
        assert!(error.to_string().contains("timed out"));
        assert!(start.elapsed() < Duration::from_secs(5));
        run_decoder(
            Command::new(&cmd).args(["/d", "/c", "exit 0"]),
            Duration::from_secs(10),
        )
        .unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn issue2984_bundled_decoder_wins_over_the_host() {
        let directory = tempfile::tempdir().unwrap();
        let executable = directory.path().join("okilum");
        let host = tempfile::tempdir().unwrap();
        let search = std::env::join_paths([host.path()]).unwrap();
        assert_eq!(
            linux_tool(&executable, "heif-convert", Some(search.clone())),
            None,
            "no bundled or host decoder"
        );
        let installed = host.path().join("heif-convert");
        std::fs::write(&installed, b"host decoder").unwrap();
        assert_eq!(
            linux_tool(&executable, "heif-convert", Some(search.clone())),
            Some(installed)
        );
        let bundled = directory.path().join("heif-convert");
        std::fs::write(&bundled, b"bundled decoder").unwrap();
        assert_eq!(
            linux_tool(&executable, "heif-convert", Some(search)),
            Some(bundled)
        );
    }
}

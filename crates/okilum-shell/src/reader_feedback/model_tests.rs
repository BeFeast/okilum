use super::*;
use std::{io::Write, net::TcpListener};

pub(super) fn meta() -> Metadata {
    Metadata {
        version: "0.1.123".into(),
        build: "123".into(),
        channel: "beta".into(),
        source: "a".repeat(40),
        os: "linux".into(),
        os_version: "Linux".into(),
        distro: Some("Ubuntu 26.04.1".into()),
        xdg_session_type: Some("wayland".into()),
        desktop: Some("GNOME".into()),
        arch: "x86_64".into(),
        scale: 1.5,
        window_width: 1200.,
        window_height: 800.,
        decorations: "CSD".into(),
    }
}
fn report() -> Report {
    Report {
        text: "A report with Ελληνικά and an idea 💡".into(),
        kind: Kind::Bug,
        meta: meta(),
        screenshot: None,
        log: None,
    }
}
fn image(format: image::ImageFormat) -> Vec<u8> {
    let mut out = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
        12,
        8,
        image::Rgb([220, 40, 15]),
    ))
    .write_to(&mut out, format)
    .unwrap();
    out.into_inner()
}
fn mock(status: u16, response: &str) -> (String, std::thread::JoinHandle<Vec<u8>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/feedback", listener.local_addr().unwrap());
    let response = response.to_owned();
    let handle = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut request = Vec::new();
        let mut buffer = [0u8; 8192];
        loop {
            let count = socket.read(&mut buffer).unwrap();
            assert!(count > 0);
            request.extend_from_slice(&buffer[..count]);
            if let Some(end) = request.windows(4).position(|b| b == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&request[..end]).to_ascii_lowercase();
                let length = headers
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length: "))
                    .unwrap()
                    .parse::<usize>()
                    .unwrap();
                if request.len() >= end + 4 + length {
                    break;
                }
            }
        }
        write!(socket, "HTTP/1.1 {status} Result\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()).unwrap();
        request
    });
    (url, handle)
}
const RECEIPT: &str =
    r#"{"issue_number":123,"issue_url":"https://git.oklabs.uk/BeFeast/okilum/issues/123"}"#;

#[test]
fn multipart_preserves_unicode_and_only_attaches_chosen_data() {
    for attachments in [false, true] {
        let mut report = report();
        report.kind = Kind::Feature;
        if attachments {
            report.screenshot = Some(Arc::new(image(image::ImageFormat::Png)));
            report.log = Some("line α\nline β".into());
        }
        let (url, server) = mock(201, RECEIPT);
        assert_eq!(send_to(&url, &report).unwrap().issue_number, 123);
        let wire = server.join().unwrap();
        let text = String::from_utf8_lossy(&wire);
        assert!(text.contains(&report.text));
        assert!(text.contains("name=\"kind\"\r\n\r\nfr"));
        assert!(text.contains("\"xdg_session_type\":\"wayland\""));
        assert!(text.contains("\"decorations\":\"CSD\""));
        assert_eq!(text.contains("name=\"screenshot\""), attachments);
        assert_eq!(text.contains("name=\"log\""), attachments);
        assert!(!text.to_ascii_lowercase().contains("authorization:"));
        if attachments {
            assert!(wire.windows(8).any(|b| b == b"\x89PNG\r\n\x1a\n"));
        }
    }
}
#[test]
fn server_failures_and_untrusted_links_are_errors_without_mutating_the_report() {
    let report = report();
    let before = report.copy_text();
    for status in [400, 413, 429, 503] {
        let (url, server) = mock(status, r#"{"error":"Try later"}"#);
        assert!(send_to(&url, &report)
            .unwrap_err()
            .to_string()
            .contains(&status.to_string()));
        server.join().unwrap();
        assert_eq!(report.copy_text(), before);
    }
    let (url, server) = mock(
        201,
        r#"{"issue_number":123,"issue_url":"https://example.com/123"}"#,
    );
    assert!(send_to(&url, &report).is_err());
    server.join().unwrap();
}
#[test]
fn payload_character_limits_and_metadata_are_explicit() {
    let mut report = report();
    report.text = "я".repeat(10_000);
    assert!(report.validate().is_ok());
    report.text.push('я');
    assert!(report.validate().is_err());
    report.text = "  ".into();
    assert!(report.validate().is_err());
    let meta = serde_json::to_value(meta()).unwrap();
    for key in [
        "version",
        "build",
        "channel",
        "source",
        "os",
        "os_version",
        "distro",
        "xdg_session_type",
        "desktop",
        "arch",
        "scale",
        "window_width",
        "window_height",
        "decorations",
    ] {
        assert!(!meta[key].is_null(), "{key}");
    }
    assert!(report
        .meta
        .summary()
        .contains("Ubuntu 26.04.1 · wayland · GNOME"));
}
#[test]
fn png_jpeg_and_clipboard_bmp_have_real_png_payloads_and_removable_previews() {
    for format in [
        image::ImageFormat::Png,
        image::ImageFormat::Jpeg,
        image::ImageFormat::Bmp,
    ] {
        let bytes = image(format);
        if format == image::ImageFormat::Bmp {
            assert!(image_bytes(&bytes, false).is_err());
        }
        let (png, preview) = image_bytes(&bytes, true).unwrap();
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        assert!(png.len() <= MAX_IMAGE);
        let decoded = image::load_from_memory(&png).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (12, 8));
        assert_eq!(preview.dimensions(), (12, 8));
    }
    assert!(image_bytes(b"not an image", true).is_err());
    let mut oversized = image(image::ImageFormat::Png);
    oversized.resize(MAX_IMAGE + 1, 0);
    assert!(
        image_bytes(&oversized, false).is_err(),
        "a valid PNG with oversized bytes must be rejected"
    );
}
#[test]
fn log_attachment_is_bounded_to_250_lines_and_256_kib_without_broken_utf8() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("log");
    std::fs::write(
        &path,
        (0..300).map(|n| format!("{n}: αβγ\n")).collect::<String>(),
    )
    .unwrap();
    let tail = log_tail(&path).unwrap();
    assert_eq!(tail.lines().count(), 250);
    assert!(tail.starts_with("50:"));
    assert!(tail.ends_with("299: αβγ"));
    std::fs::write(&path, format!("{}\nmarker\n", "α".repeat(MAX_LOG))).unwrap();
    let tail = log_tail(&path).unwrap();
    assert_eq!(tail, "marker");
    assert!(tail.len() <= MAX_LOG);
}

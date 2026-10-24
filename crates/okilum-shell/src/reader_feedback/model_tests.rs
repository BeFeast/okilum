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
        video: None,
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
fn read_mock_request(socket: &mut std::net::TcpStream) -> Vec<u8> {
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
    request
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
        let request = read_mock_request(&mut socket);
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
fn sized_file(dir: &Path, name: &str, len: u64) -> std::path::PathBuf {
    let path = dir.join(name);
    std::fs::File::create(&path).unwrap().set_len(len).unwrap();
    path
}
#[test]
fn video_cap_is_pomois_90_mb_in_decimal_units() {
    // pomoi FEEDBACK_VIDEO_MAX_BYTES; Cloudflare Free rejects 100 MB requests.
    assert_eq!(MAX_VIDEO, 90_000_000);
    assert_eq!(human_size(MAX_VIDEO), "90 MB");
    assert_eq!(human_size(95_300_000), "95.3 MB");
    assert_eq!(human_size(1_500), "2 KB");
    assert!(super::super::video_hint().contains("up to 90 MB"));
}
#[test]
fn video_files_are_checked_by_type_emptiness_and_the_exact_byte_cap() {
    let dir = tempfile::tempdir().unwrap();
    for (name, mime) in [
        ("a.mp4", "video/mp4"),
        ("b.M4V", "video/mp4"),
        ("c.MOV", "video/quicktime"),
        ("d.webm", "video/webm"),
    ] {
        assert_eq!(video_mime(Path::new(name)), Some(mime), "{name}");
    }
    assert_eq!(video_mime(Path::new("e.png")), None);
    assert_eq!(video_mime(Path::new("noext")), None);

    let at_cap = video_file(&sized_file(dir.path(), "cap.mp4", MAX_VIDEO)).unwrap();
    assert_eq!(
        (at_cap.name.as_str(), at_cap.size, at_cap.mime),
        ("cap.mp4", MAX_VIDEO, "video/mp4")
    );
    let over = video_file(&sized_file(dir.path(), "over.mov", MAX_VIDEO + 1))
        .unwrap_err()
        .to_string();
    assert!(
        over.contains("90000001 bytes; the limit is 90000000 bytes"),
        "{over}"
    );
    let far_over = video_file(&sized_file(dir.path(), "far.webm", 95_300_000))
        .unwrap_err()
        .to_string();
    assert!(
        far_over.contains("95.3 MB; the limit is 90 MB"),
        "{far_over}"
    );
    assert!(video_file(&sized_file(dir.path(), "empty.mp4", 0)).is_err());
    assert!(video_file(&sized_file(dir.path(), "clip.avi", 10)).is_err());
    assert!(video_file(&dir.path().join("missing.mp4")).is_err());
}
#[test]
fn video_is_streamed_as_its_own_multipart_part_with_type_and_name() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("clip.webm");
    let body: &[u8] = b"\x1aE\xdf\xa3 webm body";
    std::fs::write(&path, body).unwrap();
    let mut report = report();
    report.video = Some(video_file(&path).unwrap());
    assert!(report
        .copy_text()
        .contains("Video attached in the feedback draft: clip.webm (1 KB)."));
    let (url, server) = mock(201, RECEIPT);
    assert_eq!(send_to(&url, &report).unwrap().issue_number, 123);
    let wire = server.join().unwrap();
    let text = String::from_utf8_lossy(&wire);
    assert!(
        text.contains("name=\"video\"; filename=\"clip.webm\""),
        "{text}"
    );
    assert!(text
        .to_ascii_lowercase()
        .contains("content-type: video/webm"));
    assert!(wire.windows(body.len()).any(|b| b == body));
    assert!(!text.contains("name=\"screenshot\""));
}
#[test]
fn a_video_that_grew_past_the_cap_or_a_bare_413_keeps_the_draft_with_a_reason() {
    let dir = tempfile::tempdir().unwrap();
    let path = sized_file(dir.path(), "grow.mp4", 1_000);
    let mut report = report();
    report.video = Some(video_file(&path).unwrap());
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(MAX_VIDEO + 1)
        .unwrap();
    // Validation fails before any connection is made.
    assert!(send_to("http://127.0.0.1:9/feedback", &report)
        .unwrap_err()
        .to_string()
        .contains("the limit is"));
    let mut report = self::report();
    report.video = Some(video_file(&sized_file(dir.path(), "ok.mp4", 10)).unwrap());
    let (url, server) = mock(413, "{}");
    let error = send_to(&url, &report).unwrap_err().to_string();
    server.join().unwrap();
    assert!(
        error
            .contains("Attachments exceed the server's size limit. Your draft is kept. (HTTP 413)"),
        "{error}"
    );
}

#[test]
fn fallback_rebuilds_the_same_report_after_transport_failure() {
    let closed = TcpListener::bind("127.0.0.1:0").unwrap();
    let primary = format!("http://{}/feedback", closed.local_addr().unwrap());
    drop(closed);
    let mut report = report();
    report.screenshot = Some(Arc::new(image(image::ImageFormat::Png)));
    report.log = Some("selected diagnostic attachment".into());
    let before = report.copy_text();
    let (fallback, received) = mock(201, RECEIPT);
    let receipt = send_using(&primary, &fallback, &report).unwrap();
    assert_eq!(receipt.issue_number, 123);
    assert_eq!(
        receipt.delivery_endpoint.as_deref(),
        Some(fallback.as_str())
    );
    let wire = received.join().unwrap();
    let text = String::from_utf8_lossy(&wire);
    assert!(text.contains(&report.text));
    assert!(text.contains("selected diagnostic attachment"));
    assert!(wire.windows(8).any(|b| b == b"\x89PNG\r\n\x1a\n"));
    assert_eq!(report.copy_text(), before);
}

#[test]
fn http_errors_and_invalid_receipts_do_not_try_fallback() {
    for (status, body) in [
        (400, "{}"),
        (401, "{}"),
        (403, "{}"),
        (413, "{}"),
        (429, "{}"),
        (500, "{}"),
        (201, "invalid JSON"),
        (
            201,
            r#"{"issue_number":123,"issue_url":"https://example.com/123"}"#,
        ),
    ] {
        let alternate = TcpListener::bind("127.0.0.1:0").unwrap();
        alternate.set_nonblocking(true).unwrap();
        let fallback = format!("http://{}/feedback", alternate.local_addr().unwrap());
        let (primary, received) = mock(status, body);
        assert!(send_using(&primary, &fallback, &report()).is_err());
        received.join().unwrap();
        assert!(
            matches!(alternate.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
        );
    }
}

#[test]
fn primary_delivery_wins_and_server_cannot_spoof_its_address() {
    let (primary, received) = mock(
        201,
        r#"{"issue_number":123,"issue_url":"https://git.oklabs.uk/BeFeast/okilum/issues/123","delivery_endpoint":"https://untrusted.example"}"#,
    );
    let receipt = send_using(&primary, "http://must-not-be-contacted.invalid", &report()).unwrap();
    received.join().unwrap();
    assert_eq!(receipt.delivery_endpoint.as_deref(), Some(primary.as_str()));
}

#[test]
fn interrupted_primary_upload_reopens_video_for_the_one_fallback() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let primary = format!("http://{}/feedback", listener.local_addr().unwrap());
    let interrupted = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut first = [0; 8192];
        assert!(socket.read(&mut first).unwrap() > 0);
        socket.shutdown(std::net::Shutdown::Both).unwrap();
    });
    let dir = tempfile::tempdir().unwrap();
    let video_path = dir.path().join("reopened.mp4");
    let video_bytes = b"native fallback streaming attachment".repeat(2048);
    std::fs::write(&video_path, &video_bytes).unwrap();
    let mut report = report();
    report.video = Some(video_file(&video_path).unwrap());
    let (fallback, received) = mock(201, RECEIPT);
    let receipt = send_using(&primary, &fallback, &report).unwrap();
    interrupted.join().unwrap();
    assert_eq!(
        receipt.delivery_endpoint.as_deref(),
        Some(fallback.as_str())
    );
    let wire = received.join().unwrap();
    assert!(wire
        .windows(video_bytes.len())
        .any(|part| part == video_bytes));
    assert_eq!(std::fs::read(video_path).unwrap(), video_bytes);
}

#[test]
fn truncated_http_rejection_keeps_its_status_and_never_retries() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let primary = format!("http://{}/feedback", listener.local_addr().unwrap());
    let rejected = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let _request = read_mock_request(&mut socket);
        socket
            .write_all(
                b"HTTP/1.1 403 Forbidden\r\nContent-Length: 1000\r\nConnection: close\r\n\r\nshort",
            )
            .unwrap();
        // Shutdown the write half after the declared, incomplete response.
        socket.shutdown(std::net::Shutdown::Write).unwrap();
    });
    let alternate = TcpListener::bind("127.0.0.1:0").unwrap();
    alternate.set_nonblocking(true).unwrap();
    let fallback = format!("http://{}/feedback", alternate.local_addr().unwrap());
    let error = send_using(&primary, &fallback, &report()).unwrap_err();
    rejected.join().unwrap();
    assert!(error.to_string().contains("HTTP 403"), "{error:#}");
    assert!(
        matches!(alternate.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
    );
}

#[test]
fn missing_local_attachment_does_not_contact_either_address() {
    let dir = tempfile::tempdir().unwrap();
    let path = sized_file(dir.path(), "removed.mp4", 10);
    let mut report = report();
    report.video = Some(video_file(&path).unwrap());
    std::fs::remove_file(path).unwrap();
    let primary = TcpListener::bind("127.0.0.1:0").unwrap();
    let alternate = TcpListener::bind("127.0.0.1:0").unwrap();
    primary.set_nonblocking(true).unwrap();
    alternate.set_nonblocking(true).unwrap();
    let error = send_using(
        &format!("http://{}/feedback", primary.local_addr().unwrap()),
        &format!("http://{}/feedback", alternate.local_addr().unwrap()),
        &report,
    )
    .unwrap_err();
    assert!(error.to_string().contains("video file"), "{error:#}");
    for listener in [primary, alternate] {
        assert!(
            matches!(listener.accept(), Err(error) if error.kind()==std::io::ErrorKind::WouldBlock)
        );
    }
}

#[test]
fn early_http_rejection_during_video_upload_never_retries() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let primary = format!("http://{}/feedback", listener.local_addr().unwrap());
    let rejected = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        // Stop at the headers: most of this 32 MB upload is still pending.
        let mut headers = Vec::new();
        while !headers.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            socket.read_exact(&mut byte).unwrap();
            headers.push(byte[0]);
        }
        socket
            .write_all(
                b"HTTP/1.1 403 Forbidden\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
            )
            .unwrap();
        socket.shutdown(std::net::Shutdown::Write).unwrap();
        // Drain unread upload data so teardown does not replace HTTP with TCP RST.
        let _ = std::io::copy(&mut socket, &mut std::io::sink());
    });
    let dir = tempfile::tempdir().unwrap();
    let path = sized_file(dir.path(), "early.mp4", 32 * 1024 * 1024);
    let mut report = report();
    report.video = Some(video_file(&path).unwrap());
    let alternate = TcpListener::bind("127.0.0.1:0").unwrap();
    alternate.set_nonblocking(true).unwrap();
    let fallback = format!("http://{}/feedback", alternate.local_addr().unwrap());
    let error = send_using(&primary, &fallback, &report).unwrap_err();
    rejected.join().unwrap();
    assert!(error.to_string().contains("HTTP 403"), "{error:#}");
    assert!(
        matches!(alternate.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
    );
}

#[test]
fn truncated_created_receipt_never_resends_an_accepted_report() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let primary = format!("http://{}/feedback", listener.local_addr().unwrap());
    let accepted = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let _request = read_mock_request(&mut socket);
        socket
            .write_all(
                b"HTTP/1.1 201 Created\r\nContent-Length: 1000\r\nConnection: close\r\n\r\n{",
            )
            .unwrap();
        socket.shutdown(std::net::Shutdown::Write).unwrap();
    });
    let alternate = TcpListener::bind("127.0.0.1:0").unwrap();
    alternate.set_nonblocking(true).unwrap();
    let fallback = format!("http://{}/feedback", alternate.local_addr().unwrap());
    let error = send_using(&primary, &fallback, &report()).unwrap_err();
    accepted.join().unwrap();
    assert!(error.to_string().contains("server accepted"), "{error:#}");
    assert!(
        matches!(alternate.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
    );
}

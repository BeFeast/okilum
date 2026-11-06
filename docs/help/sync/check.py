#!/usr/bin/env python3
"""Offline Help behavior: all user-facing resources resolve inside the bundle."""
from hashlib import sha256
from html.parser import HTMLParser
import base64
import json
from pathlib import Path
import re
import subprocess
from urllib.parse import unquote, urlsplit

ROOT = Path(__file__).resolve().parent


class Page(HTMLParser):
    def __init__(self):
        super().__init__()
        self.refs = []
        self.ids = set()
        self.text = []
        self.tags = []

    def handle_starttag(self, tag, attrs):
        attrs = dict(attrs)
        self.tags.append((tag, attrs))
        if "id" in attrs:
            self.ids.add(attrs["id"])
        for name in ("src", "href", "poster"):
            if name in attrs:
                self.refs.append(attrs[name])

    def handle_data(self, data):
        self.text.append(data)


def check_reference(ref, ids):
    url = urlsplit(ref)
    assert not url.scheme and not url.netloc, f"Nonlocal Help resource: {ref}"
    assert not url.query, f"Help resource depends on query: {ref}"
    if url.path:
        target = ROOT.joinpath(unquote(url.path)).resolve()
        assert target.is_relative_to(ROOT) and target.is_file(), ref
    elif url.fragment:
        assert unquote(url.fragment) in ids, ref


def check_media(probe, tutorial):
    assert abs(float(probe["format"]["duration"]) - tutorial["duration_seconds"]) < 0.01
    assert int(probe["format"]["size"]) == tutorial["size_bytes"]
    video = [s for s in probe["streams"] if s["codec_type"] == "video"]
    audio = [s for s in probe["streams"] if s["codec_type"] == "audio"]
    assert len(video) == len(audio) == 1, "Tutorial must contain video and narration"
    for key, value in tutorial["video"].items():
        assert video[0]["codec_name" if key == "codec" else key] == value
    assert audio[0]["codec_name"] == tutorial["audio"]["codec"]


def main():
    html = (ROOT / "index.html").read_text()
    page = Page()
    page.feed(html)
    for ref in page.refs:
        check_reference(ref, page.ids)
    for bad in ("https://example.test/video.mp4", "//example.test/a", "../missing", "assets/okilum-sync-tutorial.mp4?expires=7"):
        try:
            check_reference(bad, page.ids)
        except AssertionError:
            pass
        else:
            raise AssertionError(f"Negative control accepted: {bad}")
    # CSS fonts are embedded. The page/player must never request hosted media.
    for ref in re.findall(r"url\(['\"]?([^)'\"]+)", html):
        assert ref.startswith("data:"), ref
        if ref.startswith("data:font/ttf;base64,"):
            font = base64.b64decode(ref.split(",", 1)[1], validate=True)
            assert font.startswith(b"\x00\x01\x00\x00"), "Invalid embedded TrueType font"
    assert not re.search(r"\b(fetch|XMLHttpRequest|WebSocket)\s*\(", html)
    manifest = json.loads((ROOT / "provenance.json").read_text())
    for relative, digest in manifest["files"].items():
        assert sha256((ROOT / relative).read_bytes()).hexdigest() == digest, relative
    expected = {
        "assets/okilum-sync-tutorial.mp4": "f11d5554d16c0b2e3f504743132e111f3c66940cd4facf35c5f2bba43cb2ce7b",
        "captions.vtt": "32046a8faa3a9ac4e5023b4635703d453a7859a02abb2a3d22e08112ad491a1a",
    }
    for relative, digest in expected.items():
        assert manifest["files"][relative] == digest, relative
    probe = json.loads(subprocess.check_output([
        "ffprobe", "-v", "error", "-show_entries",
        "format=duration,size:stream=codec_type,codec_name,width,height", "-of", "json",
        str(ROOT / "assets/okilum-sync-tutorial.mp4"),
    ], text=True))
    check_media(probe, manifest["tutorial"])
    for fault in ("missing_audio", "old_duration"):
        bad = json.loads(json.dumps(probe))
        if fault == "missing_audio":
            bad["streams"] = [s for s in bad["streams"] if s["codec_type"] != "audio"]
        else:
            bad["format"]["duration"] = "18.4"
        try:
            check_media(bad, manifest["tutorial"])
        except AssertionError:
            pass
        else:
            raise AssertionError(f"Negative media control accepted: {fault}")
    videos = [attrs for tag, attrs in page.tags if tag == "video"]
    assert len(videos) == 1 and "muted" not in videos[0]
    assert ("source", {"src": "assets/okilum-sync-tutorial.mp4", "type": "video/mp4"}) in page.tags
    tracks = [attrs for tag, attrs in page.tags if tag == "track"]
    assert len(tracks) == 1 and tracks[0]["src"] == "captions.vtt"
    assert tracks[0]["kind"] == "captions" and tracks[0]["srclang"] == "en"
    assert "default" in tracks[0]
    vtt = (ROOT / "captions.vtt").read_text()
    assert vtt.startswith("WEBVTT\n")
    def seconds(timestamp):
        h, m, s = timestamp.split(":")
        return int(h) * 3600 + int(m) * 60 + float(s)
    cues = [
        [seconds(a), seconds(b), text]
        for a, b, text in re.findall(
            r"(\d\d:\d\d:\d\d\.\d{3}) --> (\d\d:\d\d:\d\d\.\d{3})\n([^\n]+)", vtt
        )
    ]
    assert len(cues) == 15
    assert all(0 <= a < b <= float(probe["format"]["duration"]) for a, b, _ in cues)
    assert all(cues[i][1] <= cues[i + 1][0] for i in range(len(cues) - 1))
    assert json.loads(re.search(r"const cues = (\[.*?\]);", html).group(1)) == cues
    transcript = Page()
    transcript.feed(html.split('<div class="transcript">', 1)[1].split("</div>", 1)[0])
    spoken = [part for part in transcript.text if part.strip().startswith(("Desktop:", "Laptop:"))]
    assert [part.strip() for part in spoken] == [text for _, _, text in cues]
    guide = (ROOT / "setup.md").read_text()
    headings = re.findall(r"^## ([1-6])\. (.*)$", guide, re.M)
    assert [number for number, _ in headings] == list("123456")
    for required in ("including two fresh installations", "Share with another computer…", "Share with my computers", "Get notes from another computer", "Only local network and VPN", ".okilum-conflict-<time>-<random>", "This computer", "Use this connection", "Create pairing link", "Copy link", "Request connection", "Approve", "Choose folder", "Start first sync", "First sync complete", "not real-time latency evidence or Oleg acceptance"):
        assert required in guide, required
    text = " ".join(page.text)
    for required in ("Share with another computer…", "Share with my computers", "Get notes from another computer", "Only local network and VPN", "one hour", "#2043", "#2049", "macOS beta 29262", "Windows is coming"):
        assert required in text, required
    for stale in ("Fresh-host bootstrap is not ready", "existing, configured Syncthing runtime on both computers", "fresh passkey confirmation"):
        assert stale not in guide and stale not in text, stale
    assert "First connection · 1:18 · English narration and captions" in text
    for stale in ("silent", "18.4 seconds", "existing pair", "no first connection recording"):
        assert stale not in text and stale not in guide, stale
    # File export must contain every runtime HTML dependency, not just the page.
    shell = (ROOT.parents[2] / "crates/okilum-shell/src/reader_sync_help.rs").read_text()
    for ref in page.refs:
        path = urlsplit(ref).path
        if path:
            assert f'("{path}",' in shell or f'"{path}",' in shell, path
            assert path in manifest["files"], path
    assert '.on_click(|_, window, cx| show(window, cx))' in shell
    assert "Existing pair only" not in shell
    print("PASS: offline links, negative controls, tutorial hashes/duration/audio, captions/transcript, setup sequence, bundled export closure")


if __name__ == "__main__":
    main()

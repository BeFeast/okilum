#!/usr/bin/env python3
"""Offline Help behavior: all user-facing resources resolve inside the bundle."""
from hashlib import sha256
from html.parser import HTMLParser
import json
from pathlib import Path
import re
from urllib.parse import unquote, urlsplit

ROOT = Path(__file__).resolve().parent


class Page(HTMLParser):
    def __init__(self):
        super().__init__()
        self.refs = []
        self.ids = set()
        self.text = []

    def handle_starttag(self, tag, attrs):
        attrs = dict(attrs)
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


def main():
    html = (ROOT / "index.html").read_text()
    page = Page()
    page.feed(html)
    for ref in page.refs:
        check_reference(ref, page.ids)
    for bad in ("https://example.test/video.mp4", "//example.test/a", "../missing", "assets/sync-v6.mp4?expires=7"):
        try:
            check_reference(bad, page.ids)
        except AssertionError:
            pass
        else:
            raise AssertionError(f"Negative control accepted: {bad}")
    # CSS fonts are embedded. The page/player must never request hosted media.
    for ref in re.findall(r"url\(['\"]?([^)'\"]+)", html):
        assert ref.startswith("data:"), ref
    assert not re.search(r"\b(fetch|XMLHttpRequest|WebSocket)\s*\(", html)
    manifest = json.loads((ROOT / "provenance.json").read_text())
    for relative, digest in manifest["files"].items():
        assert sha256((ROOT / relative).read_bytes()).hexdigest() == digest, relative
    assert (ROOT / "assets/sync-v6.mp4").stat().st_size == 515220
    guide = (ROOT / "setup.md").read_text()
    headings = re.findall(r"^## ([1-6])\. (.*)$", guide, re.M)
    assert [number for number, _ in headings] == list("123456")
    for required in ("including two fresh installations", "Share with another computer…", "Share with my computers", "Get notes from another computer", "Only local network and VPN", ".okilum-conflict-<time>-<random>", "This computer", "Use this connection", "Copy pairing URL", "Paste pairing URL", "Confirm computer", "Choose local folder", "Bidirectional Sync active", "not real-time latency evidence or Oleg acceptance"):
        assert required in guide, required
    text = " ".join(page.text)
    for required in ("Share with another computer…", "Share with my computers", "Get notes from another computer", "Only local network and VPN", "10 minutes", "#2043", "#2049", "macOS is coming", "Windows is coming"):
        assert required in text, required
    for stale in ("Fresh-host bootstrap is not ready", "existing, configured Syncthing runtime on both computers", "fresh passkey confirmation"):
        assert stale not in guide and stale not in text, stale
    assert "not a recording of first connection" in text
    assert "6.3" in text and "18.4" in text
    # File export must contain every runtime HTML dependency, not just the page.
    shell = (ROOT.parents[2] / "crates/okilum-shell/src/reader_sync_help.rs").read_text()
    for ref in page.refs:
        path = urlsplit(ref).path
        if path:
            assert f'("{path}",' in shell or f'"{path}",' in shell, path
    assert '.on_click(|_, window, cx| show(window, cx))' in shell
    print("PASS: offline links, negative controls, v6 media hashes, setup sequence, bundled export closure")


if __name__ == "__main__":
    main()

#!/usr/bin/env python3
"""Verify offline closure, accessible instructions and real recording receipts."""
from hashlib import sha256
from html.parser import HTMLParser
import importlib.util
import json
from pathlib import Path
import re
import subprocess
import tempfile
import tomllib
from urllib.parse import unquote, urlsplit

ROOT = Path(__file__).resolve().parent


class Page(HTMLParser):
    def __init__(self):
        super().__init__()
        self.references = []
        self.ids = set()
        self.guide = []
        self.in_guide = False
        self.video = False
        self.captions = False

    def handle_starttag(self, tag, attrs):
        attrs = dict(attrs)
        if "id" in attrs:
            self.ids.add(attrs["id"])
        if tag == "article":
            self.in_guide = attrs.get("id") == "steps"
        if self.in_guide and tag in ("p", "li", "h2", "h3"):
            self.guide.append(" ")
        if tag == "figure":
            self.in_guide = False
        if tag == "video":
            self.video = "controls" in attrs and "autoplay" not in attrs
        if tag == "track":
            self.captions = attrs.get("kind") == "captions" and attrs.get("srclang") == "en"
        if tag == "img":
            assert attrs.get("alt"), "Meaningful image needs alt text"
        for key in ("href", "src", "poster"):
            if key in attrs:
                self.references.append((tag, key, attrs[key]))

    def handle_endtag(self, tag):
        if tag == "article":
            self.in_guide = False

    def handle_data(self, data):
        if self.in_guide:
            self.guide.append(data)


def reference(tag, key, ref, ids):
    url = urlsplit(ref)
    if url.scheme == "data" and tag == "link":
        return
    if url.scheme or url.netloc:
        assert tag == "a" and key == "href" and url.scheme == "https" and url.netloc == "okilum.app", ref
        return
    assert not url.query, ref
    if url.path:
        target = (ROOT / unquote(url.path)).resolve()
        assert target.is_relative_to(ROOT) and target.is_file(), ref
    elif url.fragment:
        assert unquote(url.fragment) in ids, ref


def normalize(text):
    return " ".join(text.split())


def main():
    html = (ROOT / "index.html").read_text()
    page = Page()
    page.feed(html)
    for args in page.references:
        reference(*args, page.ids)
    for ref in ("../sync/index.html", "assets/missing.mp4", "assets/board-tutorial.mp4?expires=1", "https://example.test/video.mp4", "//example.test/video.mp4"):
        try:
            reference("source", "src", ref, page.ids)
        except AssertionError:
            pass
        else:
            raise AssertionError(f"Invalid resource accepted: {ref}")
    for ref in re.findall(r"url\([\"']?([^)'\"]+)", html):
        reference("style", "src", ref, page.ids)
    assert page.video and page.captions
    assert not re.search(r"\b(fetch|XMLHttpRequest|WebSocket)\s*\(", html)
    written = (ROOT / "setup.md").read_text().split("\n", 1)[1]
    written = re.sub(r"^(?:#+ |\d+\. )", "", written, flags=re.M).replace("**", "").replace("`", "")
    assert normalize(written) in normalize("".join(page.guide)), "Website and in-app instructions diverged"
    manifest = json.loads((ROOT / "provenance.json").read_text())
    for name, digest in manifest["files"].items():
        assert sha256((ROOT / name).read_bytes()).hexdigest() == digest, name
    assert re.fullmatch(r"[0-9a-f]{40}", manifest["source_commit"])
    probe = json.loads(subprocess.check_output([
        "ffprobe", "-v", "error", "-show_entries",
        "format=duration,size:stream=codec_type,codec_name,width,height", "-of", "json",
        str(ROOT / "assets/board-tutorial.mp4"),
    ]))
    assert probe == manifest["video"]
    assert len(probe["streams"]) == 1 and probe["streams"][0]["codec_name"] == "h264"
    events = json.loads((ROOT / "events.json").read_text())
    assert len(events) == 28 and float(probe["format"]["duration"]) > events[-1]["start"]
    captions = (ROOT / "captions.vtt").read_text()
    for event in events:
        assert event["text"] in captions and event["text"] in html, event
    before = tomllib.loads((ROOT / "recorded-before.toml").read_text())["cards"]
    after = tomllib.loads((ROOT / "recorded-after.toml").read_text())["cards"]
    assert len(before) == len(after) == 1
    assert before[0]["id"] == after[0]["id"] == manifest["saved_state"]["same_card_id"]
    assert before[0]["state"] == "todo" and after[0]["state"] == "doing"
    assert {k: v for k, v in before[0].items() if k != "state"} == {k: v for k, v in after[0].items() if k != "state"}
    # Exercise the real source installer twice: the next public-site rebuild
    # must retain the guide and expose it from both existing page templates.
    spec = importlib.util.spec_from_file_location("board_site", ROOT / "install-site.py")
    installer = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(installer)
    with tempfile.TemporaryDirectory() as directory:
        site = Path(directory)
        (site / "src").mkdir()
        build = "SRC = ROOT / 'src'\nOUT = ROOT / 'public'\n"
        (site / "build.py").write_text(build)
        footer = '<nav class="footer-links" aria-label="Footer navigation"></nav>'
        for name in ("home.html", "shell.html"):
            (site / "src" / name).write_text(footer)
        installer.install(site)
        installer.install(site)
        assert (site / "build.py").read_text() == build + installer.HOOK
        for name in ("home.html", "shell.html"):
            assert (site / "src" / name).read_text().count('href="/help/board/"') == 1
        for name in installer.FILES:
            assert (site / "src/help/board" / name).read_bytes() == (ROOT / name).read_bytes()
    print("Board Help: offline closure, matching steps, captions, provenance, saved state and site integration PASS")


if __name__ == "__main__":
    main()

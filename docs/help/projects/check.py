#!/usr/bin/env python3
"""Projects Help: offline closure, matching instructions, recording receipts, site install."""
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


def check_media(probe, expected):
    # FFprobe 8 emits an empty stream_groups section that FFprobe 6 omits.
    # Compare every requested media property, not version-specific envelope keys.
    for section in ("streams", "format"):
        assert probe[section] == expected[section], (section, probe, expected)


def installer(name):
    spec = importlib.util.spec_from_file_location(f"{name}_site", ROOT.parent / name / "install-site.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def main():
    html = (ROOT / "index.html").read_text()
    page = Page()
    page.feed(html)
    for args in page.references:
        reference(*args, page.ids)
    for ref in ("../board/index.html", "assets/missing.mp4", "assets/projects-tutorial.mp4?expires=1", "https://example.test/video.mp4", "//example.test/video.mp4"):
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
    written = re.sub(r"^\s*(?:#+ |\d+\. )", "", written, flags=re.M).replace("**", "").replace("`", "")
    assert normalize(written) in normalize("".join(page.guide)), "Website and in-app instructions diverged"
    # The app bundles exactly the files the website receives.
    source = (ROOT.parents[2] / "crates/okilum-shell/src/reader_projects_help.rs").read_text()
    bundled = re.findall(r'^\s*"([^"]+)",\s*\n\s*include_bytes!\("[./]+docs/help/projects/([^"]+)"\)', source, flags=re.M)
    assert bundled and all(a == b for a, b in bundled), bundled
    assert "docs/help/projects/setup.md" in source
    site = installer("projects")
    assert sorted(a for a, _ in bundled) == sorted(site.FILES), (bundled, site.FILES)
    manifest = json.loads((ROOT / "provenance.json").read_text())
    for name, digest in manifest["files"].items():
        assert sha256((ROOT / name).read_bytes()).hexdigest() == digest, name
    assert set(site.FILES) <= set(manifest["files"])
    assert re.fullmatch(r"[0-9a-f]{40}", manifest["source_commit"])
    assert manifest["build"].rsplit(".", 1)[1] in manifest["artifact_url"]
    probe = json.loads(subprocess.check_output([
        "ffprobe", "-v", "error", "-show_entries",
        "format=duration,size:stream=codec_type,codec_name,width,height", "-of", "json",
        str(ROOT / "assets/projects-tutorial.mp4"),
    ]))
    check_media(probe, manifest["video"])
    changed = {key: json.loads(json.dumps(probe[key])) for key in ("streams", "format")}
    changed["format"]["duration"] = "18.4"
    try:
        check_media(changed, manifest["video"])
    except AssertionError:
        pass
    else:
        raise AssertionError("Changed media property accepted")
    assert len(probe["streams"]) == 1 and probe["streams"][0]["codec_name"] == "h264"
    events = json.loads((ROOT / "events.json").read_text())
    assert len(events) == 31 and float(probe["format"]["duration"]) > events[-1]["start"]
    assert [e["start"] for e in events] == sorted(e["start"] for e in events)
    captions = (ROOT / "captions.vtt").read_text()
    for event in events:
        assert event["text"] in captions and event["text"] in html, event
    # The recorded manifest and card are the saved results shown in the video.
    project = tomllib.loads((ROOT / "recorded-project.toml").read_text())
    assert project["name"] == "Kitchen remodel" and project["kind"] == "knowledge"
    assert project["index"] == "Kitchen remodel/@index.md" and "t3" not in project
    saved = manifest["saved_state"]
    cards = (ROOT / saved["cards"]).read_bytes()
    assert sha256(cards).hexdigest() == saved["cards_sha256_after_reopen"]
    cards = tomllib.loads(cards.decode())["cards"]
    assert len(cards) == 1
    assert cards[0]["title"] == saved["card_title"] and cards[0]["state"] == saved["card_state"] == "todo"
    # Exercise both real installers twice in either order: one copy hook, both
    # guides copied byte for byte and linked once from both page templates.
    for order in (("projects", "board"), ("board", "projects")):
        with tempfile.TemporaryDirectory() as directory:
            site_root = Path(directory)
            (site_root / "src").mkdir()
            build = "SRC = ROOT / 'src'\nOUT = ROOT / 'public'\n"
            (site_root / "build.py").write_text(build)
            footer = '<nav class="footer-links" aria-label="Footer navigation"></nav>'
            for name in ("home.html", "shell.html"):
                (site_root / "src" / name).write_text(footer)
            for guide in order * 2:
                installer(guide).install(site_root)
            assert (site_root / "build.py").read_text() == build + site.HOOK
            for name in ("home.html", "shell.html"):
                text = (site_root / "src" / name).read_text()
                for guide in ("projects", "board"):
                    assert text.count(f'href="/help/{guide}/"') == 1, (order, text)
            for name in site.FILES:
                assert (site_root / "src/help/projects" / name).read_bytes() == (ROOT / name).read_bytes()
    with tempfile.TemporaryDirectory() as directory:
        site_root = Path(directory)
        (site_root / "build.py").write_text("print('other layout')\n")
        try:
            site.install(site_root)
        except ValueError:
            pass
        else:
            raise AssertionError("Unsupported site layout accepted")
        assert not (site_root / "src").exists()
    print("Projects Help: offline closure, matching steps, bundle list, captions, provenance, saved state and site integration PASS")


if __name__ == "__main__":
    main()

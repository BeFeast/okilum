"""Obsidian theme converter (#3157): CSS to okilum-theme/v1 JSON with a report.

Synthetic CSS only; the real theme runs and their reports are in #3157.
"""
import contextlib
import importlib.util
import io
import json
from pathlib import Path
import re
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts/themes/obsidian-theme-convert.py"
_spec = importlib.util.spec_from_file_location("obsidian_theme_convert", SCRIPT)
conv = importlib.util.module_from_spec(_spec)
sys.modules[_spec.name] = conv  # dataclasses look the module up by name
_spec.loader.exec_module(conv)

# Pre-1.0 dialect, dark only (like Obsidianite): var chains, an `-rgb` variable
# holding a hex value, an RGB triplet, gradient decorations, fonts, keyframes.
PRE_1_0_DARK = """
@font-face { font-family: 'Rubik'; src: url(rubik.woff2); }
/* .theme-light { --background-primary: #fff; } */
:root { --brand: #0fb6d6; --brand-rgb: 15, 182, 214; }
.theme-dark {
  --background-primary: #100e17;
  --background-secondary: #191621;
  --text-normal: #bebebe;
  --text-accent: var(--brand);
  --text-sub-accent: #f4569d;
  --text-title-h1: var(--text-accent);
  --text-title-h2: #cbdbe5;
  --text-a: #6bcafb;
  --periwinkle: rgb(122, 162, 247);
  --text-mark: #263d92;
  --interactive-accent-rgb: #3dd7fb;
  --tag-background: rgba(var(--brand-rgb), 0.15);
  --text-highlight-bg: rgba(244, 86, 157, 0.25);
  --background-modifier-border: rgba(var(--interactive-accent-rgb), 0.05);
}
h1, h2 { font-family: 'Rubik'; font-weight: 600; }
.markdown-preview-view h2,
.markdown-preview-view h3 {
  margin: 45px 0 20px 0;
  border-bottom: 1px solid;
  border-image-source: linear-gradient(to right, var(--text-sub-accent), #100e17, #100e17);
}
.markdown-preview-view hr {
  border-image-source: linear-gradient(to right, transparent, var(--text-accent), transparent);
}
.markdown-preview-view blockquote {
  border-left: 3px rgba(14, 210, 247, 0.5) solid;
  background: linear-gradient(135deg, rgba(32, 28, 41, 0.45), #100e17);
}
.markdown-preview-view blockquote::before {
  width: 60%;
  background: linear-gradient(90deg, rgba(13, 185, 215, 0.5), #13111a);
}
.internal-link { background-image: linear-gradient(to bottom, rgba(14, 210, 247, .55) 0%, rgba(14, 210, 247, .55) 100%); background-size: 5px 5px; }
strong {
  -webkit-background-clip: text;
  -webkit-text-fill-color: transparent;
  background-image: linear-gradient(62deg, #87c2fd 0%, #dcb9fc 100%) !important;
}
.theme-dark .cm-s-obsidian span.cm-keyword { color: #66d9ef; }
.token.string { color: #50fa7b; }
@keyframes bounce { from { transform: scale(1); } to { transform: scale(1.2); } }
.workspace-ribbon { width: 42px; color: var(--text-normal); }
"""

# 1.0+ dialect with both modes: accent from hsl() with calc(), color-mix(),
# --h1-color, --link-color, a Style Settings variant and an @media block.
V1_BOTH = """
body {
  --accent-h: 210; --accent-s: 70%; --accent-l: 50%;
  --h1-color: var(--color-red);
}
.theme-light {
  --color-base-00: #ffffff;
  --color-base-100: #1f2328;
  --color-red: #c4314b;
  --background-secondary: color-mix(in srgb, var(--color-base-00) 90%, black);
  --link-color: hsl(var(--accent-h), var(--accent-s), calc(var(--accent-l) - 10%));
}
.theme-dark {
  --color-base-00: #1c1d21;
  --color-base-100: #e6e6e6;
  --color-red: #ff6b81;
  --link-color: hsl(var(--accent-h), var(--accent-s), calc(var(--accent-l) + 15%));
}
.theme-dark.minimal-dark-black { --color-base-00: #000; }
@media (max-width: 400px) { .theme-dark { --color-base-00: #222; } }
h1 { color: var(--h1-color); letter-spacing: -0.02em; }
"""

# 1.0+ theme whose headings are plain text and whose links are the accent.
PLAIN_HEADINGS = """
.theme-light { --accent-h: 215; --accent-s: 80%; --accent-l: 45%; --color-purple: #7a4cc4; }
.theme-dark { --accent-h: 215; --accent-s: 80%; --accent-l: 65%; --color-purple: #b48ef0; }
"""


def ratio(a, b):
    return conv.contrast(conv.parse_color(a), conv.parse_color(b))


def hue(c):
    return conv.hsl(conv.parse_color(c))[0] * 360


class ColourValues(unittest.TestCase):
    def test_parses_the_css_colour_forms_obsidian_themes_use(self):
        hexes = {
            "#abc": "#aabbcc",
            "#11223380": "#11223380",
            "rgb(255, 0, 0)": "#ff0000",
            "rgba(0 0 255 / 50%)": "#0000ff80",
            "hsl(120, 100%, 25%)": "#008000",
            "hsl(210 70% calc(50% - 10%))": "#1f66ad",
            "color-mix(in srgb, #ffffff 50%, #000000)": "#808080",
            "color-mix(in srgb, #ff0000, #0000ff)": "#800080",
            "color-mix(in oklab, white, black)": "#636363",
            "rgba(#3dd7fb, 0.5)": "#3dd7fb80",
            "15, 182, 214": "#0fb6d6",
            "white": "#ffffff",
            "transparent": "#00000000",
        }
        for css, expected in hexes.items():
            with self.subTest(css=css):
                self.assertEqual(conv.to_hex(conv.parse_color(css)), expected)

    def test_var_chains_fallbacks_triplets_and_cycles(self):
        sheet = conv.parse_css("""
            body { --a: var(--b); --b: var(--c, #123456); --rgb: 1, 2, 3;
                   --t: rgba(var(--rgb), 0.5); --loop: var(--loop2); --loop2: var(--loop); }
        """)
        r = conv.Resolver(conv.collect_variables(sheet), "dark")
        self.assertEqual(conv.to_hex(r.color("var(--a)")), "#123456")
        self.assertEqual(conv.to_hex(r.color("var(--t)")), "#01020380")
        with self.assertRaises(conv.Unresolved):
            r.color("var(--loop)")
        with self.assertRaises(conv.Unresolved):
            r.color("var(--nothing)")

    def test_scopes_keep_obsidians_precedence_and_skip_variants(self):
        self.assertEqual(conv.scope_of(":root"), ("base", 0))
        self.assertEqual(conv.scope_of("body"), ("base", 1))
        self.assertEqual(conv.scope_of("body.theme-dark"), ("dark", 2))
        self.assertEqual(conv.scope_of(".theme-light"), ("light", 2))
        for variant in (".theme-dark.minimal-dark-black", ".theme-light .view-actions",
                        "body.is-mobile", ".theme-dark:not(.x)"):
            self.assertIsNone(conv.scope_of(variant), variant)
        sheet = conv.parse_css(".theme-dark { --x: #111; } :root { --x: #222; } body { --x: #333; }")
        self.assertEqual(conv.collect_variables(sheet).theme["dark"]["--x"][0], "#111")


class Conversion(unittest.TestCase):
    def assert_gates(self, theme, report):
        failures = [c for c in report.contrast if not c[4]]
        self.assertEqual(failures, [])
        for mode in conv.MODES:
            entry = theme["modes"][mode]
            for key in conv.TOKEN_ORDER:
                self.assertRegex(entry[key], r"^#[0-9a-f]{6}([0-9a-f]{2})?$", f"{mode} {key}")
            gap = abs(hue(entry["heading"]) - hue(entry["link"]))
            self.assertGreaterEqual(min(gap, 360 - gap), 25, f"{mode} heading vs link hue")
            self.assertNotEqual(entry["heading"], entry["text"])

    def test_pre_1_0_dark_theme_converts_with_derived_light_mode_and_slots(self):
        theme, report = conv.convert(PRE_1_0_DARK, theme_id="sample", name="Sample",
                                     credit="synthetic, MIT")
        self.assertEqual(theme["schema"], "okilum-theme/v1")
        self.assertEqual(report.dialect, "pre-1.0")
        self.assertEqual(report.derived_mode, "light")
        dark, light = theme["modes"]["dark"], theme["modes"]["light"]
        self.assertEqual(dark["canvas"], "#100e17")
        self.assertEqual(dark["text"], "#bebebe")
        self.assertEqual(dark["accent"], "#0fb6d6")
        self.assertEqual(dark["heading"], "#0fb6d6")
        self.assertEqual(dark["heading2"], "#cbdbe5")
        # --text-a sits 11° from the cyan heading, so links take the theme's
        # periwinkle, the nearest of its own colours at least 25° away.
        self.assertEqual(dark["link"], "#7aa2f7")
        self.assertIn(("dark", "link", "#6bcafb", "#7aa2f7"),
                      [a[:4] for a in report.adjustments])
        self.assertEqual(dark["highlight"], "#f4569d40")
        # Light mode is derived: light canvas, same accent and heading hues.
        self.assertGreater(conv.luminance(conv.parse_color(light["canvas"])), 0.85)
        for key in ("accent", "heading", "link"):
            gap = abs(hue(light[key]) - hue(dark[key]))
            self.assertLess(min(gap, 360 - gap), 6, key)
        self.assert_gates(theme, report)
        # Decoration slots, both modes.
        deco = dark["decorations"]
        self.assertEqual(deco["headingRule"], {"levels": [2, 3], "from": "#f4569d",
                                               "to": "#100e17", "toAt": 0.5})
        self.assertEqual(deco["hr"], {"edge": "#00000000", "middle": "#0fb6d6"})
        self.assertEqual(deco["boldGradient"], {"angle": 62.0, "from": "#87c2fd", "to": "#dcb9fc"})
        self.assertEqual(deco["blockquote"]["angle"], 135.0)
        self.assertEqual(deco["blockquote"]["bars"]["top"]["width"], 0.6)
        self.assertEqual(deco["linkMarker"], {"internal": "#0ed2f78c", "heightPx": 5.0})
        self.assertEqual(dark["syntax"], {"keyword": "#66d9ef", "string": "#50fa7b"})
        light_bold = light["decorations"]["boldGradient"]
        for end in ("from", "to"):
            self.assertGreaterEqual(ratio(light_bold[end], light["canvas"]), 4.5)
        self.assertEqual(light["decorations"]["headingRule"]["to"], light["canvas"])
        # Everything left behind is reported, by rule and line.
        rendered = conv.render_report(report, theme, "sample.css")
        self.assertIn("@font-face (fonts are out of scope)", rendered)
        self.assertIn("@keyframes", rendered)
        self.assertIn("{ font-family }", rendered)
        self.assertIn(".workspace-ribbon { width }", rendered)
        self.assertNotIn(".workspace-ribbon { color }", rendered)  # carried by `text`
        self.assertNotIn("{ border-image-source }", rendered)       # carried by headingRule
        self.assertIn("| dark | text on surface |", rendered)
        self.assertIn("--tag-background", rendered)                 # read by nothing
        self.assertIn("synthetic, MIT", theme["basis"])

    def test_v1_theme_with_both_modes(self):
        theme, report = conv.convert(V1_BOTH, theme_id="v1", name="V1")
        self.assertEqual(report.dialect, "1.0+")
        self.assertIsNone(report.derived_mode)
        light, dark = theme["modes"]["light"], theme["modes"]["dark"]
        self.assertEqual(light["canvas"], "#ffffff")
        self.assertEqual(dark["canvas"], "#1c1d21")  # not the variant's #000 or @media #222
        self.assertEqual(light["sidebar"], "#e6e6e6")  # color-mix 90% white + black
        self.assertEqual(light["accent"], conv.to_hex(conv.parse_color("hsl(210, 70%, 50%)")))
        self.assertEqual(dark["heading"], "#ff6b81")
        self.assertEqual(light["heading"], "#c4314b")
        self.assertEqual(light["link"], conv.to_hex(conv.parse_color("hsl(210, 70%, 40%)")))
        self.assert_gates(theme, report)
        rendered = conv.render_report(report, theme, "v1.css")
        self.assertIn(".theme-dark.minimal-dark-black (1 variables)", rendered)
        self.assertIn("(max-width: 400px) .theme-dark (1 variables)", rendered)
        self.assertIn("h1 { letter-spacing }", rendered)

    def test_plain_text_headings_take_a_theme_colour_and_keep_accent_links(self):
        theme, report = conv.convert(PLAIN_HEADINGS, theme_id="plain", name="Plain")
        for mode in conv.MODES:
            entry = theme["modes"][mode]
            self.assertEqual(entry["link"], entry["accent"], mode)
            self.assertGreater(conv.hsl(conv.parse_color(entry["heading"]))[1], 0.15)
        self.assertEqual(report.picks["heading"], "--color-purple")
        self.assertTrue(any("plain text" in n for n in report.notes))
        self.assert_gates(theme, report)

    def test_light_only_theme_derives_dark(self):
        css = ".theme-light { --background-primary: #fdf6e3; --text-normal: #586e75; --text-accent: #b58900; }"
        theme, report = conv.convert(css, theme_id="sol", name="Sol")
        self.assertEqual(report.derived_mode, "dark")
        self.assertLess(conv.luminance(conv.parse_color(theme["modes"]["dark"]["canvas"])), 0.05)
        self.assert_gates(theme, report)


class AppSyntaxSchema(unittest.TestCase):
    # The app reads `syntax` keyed by tree-sitter capture name (#3156,
    # brand.rs rejects unknown names): the toolkit palette has no `builtin`.
    APP_CAPTURES = {"attribute", "boolean", "comment", "constant", "constructor", "embedded",
                    "function", "keyword", "label", "number", "operator", "property",
                    "punctuation", "string", "tag", "title", "type", "variable"}

    def test_builtin_fills_function_only_when_the_theme_left_it_out(self):
        self.assertEqual(conv.app_syntax({"builtin": "#50fa7b", "string": "#f1fa8c"}),
                         {"function": "#50fa7b", "string": "#f1fa8c"})
        self.assertEqual(conv.app_syntax({"builtin": "#50fa7b", "function": "#ff79c6"}),
                         {"function": "#ff79c6"})

    def test_every_slot_is_written_as_an_app_capture(self):
        slots = {slot: "#123456" for slot in conv.SYNTAX_CLASSES}
        self.assertLessEqual(set(conv.app_syntax(slots)), self.APP_CAPTURES)


class ReviewFixes(unittest.TestCase):
    def test_non_linear_or_stopless_gradients_are_reported_not_fatal(self):
        css = PRE_1_0_DARK + """
.markdown-preview-view h4 { border-image-source: radial-gradient(circle, red, blue); }
.markdown-preview-view hr { border-image-source: linear-gradient(to right); }
blockquote { background: conic-gradient(red, blue); }
.internal-link { background-image: linear-gradient(to bottom); }
strong { -webkit-background-clip: text; background-image: radial-gradient(#fff, #000); }
"""
        theme, report = conv.convert(css, theme_id="g", name="G")
        rendered = conv.render_report(report, theme, "g.css")
        self.assertIn("radial-gradient is not a linear-gradient", rendered)
        self.assertIn("conic-gradient is not a linear-gradient", rendered)
        self.assertIn("linear-gradient with 0 colour stop(s)", rendered)
        self.assertIn("h4 { border-image-source }", rendered)
        self.assertIn("blockquote { background }", rendered)

    def test_find_marks_never_equal_find_colours(self):
        # A highlight equal to the warning colour made findMark == findMatch.
        css = """.theme-dark { --background-primary: #1e1f29; --text-normal: #f8f8f2;
            --text-accent: #bd93f9; --color-orange: #ffb86c; --text-highlight-bg: #ffb86c; }"""
        theme, report = conv.convert(css, theme_id="d", name="D")
        for mode in conv.MODES:
            e = theme["modes"][mode]
            self.assertNotEqual(e["findMark"], e["findMatch"], mode)
            self.assertNotEqual(e["findMarkCurrent"], e["findCurrent"], mode)
        self.assertEqual([c for c in report.contrast if not c[4]], [])
        self.assertTrue(any(c[1].startswith("findMark differs") for c in report.contrast))


class Contract(unittest.TestCase):
    def test_writes_every_token_the_app_reads(self):
        brand = (ROOT / "crates/okilum-shell-kit/src/brand.rs").read_text()
        resolve = brand.split("fn resolve(theme: ThemeId)", 1)[1].split("\nfn tokens(", 1)[0]
        read = set(re.findall(r'color\("(\w+)"\)', resolve))
        self.assertGreater(len(read), 30)
        self.assertEqual(read - set(conv.TOKEN_ORDER), set())
        shipped = json.loads((ROOT / "crates/okilum-shell/assets/themes/obsidianite.json").read_text())
        for mode in ("light", "dark"):
            keys = list(shipped["modes"][mode])
            # Every required token, in order; besides them only the optional
            # keys the app also reads (#3153 heading levels, #3156 syntax) or
            # the converter fills (decorations).
            self.assertEqual([k for k in keys if k in conv.TOKEN_ORDER], conv.TOKEN_ORDER, mode)
            optional = set(conv.HEADING_LEVELS) | {"decorations", "syntax"}
            self.assertEqual(set(keys) - set(conv.TOKEN_ORDER) - optional, set(), mode)

    def test_cli_writes_theme_and_report(self):
        with tempfile.TemporaryDirectory() as tmp:
            css = Path(tmp, "theme.css")
            css.write_text(PRE_1_0_DARK, encoding="utf-8")
            out, rep = Path(tmp, "t.json"), Path(tmp, "t.md")
            with contextlib.redirect_stderr(io.StringIO()) as summary:
                code = conv.main([str(css), "--id", "sample", "--name", "Sample",
                                  "--out", str(out), "--report", str(rep)])
            self.assertIn("94/94 checks pass", summary.getvalue())
            self.assertEqual(code, 0)
            self.assertEqual(json.loads(out.read_text())["id"], "sample")
            self.assertIn("## Not carried over", rep.read_text())


if __name__ == "__main__":
    unittest.main()

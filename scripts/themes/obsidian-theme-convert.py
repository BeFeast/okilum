#!/usr/bin/env python3
"""Convert an Obsidian theme's CSS into `okilum-theme/v1` JSON plus a report (#3157).

Offline tool, never used at runtime: the output is a plain theme file for
`crates/okilum-shell/assets/themes/`. See docs/design/obsidian-theme-converter.md.

    scripts/themes/obsidian-theme-convert.py theme.css --id obsidianite \
        --name Obsidianite --credit "github.com/bennyxguo/Obsidian-Obsidianite, MIT" \
        --out obsidianite.json --report obsidianite-report.md

What it does:

- reads custom properties from `:root`, `body`, `.theme-light`, `.theme-dark`
  (and their compounds such as `body.theme-dark`), lowest to highest
  precedence, over a table of Obsidian 1.x app defaults;
- resolves `var()` chains with fallbacks, RGB triplets
  (`rgba(var(--interactive-accent-rgb), .5)`), `hsl()` with `calc()`,
  `color-mix()` and named colours;
- maps both variable dialects (pre-1.0 `--text-title-h1`, `--text-a`,
  `--text-accent`; 1.0+ `--h1-color`, `--link-color`, `--color-accent`) onto
  every token a theme must name;
- recognises gradient headings, `hr`, blockquotes, link markers and gradient
  bold text and fills the decoration slots, plus a code syntax palette;
- derives the missing mode (usually light) from the one the theme ships;
- adjusts colours that miss the contrast gates of `brand.rs` and reports each
  change, and lists every rule and declaration it could not carry over.
"""

from __future__ import annotations

import argparse
import colorsys
import json
import math
import re
import sys
from dataclasses import dataclass, field
from pathlib import Path

SCHEMA = "okilum-theme/v1"
MODES = ("light", "dark")

# Tokens every theme names (brand.rs `resolve`), in the order the built-in
# theme files use.
TOKEN_ORDER = [
    "canvas", "surface", "surfaceRaised", "sidebar", "text", "textMuted",
    "border", "borderSubtle", "selected", "accent", "onAccent", "focus",
    "link", "tile", "warningTint", "textFaint", "heading", "hover",
    "missingLink", "codeBg", "codeBorder", "calloutQuestion",
    "calloutImportant", "notice", "highlight", "scrim", "findMatch",
    "findCurrent", "onStatus", "success", "warning", "danger", "info",
    "findMarkTrack", "findMark", "findMarkCurrent",
]
HEADING_LEVELS = [f"heading{n}" for n in range(1, 7)]
# Tokens that may stay translucent; all others are composited over the canvas.
TRANSLUCENT = {"highlight", "scrim"}

PRE_1_0_VARS = {
    "--text-title-h1", "--text-title-h2", "--text-title-h3", "--text-title-h4",
    "--text-title-h5", "--text-title-h6", "--text-a", "--text-a-hover",
    "--text-mark", "--pre-code", "--interactive-before",
}
V1_VARS = {
    "--h1-color", "--h2-color", "--h3-color", "--h4-color", "--h5-color",
    "--h6-color", "--link-color", "--link-unresolved-color", "--color-accent",
    "--accent-h", "--accent-s", "--accent-l", "--color-base-00",
    "--color-base-100", "--code-background", "--callout-question",
    "--color-red", "--color-green", "--color-blue", "--bold-color",
}

# Obsidian 1.x app defaults: just enough of app.css for a theme's own variables
# to resolve the way Obsidian resolves them. Used only for variables the theme
# does not set.
_COMMON_DEFAULTS = {
    "--color-accent": "hsl(var(--accent-h), var(--accent-s), var(--accent-l))",
    "--color-accent-1": "hsl(var(--accent-h), var(--accent-s), calc(var(--accent-l) + 2.5%))",
    "--color-accent-2": "hsl(var(--accent-h), var(--accent-s), calc(var(--accent-l) + 5%))",
    "--background-primary": "var(--color-base-00)",
    "--background-primary-alt": "var(--color-base-10)",
    "--background-secondary": "var(--color-base-20)",
    "--background-secondary-alt": "var(--color-base-05)",
    "--background-modifier-border": "var(--color-base-30)",
    "--background-modifier-hover": "rgba(var(--mono-rgb-100), 0.075)",
    "--background-modifier-active-hover": "hsla(var(--interactive-accent-hsl), 0.15)",
    "--interactive-accent-hsl": "var(--accent-h), var(--accent-s), var(--accent-l)",
    "--text-normal": "var(--color-base-100)",
    "--text-muted": "var(--color-base-70)",
    "--text-faint": "var(--color-base-50)",
    "--text-on-accent": "white",
    "--text-accent": "var(--color-accent)",
    "--interactive-accent": "var(--color-accent)",
    "--text-selection": "hsla(var(--interactive-accent-hsl), 0.25)",
    "--link-color": "var(--text-accent)",
    "--link-unresolved-color": "var(--text-accent)",
    "--h1-color": "inherit",
    "--h2-color": "inherit",
    "--h3-color": "inherit",
    "--h4-color": "inherit",
    "--h5-color": "inherit",
    "--h6-color": "inherit",
    "--code-background": "var(--background-primary-alt)",
    "--callout-question": "var(--color-orange-rgb)",
    "--callout-important": "var(--color-cyan-rgb)",
    "--text-success": "var(--color-green)",
    "--text-warning": "var(--color-orange)",
    "--text-error": "var(--color-red)",
}
OBSIDIAN_DEFAULTS = {
    "light": {
        **_COMMON_DEFAULTS,
        "--accent-h": "258", "--accent-s": "88%", "--accent-l": "66%",
        "--mono-rgb-0": "255, 255, 255", "--mono-rgb-100": "0, 0, 0",
        "--color-base-00": "#ffffff", "--color-base-05": "#fcfcfc",
        "--color-base-10": "#fafafa", "--color-base-20": "#f6f6f6",
        "--color-base-25": "#e3e3e3", "--color-base-30": "#e0e0e0",
        "--color-base-35": "#d4d4d4", "--color-base-40": "#bdbdbd",
        "--color-base-50": "#ababab", "--color-base-60": "#707070",
        "--color-base-70": "#5a5a5a", "--color-base-100": "#222222",
        "--color-red": "#e93147", "--color-red-rgb": "233, 49, 71",
        "--color-orange": "#ec7500", "--color-orange-rgb": "236, 117, 0",
        "--color-yellow": "#e0ac00", "--color-yellow-rgb": "224, 172, 0",
        "--color-green": "#08b94e", "--color-green-rgb": "8, 185, 78",
        "--color-cyan": "#00bfbc", "--color-cyan-rgb": "0, 191, 188",
        "--color-blue": "#086ddd", "--color-blue-rgb": "8, 109, 221",
        "--color-purple": "#7852ee", "--color-purple-rgb": "120, 82, 238",
        "--color-pink": "#d53984", "--color-pink-rgb": "213, 57, 132",
        "--text-highlight-bg": "rgba(255, 208, 0, 0.4)",
        "--background-modifier-cover": "rgba(220, 220, 220, 0.4)",
    },
    "dark": {
        **_COMMON_DEFAULTS,
        "--accent-h": "254", "--accent-s": "80%", "--accent-l": "68%",
        "--mono-rgb-0": "0, 0, 0", "--mono-rgb-100": "255, 255, 255",
        "--color-base-00": "#1e1e1e", "--color-base-05": "#212121",
        "--color-base-10": "#242424", "--color-base-20": "#262626",
        "--color-base-25": "#2a2a2a", "--color-base-30": "#363636",
        "--color-base-35": "#3f3f3f", "--color-base-40": "#555555",
        "--color-base-50": "#666666", "--color-base-60": "#999999",
        "--color-base-70": "#bababa", "--color-base-100": "#dadada",
        "--color-red": "#fb464c", "--color-red-rgb": "251, 70, 76",
        "--color-orange": "#e9973f", "--color-orange-rgb": "233, 151, 63",
        "--color-yellow": "#e0de71", "--color-yellow-rgb": "224, 222, 113",
        "--color-green": "#44cf6e", "--color-green-rgb": "68, 207, 110",
        "--color-cyan": "#53dfdd", "--color-cyan-rgb": "83, 223, 221",
        "--color-blue": "#027aff", "--color-blue-rgb": "2, 122, 255",
        "--color-purple": "#a882ff", "--color-purple-rgb": "168, 130, 255",
        "--color-pink": "#fa99cd", "--color-pink-rgb": "250, 153, 205",
        "--text-highlight-bg": "rgba(255, 208, 0, 0.4)",
        "--background-modifier-cover": "rgba(10, 10, 10, 0.4)",
    },
}

# Okilum token → CSS variables to try, first hit wins. Pre-1.0 themes use
# `--text-a` for links and `--text-accent` as the brand colour; 1.0+ themes use
# `--link-color` and `--color-accent`.
SOURCES = {
    "canvas": ["--background-primary"],
    "surface": ["--background-primary"],
    "surfaceRaised": ["--background-secondary", "--background-primary-alt"],
    "sidebar": ["--background-secondary"],
    "text": ["--text-normal"],
    "textMuted": ["--text-muted"],
    "textFaint": ["--text-faint"],
    "border": ["--background-modifier-border"],
    "selected": ["--text-selection", "--background-modifier-active-hover"],
    "onAccent": ["--text-on-accent"],
    "hover": ["--background-modifier-hover"],
    "missingLink": ["--link-unresolved-color"],
    "codeBg": ["--code-background", "--pre-code", "--background-primary-alt"],
    "calloutQuestion": ["--callout-question"],
    "calloutImportant": ["--callout-important"],
    "highlight": ["--text-highlight-bg"],
    "scrim": ["--background-modifier-cover"],
    "success": ["--color-green", "--text-success"],
    "warning": ["--color-orange", "--color-yellow", "--text-warning"],
    "danger": ["--color-red", "--text-error"],
    "info": ["--color-blue", "--color-cyan"],
}
DIALECT_SOURCES = {
    "pre-1.0": {
        "accent": ["--text-accent", "--interactive-accent", "--color-accent"],
        "link": ["--text-a", "--text-accent", "--link-color"],
        "heading": ["--text-title-h1", "--h1-color"],
        **{f"heading{n}": [f"--text-title-h{n}", f"--h{n}-color"] for n in range(1, 7)},
    },
    "1.0+": {
        "accent": ["--color-accent", "--interactive-accent", "--text-accent"],
        "link": ["--link-color", "--text-a", "--text-accent"],
        "heading": ["--h1-color", "--text-title-h1"],
        **{f"heading{n}": [f"--h{n}-color", f"--text-title-h{n}"] for n in range(1, 7)},
    },
}

SYNTAX_CLASSES = {
    "comment": ["cm-comment", "token.comment", "token.prolog"],
    "string": ["cm-string", "token.string"],
    "number": ["cm-number", "token.number"],
    "keyword": ["cm-keyword", "token.keyword"],
    "function": ["cm-def", "token.function"],
    "variable": ["cm-variable", "token.variable"],
    "type": ["cm-type", "token.class-name"],
    "builtin": ["cm-builtin", "token.builtin"],
    "operator": ["cm-operator", "token.operator"],
    "property": ["cm-property", "token.property"],
    "tag": ["cm-tag", "token.tag"],
    "attribute": ["cm-attribute", "token.attr-name"],
}

NAMED_COLORS = {
    "transparent": (0, 0, 0, 0), "black": (0, 0, 0, 1), "white": (255, 255, 255, 1),
    "red": (255, 0, 0, 1), "green": (0, 128, 0, 1), "blue": (0, 0, 255, 1),
    "yellow": (255, 255, 0, 1), "orange": (255, 165, 0, 1), "purple": (128, 0, 128, 1),
    "pink": (255, 192, 203, 1), "gray": (128, 128, 128, 1), "grey": (128, 128, 128, 1),
    "silver": (192, 192, 192, 1), "cyan": (0, 255, 255, 1), "aqua": (0, 255, 255, 1),
    "magenta": (255, 0, 255, 1), "fuchsia": (255, 0, 255, 1), "lime": (0, 255, 0, 1),
    "navy": (0, 0, 128, 1), "teal": (0, 128, 128, 1), "maroon": (128, 0, 0, 1),
    "olive": (128, 128, 0, 1), "gold": (255, 215, 0, 1), "coral": (255, 127, 80, 1),
    "tomato": (255, 99, 71, 1), "salmon": (250, 128, 114, 1), "violet": (238, 130, 238, 1),
    "indigo": (75, 0, 130, 1), "crimson": (220, 20, 60, 1), "khaki": (240, 230, 140, 1),
    "turquoise": (64, 224, 208, 1), "orchid": (218, 112, 214, 1), "plum": (221, 160, 221, 1),
    "lavender": (230, 230, 250, 1), "beige": (245, 245, 220, 1), "ivory": (255, 255, 240, 1),
    "whitesmoke": (245, 245, 245, 1), "gainsboro": (220, 220, 220, 1),
    "lightgray": (211, 211, 211, 1), "lightgrey": (211, 211, 211, 1),
    "darkgray": (169, 169, 169, 1), "darkgrey": (169, 169, 169, 1),
    "dimgray": (105, 105, 105, 1), "dimgrey": (105, 105, 105, 1),
    "slategray": (112, 128, 144, 1), "steelblue": (70, 130, 180, 1),
    "royalblue": (65, 105, 225, 1), "dodgerblue": (30, 144, 255, 1),
    "deepskyblue": (0, 191, 255, 1), "skyblue": (135, 206, 235, 1),
    "hotpink": (255, 105, 180, 1), "deeppink": (255, 20, 147, 1),
    "firebrick": (178, 34, 34, 1), "darkred": (139, 0, 0, 1),
    "seagreen": (46, 139, 87, 1), "forestgreen": (34, 139, 34, 1),
    "limegreen": (50, 205, 50, 1), "darkorange": (255, 140, 0, 1),
    "goldenrod": (218, 165, 32, 1), "chocolate": (210, 105, 30, 1),
    "sienna": (160, 82, 45, 1), "tan": (210, 180, 140, 1), "wheat": (245, 222, 179, 1),
    "snow": (255, 250, 250, 1), "linen": (250, 240, 230, 1), "mintcream": (245, 255, 250, 1),
    "aliceblue": (240, 248, 255, 1), "ghostwhite": (248, 248, 255, 1),
    "rebeccapurple": (102, 51, 153, 1), "mediumpurple": (147, 112, 219, 1),
    "slateblue": (106, 90, 205, 1), "darkslategray": (47, 79, 79, 1),
    "darkslategrey": (47, 79, 79, 1), "midnightblue": (25, 25, 112, 1),
}

FONT_PROPS = {
    "font", "font-family", "font-size", "font-weight", "font-style",
    "font-variant", "font-feature-settings", "font-variation-settings",
    "letter-spacing", "line-height", "word-spacing", "text-transform",
    "-webkit-font-smoothing", "text-rendering",
}
MOTION_PROPS = {"transition", "animation", "transition-duration",
                "transition-property", "animation-name", "animation-duration",
                "transition-timing-function", "animation-timing-function",
                "animation-delay", "transition-delay", "will-change"}


COLOR_PROPS = {"color", "background-color", "background", "border-color",
               "border-left-color", "border-bottom-color", "fill", "stroke"}


class Unresolved(Exception):
    pass


# ---------------------------------------------------------------------------
# CSS parsing


@dataclass
class Rule:
    selectors: list[str]
    decls: list[tuple[str, str, bool]]
    line: int
    condition: str | None = None
    nested: int = 0


@dataclass
class AtRule:
    kind: str
    prelude: str
    line: int


@dataclass
class Sheet:
    rules: list[Rule] = field(default_factory=list)
    at_rules: list[AtRule] = field(default_factory=list)


def strip_comments(text: str) -> str:
    """Comments become the same number of newlines so line numbers survive."""
    out, i = [], 0
    while True:
        start = _find_outside_strings(text, "/*", i)
        if start < 0:
            out.append(text[i:])
            return "".join(out)
        end = text.find("*/", start + 2)
        end = len(text) if end < 0 else end + 2
        out.append(text[i:start])
        out.append("\n" * text.count("\n", start, end))
        i = end


def _find_outside_strings(text: str, needle: str, start: int) -> int:
    i, quote = start, None
    while i < len(text):
        ch = text[i]
        if quote:
            if ch == "\\":
                i += 2
                continue
            if ch == quote:
                quote = None
        elif ch in "\"'":
            quote = ch
        elif text.startswith(needle, i):
            return i
        i += 1
    return -1


def _scan_block(text: str, i: int) -> int:
    """Index just past the `}` matching the `{` at `text[i]`."""
    depth, quote = 0, None
    while i < len(text):
        ch = text[i]
        if quote:
            if ch == "\\":
                i += 2
                continue
            if ch == quote:
                quote = None
        elif ch in "\"'":
            quote = ch
        elif ch == "{":
            depth += 1
        elif ch == "}":
            depth -= 1
            if depth == 0:
                return i + 1
        i += 1
    return len(text)


def split_top(text: str, sep: str) -> list[str]:
    """Split on `sep` outside parentheses, brackets and strings."""
    parts, depth, quote, cur = [], 0, None, []
    i = 0
    while i < len(text):
        ch = text[i]
        if quote:
            cur.append(ch)
            if ch == "\\" and i + 1 < len(text):
                cur.append(text[i + 1])
                i += 2
                continue
            if ch == quote:
                quote = None
        elif ch in "\"'":
            quote = ch
            cur.append(ch)
        elif ch in "([":
            depth += 1
            cur.append(ch)
        elif ch in ")]":
            depth -= 1
            cur.append(ch)
        elif ch == sep and depth == 0:
            parts.append("".join(cur))
            cur = []
        else:
            cur.append(ch)
        i += 1
    parts.append("".join(cur))
    return parts


def parse_css(text: str) -> Sheet:
    sheet = Sheet()
    clean = strip_comments(text)
    _parse_into(sheet, clean, 0, len(clean), None, clean)
    return sheet


def _line(full: str, pos: int) -> int:
    return full.count("\n", 0, pos) + 1


def _parse_into(sheet: Sheet, text: str, start: int, end: int,
                condition: str | None, full: str) -> None:
    i = start
    while i < end:
        # Next `{` or `;` at top level.
        j, quote, depth = i, None, 0
        while j < end:
            ch = text[j]
            if quote:
                if ch == "\\":
                    j += 2
                    continue
                if ch == quote:
                    quote = None
            elif ch in "\"'":
                quote = ch
            elif ch == "(":
                depth += 1
            elif ch == ")":
                depth -= 1
            elif depth == 0 and ch in "{;}":
                break
            j += 1
        prelude = text[i:j].strip()
        pos = i + (len(text[i:j]) - len(text[i:j].lstrip()))
        if j >= end:
            break
        if text[j] in ";}":
            if prelude.startswith("@"):
                kind = prelude.split(None, 1)[0][1:].lower()
                sheet.at_rules.append(AtRule(kind, prelude, _line(full, pos)))
            i = j + 1
            continue
        close = _scan_block(text, j)
        body_start, body_end = j + 1, close - 1
        if prelude.startswith("@"):
            kind = prelude.split(None, 1)[0][1:].lower()
            if kind in ("media", "supports", "container", "layer", "document"):
                _parse_into(sheet, text, body_start, body_end, prelude, full)
            else:
                sheet.at_rules.append(AtRule(kind, prelude, _line(full, pos)))
        else:
            body = text[body_start:body_end]
            nested = body.count("{")
            if nested:
                # CSS nesting: keep the declarations, report the nested blocks.
                flat, k = [], 0
                while k < len(body):
                    brace = body.find("{", k)
                    if brace < 0:
                        flat.append(body[k:])
                        break
                    semi = body.rfind(";", k, brace)
                    flat.append(body[k:semi + 1] if semi >= 0 else "")
                    k = _scan_block(body, brace)
                body = "".join(flat)
            selectors = [" ".join(s.split()) for s in split_top(prelude, ",") if s.strip()]
            sheet.rules.append(Rule(selectors, parse_decls(body), _line(full, pos),
                                    condition, nested))
        i = close


def parse_decls(body: str) -> list[tuple[str, str, bool]]:
    decls = []
    for part in split_top(body, ";"):
        if ":" not in part:
            continue
        prop, value = part.split(":", 1)
        prop = prop.strip()
        if not prop.startswith("--"):
            prop = prop.lower()
        value = " ".join(value.split())
        important = bool(re.search(r"!\s*important\s*$", value, re.I))
        value = re.sub(r"\s*!\s*important\s*$", "", value, flags=re.I).strip()
        if prop:
            decls.append((prop, value, important))
    return decls


# ---------------------------------------------------------------------------
# Variable scopes


def scope_of(selector: str) -> tuple[str, int] | None:
    """(`base`|`light`|`dark`, precedence level) for a pure variable scope.

    `:root`/`html` (level 0) < `body` (1) < `.theme-*` on body (2). Anything
    with a combinator or an extra class (Style Settings variants such as
    `.theme-dark.minimal-dark-black`) is not a scope and returns None.
    """
    s = selector.strip()
    if not s or re.search(r"[\s>+~]", s):
        return None
    parts = re.findall(r":root|\.[\w-]+|::?[\w-]+(?:\([^)]*\))?|\[[^\]]*\]|[\w-]+|\*", s)
    if "".join(parts) != s:
        return None
    mode, level = "base", -1
    for part in parts:
        if part in (":root", "html"):
            level = max(level, 0)
        elif part == "body":
            level = max(level, 1)
        elif part in (".theme-dark", ".theme-light"):
            if mode != "base":
                return None
            mode, level = part[7:], 2
        else:
            return None
    return (mode, level) if level >= 0 else None


def selector_mode(selector: str) -> str | None:
    if re.search(r"\.theme-dark(?![\w-])", selector):
        return "dark"
    if re.search(r"\.theme-light(?![\w-])", selector):
        return "light"
    return None


@dataclass
class Variables:
    """Custom properties per mode, the way Obsidian's cascade applies them."""
    theme: dict[str, dict[str, tuple[str, int]]]
    declared_in_mode: dict[str, int]
    variants: list[tuple[str, int, int]]
    conditional: list[tuple[str, str, int, int]]

    def lookup(self, mode: str, name: str) -> tuple[str, bool] | None:
        if name in self.theme[mode]:
            return self.theme[mode][name][0], True
        if name in OBSIDIAN_DEFAULTS[mode]:
            return OBSIDIAN_DEFAULTS[mode][name], False
        return None

    def theme_names(self) -> set[str]:
        return set(self.theme["light"]) | set(self.theme["dark"])


def collect_variables(sheet: Sheet) -> Variables:
    layered: dict[str, list[tuple[int, bool, int, str, str, int]]] = {m: [] for m in MODES}
    declared = {m: 0 for m in MODES}
    variants, conditional = [], []
    order = 0
    for rule in sheet.rules:
        custom = [(p, v, imp) for p, v, imp in rule.decls if p.startswith("--")]
        if not custom:
            continue
        for selector in rule.selectors:
            scope = scope_of(selector)
            if scope is None:
                if selector_mode(selector) or re.match(r"^(:root|html|body)", selector):
                    variants.append((selector, len(custom), rule.line))
                continue
            if rule.condition:
                conditional.append((selector, rule.condition, len(custom), rule.line))
                continue
            mode, level = scope
            targets = MODES if mode == "base" else (mode,)
            for m in targets:
                if mode != "base":
                    declared[m] += len(custom)
                for prop, value, important in custom:
                    order += 1
                    layered[m].append((level, important, order, prop, value, rule.line))
    theme = {m: {} for m in MODES}
    for m in MODES:
        for _level, _imp, _order, prop, value, line in sorted(layered[m]):
            theme[m][prop] = (value, line)
    return Variables(theme, declared, variants, conditional)


# ---------------------------------------------------------------------------
# Value resolution


def _matching_paren(text: str, open_index: int) -> int:
    depth = 0
    for k in range(open_index, len(text)):
        if text[k] == "(":
            depth += 1
        elif text[k] == ")":
            depth -= 1
            if depth == 0:
                return k
    raise Unresolved(f"unbalanced parentheses in {text!r}")


class Resolver:
    def __init__(self, variables: Variables, mode: str):
        self.vars = variables
        self.mode = mode

    def substitute(self, value: str, trail: list[str] | None = None,
                   stack: tuple[str, ...] = ()) -> str:
        """Replace every `var()` with its value, recursively."""
        out, i = [], 0
        while True:
            k = value.find("var(", i)
            if k < 0:
                out.append(value[i:])
                return "".join(out)
            close = _matching_paren(value, k + 3)
            inner = value[k + 4:close]
            parts = split_top(inner, ",")
            name = parts[0].strip()
            fallback = ",".join(parts[1:]).strip() if len(parts) > 1 else None
            out.append(value[i:k])
            out.append(self.variable(name, fallback, trail, stack))
            i = close + 1

    def variable(self, name: str, fallback: str | None, trail: list[str] | None,
                 stack: tuple[str, ...]) -> str:
        if name in stack:
            raise Unresolved(f"cycle {' → '.join(stack + (name,))}")
        hit = self.vars.lookup(self.mode, name)
        if hit is None or hit[0].strip() in ("", "initial", "unset"):
            if fallback is None:
                raise Unresolved(f"{name} is not set")
            return self.substitute(fallback, trail, stack)
        if trail is not None:
            trail.append(name if hit[1] else f"{name} (Obsidian default)")
        return self.substitute(hit[0], trail, stack + (name,))

    def color(self, value: str, trail: list[str] | None = None) -> tuple[float, ...]:
        text = self.substitute(value, trail).strip()
        if text.lower() in ("inherit", "currentcolor", "unset", "initial"):
            if trail is not None:
                trail.append(text)
            return self.color("var(--text-normal)", trail)
        return parse_color(text)


# ---------------------------------------------------------------------------
# Colour maths. A colour is (r, g, b, a) with channels in 0..1.


def _calc(expr: str) -> tuple[float, str]:
    tokens = re.findall(r"\d*\.\d+(?:e[+-]?\d+)?[a-z%]*|\d+(?:e[+-]?\d+)?[a-z%]*|[-+*/()]", expr.lower())
    pos = 0

    def atom():
        nonlocal pos
        tok = tokens[pos]
        pos += 1
        if tok == "(":
            value = add()
            pos += 1
            return value
        if tok == "-":
            v, u = atom()
            return -v, u
        if tok == "+":
            return atom()
        m = re.match(r"([\d.e+-]+)([a-z%]*)", tok)
        return float(m[1]), m[2]

    def mul():
        nonlocal pos
        v, u = atom()
        while pos < len(tokens) and tokens[pos] in "*/":
            op = tokens[pos]
            pos += 1
            w, wu = atom()
            if op == "*":
                v, u = v * w, u or wu
            else:
                v = v / w
        return v, u

    def add():
        nonlocal pos
        v, u = mul()
        while pos < len(tokens) and tokens[pos] in "+-":
            op = tokens[pos]
            pos += 1
            w, wu = mul()
            v = v + w if op == "+" else v - w
            u = u or wu
        return v, u

    try:
        return add()
    except (IndexError, ValueError, ZeroDivisionError) as err:
        raise Unresolved(f"calc({expr}): {err}") from err


def _eval_calcs(text: str) -> str:
    while True:
        k = text.lower().find("calc(")
        if k < 0:
            return text
        close = _matching_paren(text, k + 4)
        value, unit = _calc(text[k + 5:close])
        text = f"{text[:k]}{value:g}{unit}{text[close + 1:]}"


def _number(token: str, scale: float) -> float:
    token = token.strip()
    if token.endswith("%"):
        return float(token[:-1]) / 100 * scale
    if token in ("none", ""):
        return 0.0
    return float(re.sub(r"(deg|px)$", "", token))


def _hue(token: str) -> float:
    token = token.strip().lower()
    for unit, factor in (("deg", 1), ("turn", 360), ("rad", 180 / math.pi)):
        if token.endswith(unit):
            return float(token[: -len(unit)]) * factor
    return float(token)


def _args(inner: str) -> tuple[list[str], str | None]:
    inner = inner.strip()
    alpha = None
    if "/" in inner:
        inner, alpha = inner.rsplit("/", 1)
        alpha = alpha.strip()
    parts = [p.strip() for p in (inner.split(",") if "," in inner else inner.split())]
    parts = [p for p in parts if p]
    return parts, alpha


def parse_color(text: str) -> tuple[float, ...]:
    text = _eval_calcs(text.strip())
    low = text.lower()
    if low in NAMED_COLORS:
        r, g, b, a = NAMED_COLORS[low]
        return (r / 255, g / 255, b / 255, a)
    if low.startswith("#"):
        h = low[1:]
        if len(h) in (3, 4):
            h = "".join(c * 2 for c in h)
        if len(h) not in (6, 8) or not re.fullmatch(r"[0-9a-f]+", h):
            raise Unresolved(f"bad hex colour {text}")
        vals = [int(h[k:k + 2], 16) / 255 for k in range(0, len(h), 2)]
        return tuple(vals) if len(vals) == 4 else (*vals, 1.0)
    m = re.match(r"^([a-z-]+)\((.*)\)$", low, re.S)
    if not m:
        triplet = [p.strip() for p in low.split(",")]
        if len(triplet) == 3 and all(re.fullmatch(r"[\d.]+", p) for p in triplet):
            return (*(float(p) / 255 for p in triplet), 1.0)
        raise Unresolved(f"not a colour: {text}")
    fn, inner = m[1], m[2]
    if fn in ("rgb", "rgba"):
        parts, alpha = _args(inner)
        if parts and parts[0].startswith("#"):
            # rgba(#3dd7fb, .5): a hex value in an `-rgb` variable.
            base = parse_color(parts[0])
            a = _number(parts[1], 1) if len(parts) > 1 else (_number(alpha, 1) if alpha else 1)
            return (*base[:3], a)
        if len(parts) == 4 and alpha is None:
            alpha = parts.pop()
        if len(parts) != 3:
            raise Unresolved(f"bad rgb(): {text}")
        r, g, b = (_number(p, 255) / 255 for p in parts)
        a = _number(alpha, 1) if alpha else 1.0
        return (_clamp(r), _clamp(g), _clamp(b), _clamp(a))
    if fn in ("hsl", "hsla"):
        parts, alpha = _args(inner)
        if len(parts) == 4 and alpha is None:
            alpha = parts.pop()
        if len(parts) != 3:
            raise Unresolved(f"bad hsl(): {text}")
        h = _hue(parts[0]) % 360 / 360
        s = _clamp(_number(parts[1], 1) if parts[1].endswith("%") else float(parts[1]) / 100)
        l = _clamp(_number(parts[2], 1) if parts[2].endswith("%") else float(parts[2]) / 100)
        r, g, b = colorsys.hls_to_rgb(h, l, s)
        return (r, g, b, _clamp(_number(alpha, 1)) if alpha else 1.0)
    if fn == "color-mix":
        return _color_mix(inner, text)
    raise Unresolved(f"unsupported colour function {fn}()")


def _color_mix(inner: str, text: str) -> tuple[float, ...]:
    parts = [p.strip() for p in split_top(inner, ",")]
    if len(parts) != 3 or not parts[0].startswith("in "):
        raise Unresolved(f"bad color-mix(): {text}")
    space = parts[0][3:].split()[0]

    def stop(part):
        m = re.match(r"^(.*?)\s+([\d.]+)%$", part)
        return (parse_color(m[1]), float(m[2]) / 100) if m else (parse_color(part), None)

    (c1, p1), (c2, p2) = stop(parts[1]), stop(parts[2])
    if p1 is None and p2 is None:
        p1 = p2 = 0.5
    elif p1 is None:
        p1 = 1 - p2
    elif p2 is None:
        p2 = 1 - p1
    total = p1 + p2
    if total <= 0:
        raise Unresolved(f"color-mix() weights sum to 0: {text}")
    alpha_scale = min(total, 1.0)
    w = p2 / total
    a = c1[3] * (1 - w) + c2[3] * w
    if space in ("oklab", "oklch"):
        l1, l2 = _to_oklab(c1), _to_oklab(c2)
        lab = [(x1 * c1[3] * (1 - w) + x2 * c2[3] * w) / a if a else 0 for x1, x2 in zip(l1, l2)]
        rgb = _from_oklab(lab)
    else:
        if space not in ("srgb", "srgb-linear", "hsl", "lab", "lch", "xyz"):
            raise Unresolved(f"color-mix() space {space}")
        rgb = [(x1 * c1[3] * (1 - w) + x2 * c2[3] * w) / a if a else 0
               for x1, x2 in zip(c1[:3], c2[:3])]
    return (*(_clamp(x) for x in rgb), a * alpha_scale)


def _lin(v: float) -> float:
    return v / 12.92 if v <= 0.04045 else ((v + 0.055) / 1.055) ** 2.4


def _unlin(v: float) -> float:
    return 12.92 * v if v <= 0.0031308 else 1.055 * v ** (1 / 2.4) - 0.055


def _to_oklab(c):
    r, g, b = (_lin(x) for x in c[:3])
    l_ = (0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b) ** (1 / 3)
    m_ = (0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b) ** (1 / 3)
    s_ = (0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b) ** (1 / 3)
    return (0.2104542553 * l_ + 0.7936177850 * m_ - 0.0040720468 * s_,
            1.9779984951 * l_ - 2.4285922050 * m_ + 0.4505937099 * s_,
            0.0259040371 * l_ + 0.7827717662 * m_ - 0.8086757660 * s_)


def _from_oklab(lab):
    L, A, B = lab
    l_ = (L + 0.3963377774 * A + 0.2158037573 * B) ** 3
    m_ = (L - 0.1055613458 * A - 0.0638541728 * B) ** 3
    s_ = (L - 0.0894841775 * A - 1.2914855480 * B) ** 3
    r = 4.0767416621 * l_ - 3.3077115913 * m_ + 0.2309699292 * s_
    g = -1.2684380046 * l_ + 2.6097574011 * m_ - 0.3413193965 * s_
    b = -0.0041960863 * l_ - 0.7034186147 * m_ + 1.7076147010 * s_
    return [_unlin(_clamp(x)) for x in (r, g, b)]


def _clamp(v: float) -> float:
    return max(0.0, min(1.0, v))


def to_hex(c) -> str:
    byte = [round(_clamp(x) * 255) for x in c]
    if byte[3] >= 255:
        return "#{:02x}{:02x}{:02x}".format(*byte[:3])
    return "#{:02x}{:02x}{:02x}{:02x}".format(*byte)


def quantize(c):
    """The colour as the theme file stores it, so checks see what ships."""
    return parse_color(to_hex(c))


def composite(c, bg):
    a = c[3]
    return (*(x * a + y * (1 - a) for x, y in zip(c[:3], bg[:3])), 1.0)


def mix(a, b, t):
    """`t` of the way from `a` to `b`."""
    return tuple(x + (y - x) * t for x, y in zip(a, b))


def luminance(c) -> float:
    r, g, b = (_lin(x) for x in c[:3])
    return 0.2126 * r + 0.7152 * g + 0.0722 * b


def contrast(a, b) -> float:
    la, lb = luminance(a), luminance(b)
    return (max(la, lb) + 0.05) / (min(la, lb) + 0.05)


def hsl(c):
    h, l, s = colorsys.rgb_to_hls(*c[:3])
    return h, s, l


def from_hsl(h, s, l, a=1.0):
    return (*colorsys.hls_to_rgb(h % 1.0, _clamp(l), _clamp(s)), a)


def hue_gap(a, b) -> float:
    gap = abs(hsl(a)[0] - hsl(b)[0]) * 360
    return min(gap, 360 - gap)


def same(a, b, tol=2 / 255) -> bool:
    return all(abs(x - y) <= tol for x, y in zip(a, b))


# ---------------------------------------------------------------------------
# Conversion


@dataclass
class Report:
    name: str
    dialect: str = ""
    dialect_counts: dict = field(default_factory=dict)
    derived_mode: str | None = None
    sources: dict = field(default_factory=lambda: {m: {} for m in MODES})
    notes: list[str] = field(default_factory=list)
    adjustments: list[tuple[str, str, str, str, str]] = field(default_factory=list)
    contrast: list[tuple[str, str, float, float, bool]] = field(default_factory=list)
    decorations: list[str] = field(default_factory=list)
    unresolved: list[str] = field(default_factory=list)
    not_carried: dict[str, list[str]] = field(default_factory=dict)
    unread_vars: list[str] = field(default_factory=list)
    picks: dict[str, str] = field(default_factory=dict)

    def skip(self, reason: str, item: str) -> None:
        self.not_carried.setdefault(reason, []).append(item)


def detect_dialect(variables: Variables) -> tuple[str, dict]:
    names = variables.theme_names()
    pre = sorted(names & PRE_1_0_VARS)
    new = sorted(names & V1_VARS)
    counts = {"pre-1.0": pre, "1.0+": new}
    if pre and not new:
        return "pre-1.0", counts
    if new and not pre:
        return "1.0+", counts
    if not pre and not new:
        return "1.0+", counts
    return ("pre-1.0" if len(pre) > len(new) else "1.0+"), counts


# Tokens where Obsidian's own default is what the theme shows in Obsidian and
# still fits any palette (status hues, callouts, highlight).
DEFAULT_OK = {"success", "warning", "danger", "info", "calloutQuestion",
              "calloutImportant", "highlight", "onAccent"}


def _uses_theme(trail: list[str]) -> bool:
    return any(t.startswith("--") and not t.endswith("(Obsidian default)") for t in trail)


def map_mode(variables: Variables, mode: str, dialect: str, report: Report) -> dict:
    """Theme tokens for one mode, straight from the theme's variables."""
    resolver = Resolver(variables, mode)
    table = {**SOURCES, **DIALECT_SOURCES[dialect]}
    raw, source = {}, {}
    for key, names in table.items():
        value, label = None, None
        for theme_only in (True, False):
            for name in names:
                hit = variables.lookup(mode, name)
                if hit is None or (theme_only and not hit[1]):
                    continue
                trail: list[str] = []
                try:
                    candidate = resolver.color(f"var({name})", trail)
                except Unresolved as err:
                    report.unresolved.append(f"{mode} {key} from {name}: {err}")
                    continue
                if not theme_only and key not in DEFAULT_OK and not _uses_theme(trail):
                    # Obsidian's neutral greys and purple accent are not this
                    # theme's colours; derive the token from the theme instead.
                    continue
                value, label = candidate, " → ".join(trail)
                break
            if value is not None:
                break
        if value is not None:
            raw[key], source[key] = value, label
    tokens = dict(raw)
    fallback_bg = (1, 1, 1, 1) if mode == "light" else (0, 0, 0, 1)
    canvas = composite(tokens.get("canvas", fallback_bg), fallback_bg)
    tokens["canvas"] = canvas
    for key in list(tokens):
        if key not in TRANSLUCENT and tokens[key][3] < 1:
            tokens[key] = composite(tokens[key], canvas)
            source[key] = f"{source[key]} (composited over canvas)"
    if "text" not in tokens:
        tokens["text"] = (0.13, 0.13, 0.13, 1) if mode == "light" else (0.85, 0.85, 0.85, 1)
        source["text"] = "default"
    report.sources[mode] = source
    return tokens


def complete(tokens: dict, source: dict, mode: str) -> None:
    """Derive every token the theme did not set from the ones it did."""
    canvas, text = tokens["canvas"], tokens["text"]

    def derive(key, value, how):
        if key not in tokens:
            tokens[key] = value
            source[key] = f"derived: {how}"

    derive("surface", canvas, "canvas")
    derive("surfaceRaised", mix(canvas, text, 0.05), "canvas 5% towards text")
    derive("sidebar", tokens["surfaceRaised"], "surfaceRaised")
    derive("textMuted", mix(text, canvas, 0.25), "text 25% towards canvas")
    derive("textFaint", mix(text, canvas, 0.45), "text 45% towards canvas")
    derive("accent", (0.35, 0.45, 0.85, 1), "fallback blue")
    derive("border", mix(canvas, text, 0.2), "canvas 20% towards text")
    if contrast(tokens["border"], canvas) < 1.25:
        tokens["border"] = mix(canvas, mix(tokens["border"], text, 0.5), 0.3)
        source["border"] += "; too faint to see, mixed 15% towards text"
    derive("borderSubtle", mix(tokens["border"], canvas, 0.5), "border halfway to canvas")
    derive("selected", mix(canvas, tokens["accent"], 0.2), "canvas 20% towards accent")
    derive("focus", tokens["accent"], "accent")
    derive("link", tokens["accent"], "accent")
    # Obsidian headings inherit the text colour unless the theme says otherwise;
    # settle_heading then gives them a colour.
    derive("heading", text, "text (Obsidian headings inherit it)")
    derive("tile", mix(canvas, tokens["accent"], 0.1), "canvas 10% towards accent")
    derive("notice", tokens["tile"], "tile")
    derive("hover", mix(canvas, text, 0.07), "canvas 7% towards text")
    derive("codeBg", tokens["surfaceRaised"], "surfaceRaised")
    derive("codeBorder", mix(tokens["codeBg"], text, 0.1), "codeBg 10% towards text")
    derive("success", (0.2, 0.6, 0.3, 1), "fallback green")
    derive("warning", (0.8, 0.5, 0.1, 1), "fallback orange")
    derive("danger", (0.8, 0.2, 0.25, 1), "fallback red")
    derive("info", tokens["accent"], "accent")
    derive("missingLink", tokens["warning"], "warning")
    derive("calloutQuestion", tokens["warning"], "warning")
    derive("calloutImportant", tokens["accent"], "accent")
    derive("warningTint", mix(canvas, tokens["warning"], 0.12), "canvas 12% towards warning")
    derive("highlight", (*tokens["warning"][:3], 0.3), "warning at 30%")
    derive("scrim", (*text[:3], 0.2) if mode == "light" else (0, 0, 0, 0.6), "text/black, translucent")
    highlight = composite(tokens["highlight"], canvas)
    derive("findMatch", highlight, "highlight over canvas")
    derive("findCurrent", mix(highlight, tokens["accent"], 0.35), "find match 35% towards accent")
    derive("findMarkTrack", tokens["sidebar"], "sidebar")
    derive("findMark", tokens["warning"], "warning")
    derive("findMarkCurrent", tokens["accent"], "accent")
    derive("onAccent", (1, 1, 1, 1), "white")
    derive("onStatus", canvas, "canvas")
    for n in range(1, 7):
        derive(f"heading{n}", tokens["heading"], "heading")


BACKGROUND_KEYS = {
    "canvas", "surface", "surfaceRaised", "sidebar", "border", "borderSubtle",
    "selected", "tile", "notice", "warningTint", "hover", "codeBg", "codeBorder",
    "findMatch", "findCurrent", "findMarkTrack",
}
TEXT_KEYS = {"text", "textMuted", "textFaint"}


def match_contrast(color, bg, ratio: float):
    """Same hue and saturation, lightness chosen for `ratio` against `bg`."""
    h, s, _l = hsl(color)
    darker = luminance(bg) > 0.18
    best = None
    for k in range(401):
        l = k / 400 if not darker else 1 - k / 400
        cand = from_hsl(h, s, l, color[3])
        if contrast(quantize(cand), bg) >= ratio:
            best = cand
            break
    return best or ((0, 0, 0, color[3]) if darker else (1, 1, 1, color[3]))


@dataclass
class Mirror:
    """Maps a colour from the native mode into the derived one."""
    native: dict
    native_mode: str

    def __post_init__(self):
        self.ch, self.cs, self.cl = hsl(self.native["canvas"])
        self.target = 0.975 if self.native_mode == "dark" else 0.09
        self.canvas = from_hsl(self.ch, min(self.cs, 0.35), self.target)

    def surface(self, c):
        h, s, l = hsl(c)
        return from_hsl(h, min(s, 0.35), self.target - (l - self.cl), c[3])

    def ink(self, c, low: float, high: float, max_s: float = 1.0):
        """A foreground keeps its contrast to the canvas, within [low, high]."""
        ratio = contrast(composite(c, self.native["canvas"]), self.native["canvas"])
        h, s, l = hsl(c)
        return match_contrast(from_hsl(h, min(s, max_s), l, c[3]), self.canvas,
                              min(max(ratio, low), high))


def derive_other_mode(native: dict, native_mode: str) -> dict:
    """The missing mode: surfaces mirror in lightness, inks keep their contrast."""
    mirror = Mirror(native, native_mode)
    out = {}
    for key, c in native.items():
        if key in BACKGROUND_KEYS:
            out[key] = mirror.surface(c)
        elif key in TEXT_KEYS:
            out[key] = mirror.ink(c, 3.2, 15.0, max_s=0.3)
        elif key == "scrim":
            out[key] = ((*native["text"][:3], 0.2) if native_mode == "light"
                        else (*from_hsl(mirror.ch, mirror.cs, 0.15)[:3], 0.2))
        elif key == "highlight":
            out[key] = c
        elif key in ("onAccent", "onStatus"):
            out[key] = (1, 1, 1, 1) if native_mode == "dark" else mirror.canvas
        else:
            out[key] = mirror.ink(c, 4.6, 5.5)
    return out


def _nudge(color, against: list, minimum: float, prefer_dark: bool | None = None):
    """Move lightness away from the backgrounds until every ratio holds."""
    if all(contrast(quantize(color), bg) >= minimum + 0.01 for bg in against):
        return color
    h, s, l = hsl(color)
    bg_l = sum(luminance(bg) for bg in against) / len(against)
    order = [prefer_dark] if prefer_dark is not None else []
    order += [bg_l > 0.18, bg_l <= 0.18]
    for dark in order:
        step = -0.005 if dark else 0.005
        nl = l
        for _ in range(220):
            nl += step
            if nl < 0 or nl > 1:
                break
            cand = from_hsl(h, s, nl, color[3])
            if all(contrast(quantize(cand), bg) >= minimum + 0.01 for bg in against):
                return cand
    return (0, 0, 0, 1) if bg_l > 0.18 else (1, 1, 1, 1)


def enforce(tokens: dict, mode: str, report: Report, reading: float, source: dict) -> None:
    """Apply the gates of brand.rs `every_theme_meets_wcag_contrast_*`."""

    def fix(key, against_keys, minimum, why):
        before = tokens[key]
        after = _nudge(before, [quantize(tokens[k]) for k in against_keys], minimum)
        if not same(before, after, 0):
            tokens[key] = after
            report.adjustments.append((mode, key, to_hex(before), to_hex(after), why))

    def fix_background(key, minimum):
        """`text on X`: move the background X towards the canvas side."""
        text = quantize(tokens["text"])
        before = tokens[key]
        if contrast(quantize(before), text) >= minimum + 0.01:
            return
        after = _nudge(before, [text], minimum, prefer_dark=luminance(text) > 0.18)
        tokens[key] = after
        report.adjustments.append((mode, key, to_hex(before), to_hex(after),
                                   f"text on {key} below {minimum}:1"))

    fix("text", ["surface", "canvas", "sidebar"], reading, f"text below {reading}:1")
    fix("textMuted", ["surface", "canvas", "sidebar"], reading, f"muted text below {reading}:1")
    for key in ("codeBg", "surfaceRaised", "selected", "hover", "warningTint"):
        fix_background(key, reading)
    for key in ["link", "heading", "missingLink", *HEADING_LEVELS]:
        fix(key, ["surface", "canvas"], reading, f"{key} on surface/canvas below {reading}:1")
    fix("textFaint", ["surface"], 4.5 if reading > 4.5 else 3.0, "faint text below 3:1")
    for key in ("success", "warning", "danger", "info"):
        fix(key, ["surface"], 4.5, f"{key} on surface below 4.5:1")
    # on-accent / on-status: the best of the theme's own extremes and white/black.
    for on, fills in (("onAccent", ["accent"]), ("onStatus", ["success", "warning", "danger", "info"])):
        cands = [tokens[on], tokens["canvas"], (1, 1, 1, 1), (0, 0, 0, 1), tokens["text"]]

        def low(c):
            return min(contrast(quantize(c), quantize(tokens[f])) for f in fills)
        # The first candidate that passes, in the theme's own order of preference.
        best = next((c for c in cands if low(c) >= 4.51), max(cands, key=low))
        worst = low(best)
        if not same(best, tokens[on], 0):
            ratio = min(contrast(quantize(tokens[on]), quantize(tokens[f])) for f in fills)
            if ratio < 4.51:
                report.adjustments.append((mode, on, to_hex(tokens[on]), to_hex(best),
                                           f"{on} on {', '.join(fills)} below 4.5:1"))
                tokens[on] = best
        if worst < 4.51:
            for f in fills:
                before = tokens[f]
                after = _nudge(before, [quantize(tokens[on])], 4.5)
                if not same(before, after, 0):
                    tokens[f] = after
                    report.adjustments.append((mode, f, to_hex(before), to_hex(after),
                                               f"{on} on {f} below 4.5:1"))
            for f in fills:
                if f in ("success", "warning", "danger", "info") and contrast(quantize(tokens[f]), quantize(tokens["surface"])) < 4.5:
                    report.notes.append(f"{mode}: {f} cannot meet 4.5:1 on both surface and {on}; check by hand")
    for key, find in (("findMark", "findMatch"), ("findMarkCurrent", "findCurrent")):
        fix(key, ["findMarkTrack", "canvas"], 3.0, f"{key} below 3:1 on the scrollbar track")
        # brand.rs code2810 also requires a mark to differ from its find colour.
        if to_hex(tokens[key]) == to_hex(tokens[find]):
            before = tokens[key]
            against = [quantize(tokens["findMarkTrack"]), quantize(tokens["canvas"])]
            h, s, l = hsl(before)
            moved = None
            for k in range(1, 201):
                for sign in (1, -1):
                    cand = from_hsl(h, s, l + sign * k * 0.005, before[3])
                    if (to_hex(cand) != to_hex(tokens[find])
                            and all(contrast(quantize(cand), bg) >= 3.01 for bg in against)):
                        moved = cand
                        break
                if moved:
                    break
            if moved:
                tokens[key] = moved
                report.adjustments.append((mode, key, to_hex(before), to_hex(moved),
                                           f"{key} was the same colour as {find}"))
    if "focus" in source and source["focus"].startswith("derived"):
        tokens["focus"] = tokens["accent"]


def contrast_rows(tokens: dict, reading: float) -> list[tuple[str, float, float]]:
    q = {k: quantize(v) for k, v in tokens.items()}
    rows = []
    for name in ("surface", "canvas", "sidebar"):
        rows.append((f"text on {name}", contrast(q["text"], q[name]), reading))
        rows.append((f"textMuted on {name}", contrast(q["textMuted"], q[name]), reading))
    for name in ("surface", "canvas"):
        for fg in ["link", "heading", "missingLink", *HEADING_LEVELS]:
            rows.append((f"{fg} on {name}", contrast(q[fg], q[name]), reading))
    for name in ("codeBg", "surfaceRaised", "selected", "hover", "warningTint"):
        rows.append((f"text on {name}", contrast(q["text"], q[name]), reading))
    rows.append(("heading hue from link (degrees)", hue_gap(q["heading"], q["link"]), 25.0))
    rows.append(("heading saturation (x100)", hsl(q["heading"])[1] * 100 if not same(q["heading"], q["text"], 0) else 0, 15.0))
    rows.append(("onAccent on accent", contrast(q["onAccent"], q["accent"]), 4.5))
    rows.append(("textFaint on surface", contrast(q["textFaint"], q["surface"]), 4.5 if reading > 4.5 else 3.0))
    for s in ("success", "warning", "danger", "info"):
        rows.append((f"{s} on surface", contrast(q[s], q["surface"]), 4.5))
        rows.append((f"onStatus on {s}", contrast(q["onStatus"], q[s]), 4.5))
    for m in ("findMark", "findMarkCurrent"):
        rows.append((f"{m} on findMarkTrack", contrast(q[m], q["findMarkTrack"]), 3.0))
        rows.append((f"{m} on canvas", contrast(q[m], q["canvas"]), 3.0))
    for m, f in (("findMark", "findMatch"), ("findMarkCurrent", "findCurrent")):
        rows.append((f"{m} differs from {f} (1 = yes)",
                     float(to_hex(tokens[m]) != to_hex(tokens[f])), 1.0))
    return rows


PALETTE_NAMES = [f"--color-{c}" for c in
                 ("red", "orange", "yellow", "green", "cyan", "blue", "purple", "pink")]


def theme_palette(variables: Variables, mode: str, canvas) -> list[tuple[str, tuple]]:
    """Every saturated, visible colour the theme defines (plus Obsidian's hues)."""
    resolver = Resolver(variables, mode)
    names = sorted(variables.theme[mode]) + [n for n in PALETTE_NAMES if n not in variables.theme[mode]]
    out = []
    for name in names:
        try:
            c = composite(resolver.color(f"var({name})"), canvas)
        except (Unresolved, ValueError, ZeroDivisionError):
            continue
        if hsl(c)[1] >= 0.25 and contrast(c, canvas) >= 3.0:
            out.append((name, c))
    return out


def _closest(palette, near, away, prefer=None, gap=25.0):
    """The palette colour nearest in hue to `near` and `gap`° or more from `away`.

    `prefer` (the variable the other mode picked) wins when it fits, so both
    modes take their heading or link from the same theme colour.
    """
    fits = [(hue_gap(c, near), name, c) for name, c in palette if hue_gap(c, away) >= gap]
    for fit in fits:
        if fit[1] == prefer:
            return fit
    return min(fits, default=None)


def settle_heading(tokens: dict, mode: str, report: Report, variables: Variables, source: dict) -> None:
    """#3147: a coloured heading at least 25° in hue from links.

    A theme whose H1 is plain text keeps its links and gets a heading colour from
    its own palette; a theme whose coloured H1 sits too close to its links keeps
    the heading and moves the links.
    """
    text, link = tokens["text"], tokens["link"]
    palette = theme_palette(variables, mode, tokens["canvas"])
    if same(tokens["heading"], text) or hsl(tokens["heading"])[1] < 0.15:
        before = tokens["heading"]
        if hue_gap(tokens["accent"], link) >= 25 and hsl(tokens["accent"])[1] >= 0.15:
            tokens["heading"], why = tokens["accent"], "the accent"
        else:
            pick = _closest(palette, tokens["accent"], link, report.picks.get("heading"))
            if pick:
                report.picks["heading"] = pick[1]
                tokens["heading"], why = pick[2], f"theme colour {pick[1]}"
            else:
                h, s, l = hsl(tokens["accent"])
                tokens["heading"] = from_hsl(h + 40 / 360, max(s, 0.4), l)
                why = "the accent turned 40°"
        report.notes.append(
            f"{mode}: the theme's H1 colour {to_hex(before)} is plain text; Okilum headings are "
            f"coloured (#3147), so `heading` is {why}")
        source["heading"] = f"{source.get('heading', '')}; uncoloured, now {why}"
    for key in HEADING_LEVELS:
        if same(tokens[key], text) or hsl(tokens[key])[1] < 0.15:
            tokens[key] = tokens["heading"]
            source[key] = f"{source.get(key, '')}; uncoloured, now heading"
    if same(tokens["missingLink"], link) or hue_gap(tokens["missingLink"], tokens["heading"]) < 10:
        tokens["missingLink"] = tokens["warning"]
        source["missingLink"] = f"{source.get('missingLink', '')}; looked like a link or heading, now warning"
    if hue_gap(tokens["heading"], link) >= 25:
        return
    pick = _closest(palette, link, tokens["heading"], report.picks.get("link"))
    if pick:
        report.picks["link"] = pick[1]
        tokens["link"], why = pick[2], f"theme colour {pick[1]}"
    else:
        h, s, l = hsl(link)
        sign = 1 if (h - hsl(tokens["heading"])[0]) % 1 < 0.5 else -1
        tokens["link"] = from_hsl(hsl(tokens["heading"])[0] + sign * 30 / 360, max(s, 0.4), l)
        why = "hue turned 30° away from the heading"
    source["link"] = f"{source.get('link', '')}; too close to heading hue, {why}"
    report.adjustments.append((mode, "link", to_hex(link), to_hex(tokens["link"]),
                               f"link hue {hue_gap(link, tokens['heading']):.0f}° from heading (< 25°): {why}"))


# ---------------------------------------------------------------------------
# Decorations and syntax


def parse_gradient(value: str, resolver: Resolver):
    """(angle in degrees, [(colour, position or None)]) for a linear-gradient."""
    m = re.search(r"(?:-webkit-)?linear-gradient\(", value)
    if not m:
        kind = re.search(r"[\w-]*gradient", value)
        raise Unresolved(f"{kind[0] if kind else 'gradient'} is not a linear-gradient; "
                         "the slots take linear gradients only")
    close = _matching_paren(value, m.end() - 1)
    inner = resolver.substitute(value[m.end():close])
    parts = [p.strip() for p in split_top(inner, ",")]
    angle = 180.0
    first = parts[0].lower()
    directions = {"to top": 0, "to right": 90, "to bottom": 180, "to left": 270,
                  "to top right": 45, "to right top": 45, "to bottom right": 135,
                  "to right bottom": 135, "to bottom left": 225, "to left bottom": 225,
                  "to top left": 315, "to left top": 315}
    if first in directions:
        angle, parts = directions[first], parts[1:]
    elif re.fullmatch(r"-?[\d.]+(deg|turn|rad)", first):
        angle, parts = _hue(first), parts[1:]
    stops = []
    for part in parts:
        pos = None
        m2 = re.match(r"^(.*?)\s+(-?[\d.]+)(%|px)?(?:\s+-?[\d.]+(?:%|px)?)?$", part)
        color_text = part
        if m2 and not re.match(r"^[\d.]+$", m2[1]):
            color_text = m2[1]
            pos = float(m2[2]) / 100 if m2[3] == "%" else None
        stops.append((parse_color(color_text), pos))
    if len(stops) < 2:
        raise Unresolved(f"linear-gradient with {len(stops)} colour stop(s)")
    return angle, stops


def two_stops(stops, report_line: list[str]):
    """Reduce stops to the slot's two: first colour and the last distinct one."""
    colors = [c for c, _ in stops]
    if len(colors) <= 2:
        return colors[0], colors[-1], None
    last = colors[-1]
    first_last = next(k for k, c in enumerate(colors) if same(c, last))
    report_line.append(f"{len(colors)} stops reduced to two (the slots take two stops)")
    at = first_last / (len(colors) - 1)
    return colors[0], last, round(at, 3)


def _selector_has(selector: str, pattern: str) -> bool:
    return re.search(pattern, selector) is not None


HEADING_RE = r"(?:^|[\s>,])h([1-6])(?![\w-])|HyperMD-header-([1-6])|cm-header-([1-6])"


def find_decorations(sheet: Sheet, variables: Variables, mode: str,
                     tokens: dict) -> tuple[dict, dict, set, list[str]]:
    """Decoration slots, syntax palette, consumed (rule, prop) ids, notes."""
    resolver = Resolver(variables, mode)
    deco, syntax, used, notes = {}, {}, set(), []

    def applies(rule, selector):
        sm = selector_mode(selector)
        return rule.condition is None and (sm is None or sm == mode)

    def opaque(c):
        return composite(c, tokens["canvas"])

    for idx, rule in enumerate(sheet.rules):
        props = {p: v for p, v, _ in rule.decls}
        for selector in rule.selectors:
            if not applies(rule, selector):
                continue
            levels = {int(next(g for g in m.groups() if g)) for m in re.finditer(HEADING_RE, selector)}
            # Heading rule: a gradient border image under H2–H6.
            image = props.get("border-image-source") or props.get("border-image")
            if levels and image and "gradient" in image:
                try:
                    angle, stops = parse_gradient(image, resolver)
                    lines: list[str] = []
                    a, b, at = two_stops(stops, lines)
                    slot = deco.setdefault("headingRule", {"levels": []})
                    slot["levels"] = sorted(set(slot["levels"]) | levels)
                    slot.update({"from": to_hex(opaque(a)), "to": to_hex(opaque(b))})
                    if at is not None:
                        slot["toAt"] = at
                    for p in ("border-image-source", "border-image", "border-image-slice",
                              "border-bottom", "border-width"):
                        used.add((idx, p))
                    notes.extend(f"headingRule (L{rule.line}): {line}" for line in lines)
                except Unresolved as err:
                    notes.append(f"headingRule (L{rule.line}): {err}")
            # hr: transparent → accent → transparent.
            if _selector_has(selector, r"(?:^|[\s>])hr(?![\w-])(?!::)") and image and "gradient" in image:
                try:
                    _angle, stops = parse_gradient(image, resolver)
                    colors = [c for c, _ in stops]
                    middle = max(colors, key=lambda c: c[3])
                    edge = colors[0]
                    deco["hr"] = {"edge": to_hex(edge if edge[3] < 1 else opaque(edge)),
                                  "middle": to_hex(opaque(middle))}
                    for p in ("border-image-source", "border-image", "border-image-slice",
                              "border-bottom", "border-width", "border", "height"):
                        used.add((idx, p))
                except Unresolved as err:
                    notes.append(f"hr (L{rule.line}): {err}")
            # Blockquote: gradient background, bars from ::before/::after, left border.
            if _selector_has(selector, r"blockquote|HyperMD-quote(?![\w-])"):
                pseudo = re.search(r"::?(before|after)\s*$", selector)
                bg = props.get("background") or props.get("background-image")
                try:
                    if bg and "gradient" in bg:
                        angle, stops = parse_gradient(bg, resolver)
                        lines = []
                        a, b, _at = two_stops(stops, lines)
                        slot = deco.setdefault("blockquote", {})
                        if pseudo:
                            bar = slot.setdefault("bars", {})
                            width = props.get("width", "")
                            bar[pseudo[1] == "before" and "top" or "bottom"] = {
                                "from": to_hex(a), "to": to_hex(opaque(b)),
                                **({"width": float(width[:-1]) / 100} if width.endswith("%") else {}),
                            }
                            for p in ("background", "background-image", "width", "height",
                                      "content", "position", "top", "bottom", "left"):
                                used.add((idx, p))
                        else:
                            slot.update({"angle": angle, "from": to_hex(a), "to": to_hex(opaque(b))})
                            used.add((idx, "background"))
                            used.add((idx, "background-image"))
                    border = props.get("border-left") or props.get("border-left-color")
                    if border and not pseudo:
                        color = _color_in(border, resolver)
                        if color:
                            deco.setdefault("blockquote", {})["border"] = to_hex(opaque(color))
                            used.add((idx, "border-left"))
                            used.add((idx, "border-left-color"))
                except Unresolved as err:
                    notes.append(f"blockquote (L{rule.line}): {err}")
            # Link marker: a solid gradient band under internal/external links.
            kind = ("internal" if "internal-link" in selector or "hmd-internal-link" in selector
                    else "external" if ("external-link" in selector or "cm-link" in selector) else None)
            bg = props.get("background-image") or props.get("background")
            if kind and bg and "gradient" in bg and ":hover" not in selector:
                try:
                    _angle, stops = parse_gradient(bg, resolver)
                    slot = deco.setdefault("linkMarker", {})
                    slot[kind] = to_hex(stops[0][0])
                    used.add((idx, "background-image"))
                    used.add((idx, "background"))
                except Unresolved as err:
                    notes.append(f"linkMarker (L{rule.line}): {err}")
            if kind and ":hover" not in selector and "background-size" in props:
                size = props["background-size"].split()
                if len(size) == 2 and size[1].endswith("px"):
                    deco.setdefault("linkMarker", {})["heightPx"] = float(size[1][:-2])
                    used.add((idx, "background-size"))
                    used.add((idx, "background-position"))
                    used.add((idx, "background-repeat"))
            # Bold text gradient: background-clip: text with a gradient.
            clip = props.get("-webkit-background-clip") or props.get("background-clip")
            if (_selector_has(selector, r"(?:^|[\s>])strong(?![\w-])|cm-strong(?![\w-])")
                    and clip == "text" and not re.search(r"::?selection", selector)):
                bg = props.get("background-image") or props.get("background")
                if bg and "gradient" in bg:
                    try:
                        angle, stops = parse_gradient(bg, resolver)
                        lines = []
                        a, b, _at = two_stops(stops, lines)
                        deco["boldGradient"] = {"angle": angle, "from": to_hex(opaque(a)),
                                                "to": to_hex(opaque(b))}
                        for p in ("-webkit-background-clip", "background-clip", "background-image",
                                  "background", "-webkit-text-fill-color", "background-color", "color"):
                            used.add((idx, p))
                    except Unresolved as err:
                        notes.append(f"boldGradient (L{rule.line}): {err}")
            # Syntax palette.
            if "color" in props:
                for slot, classes in SYNTAX_CLASSES.items():
                    for cls in classes:
                        if re.search(r"\." + re.escape(cls).replace(r"\.", r"\.") + r"(?![\w-])", selector):
                            try:
                                syntax[slot] = to_hex(opaque(resolver.color(props["color"])))
                                used.add((idx, "color"))
                            except Unresolved as err:
                                notes.append(f"syntax {slot} (L{rule.line}): {err}")
    return deco, syntax, used, notes


def app_syntax(syntax: dict) -> dict:
    """The palette in the app's `syntax` schema (#3156): tree-sitter capture
    names only. The toolkit palette has no `builtin` capture; builtins are
    functions there, so `builtin` fills `function` when the theme left it out."""
    out = {k: v for k, v in syntax.items() if k != "builtin"}
    if "builtin" in syntax:
        out.setdefault("function", syntax["builtin"])
    return out


def _color_in(value: str, resolver: Resolver):
    text = resolver.substitute(value)
    for part in split_top(text, " "):
        part = part.strip()
        if not part or re.fullmatch(r"-?[\d.]+(px|em|rem|%)?|solid|dashed|dotted|none|double", part):
            continue
        try:
            return parse_color(part)
        except Unresolved:
            continue
    return None


def recolor_for_derived(value, mirror: Mirror, derived_tokens: dict):
    """A decoration colour in the derived mode follows the token it matches."""
    if isinstance(value, dict):
        return {k: recolor_for_derived(v, mirror, derived_tokens) for k, v in value.items()}
    if not (isinstance(value, str) and value.startswith("#")):
        return value
    c = parse_color(value)
    if c[3] == 0:
        return value
    native = mirror.native
    best = min(native, key=lambda k: sum(abs(x - y) for x, y in zip(native[k][:3], c[:3])))
    if sum(abs(x - y) for x, y in zip(native[best][:3], c[:3])) < 0.06:
        return to_hex((*derived_tokens[best][:3], c[3]))
    _h, s, l = hsl(c)
    if s < 0.35 and (l < 0.25 or l > 0.8):
        return to_hex(mirror.surface(c))
    return to_hex(mirror.ink(c, 3.0, 5.5))


# ---------------------------------------------------------------------------
# Driver


def convert(css: str, *, theme_id: str, name: str, credit: str = "",
            high_contrast: bool = False) -> tuple[dict, Report]:
    sheet = parse_css(css)
    variables = collect_variables(sheet)
    report = Report(name)
    report.dialect, report.dialect_counts = detect_dialect(variables)
    reading = 7.0 if high_contrast else 4.5

    declared = variables.declared_in_mode
    if declared["light"] and declared["dark"]:
        native = list(MODES)
    elif declared["dark"] or declared["light"]:
        native = ["dark" if declared["dark"] else "light"]
    else:
        # Only :root/body variables: whichever mode they look like is native.
        probe = map_mode(variables, "dark", report.dialect, Report(name))
        native = ["dark" if luminance(probe["canvas"]) < 0.2 else "light"]
    modes = {}
    for mode in native:
        tokens = map_mode(variables, mode, report.dialect, report)
        complete(tokens, report.sources[mode], mode)
        settle_heading(tokens, mode, report, variables, report.sources[mode])
        enforce(tokens, mode, report, reading, report.sources[mode])
        modes[mode] = tokens
    mirror = None
    if len(native) == 1:
        other = "light" if native[0] == "dark" else "dark"
        report.derived_mode = other
        mirror = Mirror(modes[native[0]], native[0])
        modes[other] = derive_other_mode(modes[native[0]], native[0])
        report.sources[other] = {k: f"derived from {native[0]}" for k in modes[other]}
        report.notes.append(f"the theme ships no {other} mode; it is derived from {native[0]} "
                            "(surfaces mirrored in lightness, text and accents keep hue and contrast)")
        settle_heading(modes[other], other, report, variables, report.sources[other])
        enforce(modes[other], other, report, reading, report.sources[other])
    for mode in MODES:
        for row in contrast_rows(modes[mode], reading):
            report.contrast.append((mode, row[0], row[1], row[2], row[1] >= row[2]))

    # Decorations and syntax.
    deco_by_mode, syntax_by_mode, used = {}, {}, set()
    for mode in MODES:
        if mode == report.derived_mode:
            continue
        deco, syntax, u, notes = find_decorations(sheet, variables, mode, modes[mode])
        deco_by_mode[mode], syntax_by_mode[mode] = deco, syntax
        used |= u
        report.decorations.extend(f"{mode}: {n}" for n in dict.fromkeys(notes))
    if report.derived_mode:
        src = native[0]
        deco_by_mode[report.derived_mode] = recolor_for_derived(deco_by_mode[src], mirror, modes[report.derived_mode])
        syntax = {}
        for k, v in syntax_by_mode[src].items():
            c = parse_color(v)
            syntax[k] = to_hex(_nudge(c, [quantize(modes[report.derived_mode]["codeBg"])], reading))
        syntax_by_mode[report.derived_mode] = syntax
    for mode in MODES:
        code = quantize(modes[mode]["codeBg"])
        for k, v in list(syntax_by_mode[mode].items()):
            c = parse_color(v)
            if contrast(c, code) < reading + 0.01:
                fixed = to_hex(_nudge(c, [code], reading))
                report.adjustments.append((mode, f"syntax.{k}", v, fixed, f"syntax {k} on codeBg below {reading}:1"))
                syntax_by_mode[mode][k] = fixed

    # Gradient bold text is reading text: both ends meet the reading gate.
    for mode in MODES:
        bold = deco_by_mode.get(mode, {}).get("boldGradient")
        if not bold:
            continue
        canvas = quantize(modes[mode]["canvas"])
        for end in ("from", "to"):
            c = parse_color(bold[end])
            if contrast(c, canvas) < reading + 0.01:
                fixed = to_hex(_nudge(c, [canvas], reading))
                report.adjustments.append((mode, f"boldGradient.{end}", bold[end], fixed,
                                           f"bold text on canvas below {reading}:1"))
                bold[end] = fixed

    # Everything not carried over.
    mapped_vars = set()
    for mode in MODES:
        for label in report.sources[mode].values():
            mapped_vars |= set(re.findall(r"--[\w-]+", label))
    for at in sheet.at_rules:
        reason = {"font-face": "@font-face (fonts are out of scope)",
                  "keyframes": "@keyframes (animations come from slots, #3155)",
                  "-webkit-keyframes": "@keyframes (animations come from slots, #3155)",
                  "import": "@import (offline tool, nothing fetched)"}.get(at.kind, f"@{at.kind}")
        report.skip(reason, f"L{at.line} {at.prelude[:120]}")
    for selector, count, line in variables.variants:
        report.skip("variable blocks for Style Settings variants or nested scopes",
                    f"L{line} {selector} ({count} variables)")
    for selector, condition, count, line in variables.conditional:
        report.skip("variable blocks inside @media/@supports", f"L{line} {condition} {selector} ({count} variables)")
    for idx, rule in enumerate(sheet.rules):
        if rule.nested:
            report.skip("nested CSS rules", f"L{rule.line} {', '.join(rule.selectors)[:120]} ({rule.nested} nested)")
        for prop, value, _imp in rule.decls:
            if prop.startswith("--") or (idx, prop) in used:
                continue
            ref = re.fullmatch(r"var\((--[\w-]+)\)", value)
            if prop in COLOR_PROPS and ref and ref[1] in mapped_vars and not rule.condition:
                continue  # the colour arrives through the token that variable maps to
            if prop in FONT_PROPS:
                reason = "fonts and text metrics (Okilum keeps its own typography)"
            elif prop in MOTION_PROPS:
                reason = "transitions and animations"
            elif rule.condition:
                reason = "rules inside @media/@supports"
            else:
                reason = "styling with no Okilum token or slot"
            sel = ", ".join(rule.selectors)
            report.skip(reason, f"L{rule.line} {sel[:160]} {{ {prop} }}")
    report.unread_vars = sorted(n for n in variables.theme_names() if n not in mapped_vars)

    basis = (f"Converted from the {name} theme for Obsidian"
             + (f" ({credit})" if credit else "")
             + f" by scripts/themes/obsidian-theme-convert.py; {report.dialect} variables"
             + (f"; the {report.derived_mode} variant is derived" if report.derived_mode else "")
             + ". Tuned to WCAG AA" + (" (AAA reading text)" if high_contrast else "") + ".")
    out_modes = {}
    for mode in MODES:
        t = modes[mode]
        entry = {k: to_hex(t[k]) for k in TOKEN_ORDER}
        levels = {k: to_hex(t[k]) for k in HEADING_LEVELS}
        if len(set(levels.values())) > 1 or levels["heading1"] != entry["heading"]:
            entry.update(levels)
        if deco_by_mode.get(mode):
            entry["decorations"] = deco_by_mode[mode]
        if syntax_by_mode.get(mode):
            entry["syntax"] = app_syntax(syntax_by_mode[mode])
        out_modes[mode] = entry
    theme = {"schema": SCHEMA, "version": "1.0.0", "id": theme_id, "name": name,
             "basis": basis, "modes": out_modes}
    return theme, report


def render_report(report: Report, theme: dict, source_name: str) -> str:
    lines = [f"# {report.name}: Obsidian theme conversion report", "",
             f"Source: `{source_name}`. Output: `okilum-theme/v1`, id `{theme['id']}`.", ""]
    pre, new = report.dialect_counts["pre-1.0"], report.dialect_counts["1.0+"]
    lines += [f"- Variable dialect: **{report.dialect}** (pre-1.0 variables: {len(pre)}"
              f"{' — ' + ', '.join(pre[:8]) if pre else ''}; 1.0+ variables: {len(new)}"
              f"{' — ' + ', '.join(new[:8]) if new else ''})."]
    lines.append(f"- Derived mode: {report.derived_mode or 'none, the theme ships both'}.")
    fails = [c for c in report.contrast if not c[4]]
    lines.append(f"- Contrast: {len(report.contrast) - len(fails)}/{len(report.contrast)} checks pass"
                 f"{'' if not fails else ' — FAILURES below'}; {len(report.adjustments)} colours adjusted.")
    total = sum(len(v) for v in report.not_carried.values())
    lines.append(f"- Not carried over: {total} items in {len(report.not_carried)} groups; "
                 f"{len(report.unread_vars)} theme variables not read.")
    for note in report.notes:
        lines.append(f"- Note: {note}.")
    lines += ["", "## Tokens", "", "| Token | Light | Light source | Dark | Dark source |", "| --- | --- | --- | --- | --- |"]
    keys = TOKEN_ORDER + [k for k in HEADING_LEVELS if k in theme["modes"]["dark"]]
    for k in keys:
        lv, dv = theme["modes"]["light"].get(k, ""), theme["modes"]["dark"].get(k, "")
        ls = report.sources["light"].get(k, "")
        ds = report.sources["dark"].get(k, "")
        lines.append(f"| `{k}` | `{lv}` | {ls or '—'} | `{dv}` | {ds or '—'} |")
    lines += ["", "## Adjusted for contrast or hue", ""]
    if report.adjustments:
        lines += ["| Mode | Token | From | To | Why |", "| --- | --- | --- | --- | --- |"]
        lines += [f"| {m} | `{k}` | `{a}` | `{b}` | {w} |" for m, k, a, b, w in report.adjustments]
    else:
        lines.append("None.")
    lines += ["", "## Contrast", "", "| Mode | Pair | Ratio | Minimum | Result |", "| --- | --- | --- | --- | --- |"]
    lines += [f"| {m} | {p} | {r:.2f} | {mn:g} | {'pass' if ok else '**FAIL**'} |"
              for m, p, r, mn, ok in report.contrast]
    lines += ["", "## Decoration slots and syntax", ""]
    for mode in MODES:
        entry = theme["modes"][mode]
        lines.append(f"- {mode} decorations: `{json.dumps(entry.get('decorations', {}))}`")
        lines.append(f"- {mode} syntax: `{json.dumps(entry.get('syntax', {}))}`")
    lines += [f"- {d}" for d in report.decorations]
    if report.unresolved:
        lines += ["", "## Unresolved values", ""] + [f"- {u}" for u in report.unresolved]
    lines += ["", "## Not carried over", ""]
    for reason, items in sorted(report.not_carried.items(), key=lambda kv: -len(kv[1])):
        lines += [f"### {reason} ({len(items)})", ""] + [f"- `{i}`" for i in items] + [""]
    lines += ["## Theme variables not read", "",
              ", ".join(f"`{v}`" for v in report.unread_vars) or "None.", ""]
    return "\n".join(lines)


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("css", type=Path, help="the theme's theme.css (or obsidian.css)")
    ap.add_argument("--id", required=True, help="theme key, e.g. obsidianite")
    ap.add_argument("--name", required=True, help="display name")
    ap.add_argument("--credit", default="", help="source and licence for the basis field")
    ap.add_argument("--high-contrast", action="store_true", help="AAA (7:1) reading text")
    ap.add_argument("--out", type=Path, help="theme JSON (default: stdout)")
    ap.add_argument("--report", type=Path, help="Markdown report (default: stderr summary only)")
    args = ap.parse_args(argv)
    css = args.css.read_text(encoding="utf-8", errors="replace")
    theme, report = convert(css, theme_id=args.id, name=args.name, credit=args.credit,
                            high_contrast=args.high_contrast)
    text = json.dumps(theme, indent=2, ensure_ascii=False) + "\n"
    if args.out:
        args.out.write_text(text, encoding="utf-8")
    else:
        sys.stdout.write(text)
    rendered = render_report(report, theme, args.css.name)
    if args.report:
        args.report.write_text(rendered, encoding="utf-8")
    fails = [c for c in report.contrast if not c[4]]
    summary = rendered.split("\n## Tokens", 1)[0]
    print(summary, file=sys.stderr)
    return 1 if fails else 0


if __name__ == "__main__":
    sys.exit(main())

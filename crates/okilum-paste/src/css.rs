//! The little CSS a clipboard needs: inline `style=""` and simple
//! `tag.class {}` rules (Cocoa HTML Writer, Excel, Word), reduced to the
//! properties that become Markdown.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Decl {
    pub bold: Option<bool>,
    pub italic: Option<bool>,
    pub strike: Option<bool>,
    pub mono: Option<bool>,
    pub mark: Option<bool>,
    pub hidden: bool,
    /// `white-space: pre*` keeps newlines.
    pub pre: Option<bool>,
    /// Word: `mso-list: l0 level2 lfo1`.
    pub mso_level: Option<u32>,
    /// Word: `mso-list: Ignore` marks the bullet glyph run.
    pub mso_ignore: bool,
}

impl Decl {
    pub fn apply(&mut self, other: &Decl) {
        macro_rules! take {
            ($($f:ident),*) => {$(if other.$f.is_some() { self.$f = other.$f; })*};
        }
        take!(bold, italic, strike, mono, mark, pre, mso_level);
        self.hidden |= other.hidden;
        self.mso_ignore |= other.mso_ignore;
    }
}

#[derive(Debug, Default)]
pub struct Sheet {
    rules: Vec<(Option<String>, Option<String>, Decl)>,
}

impl Sheet {
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// Adds the rules of one `<style>` element. Only `tag`, `.class` and
    /// `tag.class` selectors are kept; at-rules are skipped whole.
    pub fn add(&mut self, css: &str) {
        let css = strip_comments(css);
        let bytes = css.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            let Some(open) = css[i..].find('{').map(|o| i + o) else {
                break;
            };
            let selector = css[i..open].trim();
            // Find the matching close brace (at-rules nest).
            let mut depth = 0usize;
            let mut close = None;
            for (k, b) in bytes.iter().enumerate().skip(open) {
                match b {
                    b'{' => depth += 1,
                    b'}' => {
                        depth -= 1;
                        if depth == 0 {
                            close = Some(k);
                            break;
                        }
                    }
                    _ => {}
                }
            }
            let close = close.unwrap_or(bytes.len());
            if !selector.starts_with('@') {
                let body = &css[(open + 1).min(close)..close];
                let mut decl = Decl::default();
                parse_decls(body, &mut decl);
                if decl != Decl::default() {
                    for sel in selector.split(',') {
                        if let Some((tag, class)) = simple_selector(sel.trim()) {
                            self.rules.push((tag, class, decl));
                        }
                    }
                }
            }
            i = close + 1;
        }
    }

    pub fn apply(&self, tag: &str, classes: &str, into: &mut Decl) {
        for (rule_tag, rule_class, decl) in &self.rules {
            if rule_tag.as_deref().is_some_and(|t| t != tag) {
                continue;
            }
            if let Some(class) = rule_class {
                if !classes.split_ascii_whitespace().any(|c| c == class) {
                    continue;
                }
            }
            into.apply(decl);
        }
    }
}

fn strip_comments(css: &str) -> String {
    let mut out = String::with_capacity(css.len());
    let mut rest = css;
    while let Some(start) = rest.find("/*") {
        out.push_str(&rest[..start]);
        match rest[start + 2..].find("*/") {
            Some(end) => rest = &rest[start + 2 + end + 2..],
            None => {
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out.replace("<!--", " ").replace("-->", " ")
}

fn simple_selector(sel: &str) -> Option<(Option<String>, Option<String>)> {
    if sel.is_empty() {
        return None;
    }
    let ok = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    };
    let (tag, class) = match sel.split_once('.') {
        Some((tag, class)) => (tag, Some(class)),
        None => (sel, None),
    };
    if !tag.is_empty() && !ok(tag) {
        return None;
    }
    if class.is_some_and(|c| !ok(c)) {
        return None;
    }
    if tag.is_empty() && class.is_none() {
        return None;
    }
    Some((
        (!tag.is_empty()).then(|| tag.to_ascii_lowercase()),
        class.map(str::to_string),
    ))
}

pub fn parse_decls(style: &str, into: &mut Decl) {
    for decl in style.split(';') {
        let Some((prop, value)) = decl.split_once(':') else {
            continue;
        };
        let prop = prop.trim().to_ascii_lowercase();
        let value = value.trim();
        let value = value
            .strip_suffix("!important")
            .unwrap_or(value)
            .trim()
            .to_ascii_lowercase();
        match prop.as_str() {
            "font-weight" => into.bold = weight(&value),
            "font-style" => {
                if value.starts_with("italic") || value.starts_with("oblique") {
                    into.italic = Some(true);
                } else if value == "normal" {
                    into.italic = Some(false);
                }
            }
            "font-family" => {
                if !matches!(
                    value.as_str(),
                    "inherit" | "initial" | "unset" | "revert" | ""
                ) {
                    into.mono = Some(is_mono_family(&value));
                }
            }
            "font" => font_shorthand(&value, into),
            "text-decoration" | "text-decoration-line" => {
                if value.contains("line-through") {
                    into.strike = Some(true);
                }
            }
            "background" | "background-color" => {
                if is_yellow(&value) {
                    into.mark = Some(true);
                }
            }
            "mso-highlight" => {
                if value != "none" && value != "auto" {
                    into.mark = Some(true);
                }
            }
            "display" => {
                if value == "none" {
                    into.hidden = true;
                }
            }
            "visibility" => {
                if value == "hidden" || value == "collapse" {
                    into.hidden = true;
                }
            }
            "mso-hide" => {
                if value == "all" {
                    into.hidden = true;
                }
            }
            "white-space" => {
                into.pre = Some(matches!(
                    value.as_str(),
                    "pre" | "pre-wrap" | "pre-line" | "break-spaces"
                ));
            }
            "mso-list" => {
                if value == "ignore" {
                    into.mso_ignore = true;
                } else if let Some(pos) = value.find("level") {
                    let digits: String = value[pos + 5..]
                        .chars()
                        .take_while(char::is_ascii_digit)
                        .collect();
                    into.mso_level = digits.parse().ok();
                }
            }
            _ => {}
        }
    }
}

fn weight(value: &str) -> Option<bool> {
    match value {
        "bold" | "bolder" => Some(true),
        "normal" | "lighter" => Some(false),
        v => v.parse::<f32>().ok().map(|n| n >= 600.0),
    }
}

const MONO: [&str; 24] = [
    "menlo",
    "monaco",
    "sf mono",
    "sfmono",
    "consolas",
    "courier",
    "jetbrains mono",
    "fira code",
    "fira mono",
    "source code pro",
    "monospace",
    "ui-monospace",
    "roboto mono",
    "ubuntu mono",
    "dejavu sans mono",
    "liberation mono",
    "andale mono",
    "lucida console",
    "cascadia",
    "inconsolata",
    "ibm plex mono",
    "pt mono",
    "hack",
    "noto sans mono",
];

pub fn is_mono_family(families: &str) -> bool {
    let mut first = true;
    for family in families.split(',') {
        let family = family
            .trim()
            .trim_matches(|c| c == '\'' || c == '"')
            .trim()
            .to_ascii_lowercase();
        if family == "monospace" {
            return true;
        }
        if first && MONO.iter().any(|m| family.starts_with(m)) {
            return true;
        }
        first = false;
    }
    false
}

fn font_shorthand(value: &str, into: &mut Decl) {
    // `[style] [variant] [weight] size[/line-height] family[, family]*`
    let mut tokens = value.split_ascii_whitespace();
    let mut family = String::new();
    for token in tokens.by_ref() {
        let starts_numeric = token
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_digit() || c == '.');
        let is_size = starts_numeric
            && (token.contains("px")
                || token.contains("pt")
                || token.contains("em")
                || token.contains('%')
                || token.contains('/'));
        if is_size {
            break;
        }
        match token {
            "italic" | "oblique" => into.italic = Some(true),
            t => {
                if let Some(b) = weight(t) {
                    if t != "normal" {
                        into.bold = Some(b);
                    }
                }
            }
        }
    }
    for token in tokens {
        if !family.is_empty() {
            family.push(' ');
        }
        family.push_str(token);
    }
    if !family.is_empty() {
        into.mono = Some(is_mono_family(&family));
    }
}

fn is_yellow(value: &str) -> bool {
    let v: String = value.chars().filter(|c| !c.is_whitespace()).collect();
    [
        "yellow",
        "#ff0",
        "#ffff00",
        "rgb(255,255,0)",
        "rgba(255,255,0,1)",
    ]
    .iter()
    .any(|y| v == *y || v.starts_with(&format!("{y} ")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn class_rules_and_shorthand() {
        let mut sheet = Sheet::default();
        sheet.add(
            "<!-- p.p1 {margin: 0.0px; font: 13.0px 'Helvetica Neue'}\n\
             span.s1 {font-weight: bold} .xl65 {mso-style-parent:style0; font-weight:700;}\n\
             @media print { p { display: none } }\n\
             span.s2 {font: 12.0px Menlo} /* c */ td div.x {font-weight:bold} -->",
        );
        let mut d = Decl::default();
        sheet.apply("span", "s1", &mut d);
        assert_eq!(d.bold, Some(true));
        let mut d = Decl::default();
        sheet.apply("td", "xl65", &mut d);
        assert_eq!(d.bold, Some(true));
        let mut d = Decl::default();
        sheet.apply("span", "s2", &mut d);
        assert_eq!(d.mono, Some(true));
        let mut d = Decl::default();
        sheet.apply("p", "p1", &mut d);
        assert_eq!(d.mono, Some(false));
        assert!(!d.hidden);
    }

    #[test]
    fn inline_declarations() {
        let mut d = Decl::default();
        parse_decls(
            "font-weight:normal;font-style:italic;text-decoration:line-through;mso-list:l0 level3 lfo1",
            &mut d,
        );
        assert_eq!(d.bold, Some(false));
        assert_eq!(d.italic, Some(true));
        assert_eq!(d.strike, Some(true));
        assert_eq!(d.mso_level, Some(3));
        let mut d = Decl::default();
        parse_decls("font: bold 12px/14px \"Courier New\", monospace", &mut d);
        assert_eq!((d.bold, d.mono), (Some(true), Some(true)));
    }
}

//! A bounded TeX math subset for inline `$…$` formulas (#1645).
//!
//! Three consumers share one parse: the conservative inline rule that keeps
//! money as text, a one-line Unicode rendering Live Preview draws in place of
//! a formula's source, and a layout tree the editor's floating preview draws
//! in two dimensions. Pure and allocation-bounded by the formula length; it
//! never sees the rest of the note.
//!
//! Unsupported syntax is an [`Error`] with a short message, never a guess.
use std::ops::Range;

/// Formulas longer than this are left as source: inline math is short, and a
/// pathological span must not cost more than the line it sits on.
pub const MAX_FORMULA_BYTES: usize = 4 * 1024;
/// Nesting deeper than this is refused instead of recursing further.
const MAX_DEPTH: usize = 24;

/// The conservative inline rule on top of the dollar syntax the parser
/// already applied (opening `$` not followed by whitespace, closing `$` not
/// preceded by whitespace nor followed by a digit, one line): a formula must
/// be more than a number. `$2$`, `$1.5k$` and `$-3.50$` stay text, so money
/// in a financial note never becomes a formula.
pub fn is_inline_formula(content: &str) -> bool {
    let trimmed = content.trim();
    if trimmed.is_empty() || trimmed.len() > MAX_FORMULA_BYTES || trimmed.contains(['\n', '\r']) {
        return false;
    }
    // `$5 and $2$`: a `$` inside means the dollars pair money, not TeX.
    let inner_dollar = trimmed
        .char_indices()
        .any(|(at, c)| c == '$' && !trimmed[..at].ends_with('\\'));
    !inner_dollar && !is_amount(trimmed)
}

/// A number, optionally signed, with separators and one magnitude or
/// percent suffix: `2`, `1,500`, `-3.50`, `1.5k`, `12%`, `3 M`.
fn is_amount(text: &str) -> bool {
    let body = text.strip_prefix(['+', '-', '~', '≈']).unwrap_or(text);
    let body = body.trim_end();
    let body = body
        .strip_suffix(['k', 'K', 'm', 'M', 'b', 'B', '%'])
        .map(str::trim_end)
        .unwrap_or(body);
    body.starts_with(|c: char| c.is_ascii_digit())
        && body
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '.' | ',' | '\''))
}

/// What a formula failed on, phrased for the person writing it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error(pub String);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

fn error<T>(message: impl Into<String>) -> Result<T, Error> {
    Err(Error(message.into()))
}

/// TeX atom classes, as far as spacing and font are concerned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    /// Letters: drawn italic.
    Var,
    /// Digits and upright symbols.
    Ord,
    /// A named operator (`sin`, `lim`): upright, a space before an operand.
    Function,
    /// A large operator (`∑`, `∫`): scripts become limits in the preview.
    Large,
    Bin,
    Rel,
    Open,
    Close,
    Punct,
    /// `\text{…}`: upright, spaces kept.
    Text,
}

/// Font variants that change how glyphs are drawn, not which glyphs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Font {
    pub upright: bool,
    pub bold: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Node {
    Atom {
        text: String,
        class: Class,
        font: Font,
    },
    Group(Vec<Node>),
    Scripts {
        base: Box<Node>,
        sub: Option<Vec<Node>>,
        sup: Option<Vec<Node>>,
    },
    /// `bar` is false for `\binom`.
    Frac {
        num: Vec<Node>,
        den: Vec<Node>,
        bar: bool,
    },
    Sqrt {
        index: Option<Vec<Node>>,
        body: Vec<Node>,
    },
    Fenced {
        open: String,
        close: String,
        body: Vec<Node>,
    },
    Accent {
        mark: char,
        body: Vec<Node>,
    },
    /// Horizontal space in eighteenths of an em (TeX's `mu`).
    Space(u8),
}

impl Node {
    fn atom(text: impl Into<String>, class: Class) -> Self {
        Node::Atom {
            text: text.into(),
            class,
            font: Font::default(),
        }
    }
    /// The class that decides spacing next to this node.
    pub fn class(&self) -> Class {
        match self {
            Node::Atom { class, .. } => *class,
            Node::Scripts { base, .. } => base.class(),
            Node::Fenced { .. } | Node::Frac { .. } | Node::Sqrt { .. } => Class::Ord,
            Node::Group(nodes) | Node::Accent { body: nodes, .. } => match nodes.as_slice() {
                [only] => only.class(),
                _ => Class::Ord,
            },
            Node::Space(_) => Class::Punct,
        }
    }
}

/// Parse one formula's content (without its `$` delimiters).
pub fn parse(source: &str) -> Result<Vec<Node>, Error> {
    if source.len() > MAX_FORMULA_BYTES {
        return error("Formula is too long to preview");
    }
    let mut parser = Parser {
        chars: source.char_indices().peekable(),
        source,
        depth: 0,
    };
    let nodes = parser.row(End::Eof)?;
    if nodes.is_empty() {
        return error("Empty formula");
    }
    Ok(nodes)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum End {
    Eof,
    Brace,
    Bracket,
    Right,
}

struct Parser<'s> {
    chars: std::iter::Peekable<std::str::CharIndices<'s>>,
    source: &'s str,
    depth: usize,
}

enum Token {
    Char(char),
    Command(String),
}

impl Parser<'_> {
    fn skip_spaces(&mut self) {
        while self.chars.next_if(|(_, c)| c.is_whitespace()).is_some() {}
    }

    fn next_token(&mut self) -> Option<Token> {
        self.skip_spaces();
        let (_, c) = self.chars.next()?;
        if c != '\\' {
            return Some(Token::Char(c));
        }
        let Some(&(start, first)) = self.chars.peek() else {
            return Some(Token::Command(String::new()));
        };
        self.chars.next();
        if !first.is_ascii_alphabetic() {
            return Some(Token::Command(first.to_string()));
        }
        let mut end = start + first.len_utf8();
        while let Some((at, c)) = self.chars.next_if(|(_, c)| c.is_ascii_alphabetic()) {
            end = at + c.len_utf8();
        }
        Some(Token::Command(self.source[start..end].to_owned()))
    }

    fn peek_char(&mut self) -> Option<char> {
        self.skip_spaces();
        self.chars.peek().map(|&(_, c)| c)
    }

    fn row(&mut self, end: End) -> Result<Vec<Node>, Error> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return error("Formula is nested too deeply");
        }
        let mut nodes: Vec<Node> = Vec::new();
        loop {
            match (self.peek_char(), end) {
                (None, End::Eof) => break,
                (None, End::Brace | End::Bracket) => return error("Missing closing brace"),
                (None, End::Right) => return error("\\left without \\right"),
                (Some('}'), End::Brace) | (Some(']'), End::Bracket) => {
                    self.chars.next();
                    break;
                }
                (Some('}'), _) => return error("Unexpected closing brace"),
                (Some('^' | '_'), _) => {
                    let base = nodes.pop().unwrap_or(Node::Group(Vec::new()));
                    nodes.push(self.scripts(base)?);
                    continue;
                }
                (Some('\''), _) => {
                    self.chars.next();
                    let base = nodes.pop().unwrap_or(Node::Group(Vec::new()));
                    nodes.push(attach(base, None, Some(vec![Node::atom("′", Class::Ord)]))?);
                    continue;
                }
                _ => {}
            }
            if end == End::Right && self.at_command("right") {
                break;
            }
            match self.item()? {
                Some(node) => nodes.push(node),
                None => continue,
            }
        }
        self.depth -= 1;
        Ok(nodes)
    }

    fn at_command(&mut self, name: &str) -> bool {
        self.skip_spaces();
        let rest = self.chars.peek().map_or("", |&(at, _)| &self.source[at..]);
        rest.strip_prefix('\\')
            .and_then(|r| r.strip_prefix(name))
            .is_some_and(|r| !r.starts_with(|c: char| c.is_ascii_alphabetic()))
    }

    fn scripts(&mut self, base: Node) -> Result<Node, Error> {
        let mut base = base;
        while let Some(c @ ('^' | '_')) = self.peek_char() {
            self.chars.next();
            let arg = self.argument()?;
            let (sub, sup) = if c == '^' {
                (None, Some(arg))
            } else {
                (Some(arg), None)
            };
            base = attach(base, sub, sup)?;
        }
        Ok(base)
    }

    /// A braced group or one token: `\frac12`, `x^2`, `x^\alpha`.
    fn argument(&mut self) -> Result<Vec<Node>, Error> {
        match self.peek_char() {
            None | Some('}') => error("Missing argument"),
            Some('{') => {
                self.chars.next();
                self.row(End::Brace)
            }
            Some('^' | '_') => error("Missing argument"),
            Some(_) => match self.item()? {
                Some(node) => Ok(vec![node]),
                None => error("Missing argument"),
            },
        }
    }

    fn optional(&mut self) -> Result<Option<Vec<Node>>, Error> {
        if self.peek_char() == Some('[') {
            self.chars.next();
            return self.row(End::Bracket).map(Some);
        }
        Ok(None)
    }

    /// Raw braced text for `\text{…}`, spaces included.
    fn text_argument(&mut self) -> Result<String, Error> {
        if self.peek_char() != Some('{') {
            return match self.next_token() {
                Some(Token::Char(c)) => Ok(c.to_string()),
                _ => error("Missing argument"),
            };
        }
        self.chars.next();
        let mut depth = 0usize;
        let mut text = String::new();
        while let Some((_, c)) = self.chars.next() {
            match c {
                '{' => depth += 1,
                '}' if depth == 0 => return Ok(text),
                '}' => depth -= 1,
                '\\' => {
                    if let Some((_, escaped)) = self.chars.next() {
                        text.push(escaped);
                    }
                    continue;
                }
                _ => {}
            }
            if c != '{' && c != '}' {
                text.push(c);
            }
        }
        error("Missing closing brace")
    }

    fn delimiter(&mut self) -> Result<String, Error> {
        match self.next_token() {
            Some(Token::Char('.')) => Ok(String::new()),
            Some(Token::Char(c @ ('(' | ')' | '[' | ']' | '|' | '/' | '<' | '>'))) => Ok(match c {
                '<' => "⟨".into(),
                '>' => "⟩".into(),
                c => c.to_string(),
            }),
            Some(Token::Command(name)) => match delimiter_command(&name) {
                Some(text) => Ok(text.into()),
                None => error(format!("\\{name} is not a delimiter")),
            },
            _ => error("Missing delimiter"),
        }
    }

    fn item(&mut self) -> Result<Option<Node>, Error> {
        let Some(token) = self.next_token() else {
            return Ok(None);
        };
        let name = match token {
            Token::Char(c) => return self.char_item(c).map(Some),
            Token::Command(name) => name,
        };
        Ok(Some(match name.as_str() {
            "" => return error("Lone backslash"),
            "frac" | "dfrac" | "tfrac" | "cfrac" => Node::Frac {
                num: self.argument()?,
                den: self.argument()?,
                bar: true,
            },
            "binom" | "dbinom" | "tbinom" => Node::Fenced {
                open: "(".into(),
                close: ")".into(),
                body: vec![Node::Frac {
                    num: self.argument()?,
                    den: self.argument()?,
                    bar: false,
                }],
            },
            "sqrt" => Node::Sqrt {
                index: self.optional()?,
                body: self.argument()?,
            },
            "left" => {
                let open = self.delimiter()?;
                let body = self.row(End::Right)?;
                if !self.at_command("right") {
                    return error("\\left without \\right");
                }
                self.next_token();
                let close = self.delimiter()?;
                Node::Fenced { open, close, body }
            }
            "right" => return error("\\right without \\left"),
            "big" | "Big" | "bigg" | "Bigg" | "bigl" | "bigr" | "Bigl" | "Bigr" | "biggl"
            | "biggr" | "Biggl" | "Biggr" | "bigm" | "Bigm" => {
                let text = self.delimiter()?;
                let class = if name.ends_with('l') {
                    Class::Open
                } else if name.ends_with('r') {
                    Class::Close
                } else {
                    Class::Ord
                };
                Node::atom(text, class)
            }
            "text" | "textrm" | "textup" | "textnormal" | "mbox" | "textit" | "textbf" => {
                let text = self.text_argument()?;
                Node::Atom {
                    text,
                    class: Class::Text,
                    font: Font {
                        upright: name != "textit",
                        bold: name == "textbf",
                    },
                }
            }
            "operatorname" => Node::atom(self.text_argument()?, Class::Function),
            "mathrm" | "mathup" | "mathsf" | "mathtt" | "rm" => {
                restyle(self.argument()?, |font| font.upright = true)
            }
            "mathit" => restyle(self.argument()?, |font| font.upright = false),
            "mathbf" | "boldsymbol" | "bm" | "bf" => restyle(self.argument()?, |font| {
                font.bold = true;
                font.upright |= name == "mathbf";
            }),
            "mathbb" => letters(self.argument()?, double_struck)?,
            "mathcal" | "mathscr" => letters(self.argument()?, script)?,
            "mathfrak" => letters(self.argument()?, fraktur)?,
            "not" => match self.item()? {
                Some(Node::Atom { text, class, font }) => Node::Atom {
                    text: negate(&text),
                    class,
                    font,
                },
                _ => return error("\\not needs a symbol"),
            },
            "displaystyle" | "textstyle" | "scriptstyle" | "limits" | "nolimits" => {
                return Ok(None)
            }
            _ => {
                if let Some(mark) = accent(&name) {
                    Node::Accent {
                        mark,
                        body: self.argument()?,
                    }
                } else if let Some(space) = space(&name) {
                    Node::Space(space)
                } else if let Some((text, class)) = symbol(&name) {
                    let mut node = Node::atom(text, class);
                    if class == Class::Var {
                        // Greek capitals are upright in TeX; lowercase italic.
                        if let Node::Atom { font, .. } = &mut node {
                            font.upright = text.chars().all(char::is_uppercase);
                        }
                    }
                    node
                } else if FUNCTIONS.contains(&name.as_str()) {
                    Node::atom(name, Class::Function)
                } else {
                    return error(format!("Unknown command \\{name}"));
                }
            }
        }))
    }

    fn char_item(&mut self, c: char) -> Result<Node, Error> {
        Ok(match c {
            '{' => Node::Group(self.row(End::Brace)?),
            'a'..='z' | 'A'..='Z' => Node::atom(c, Class::Var),
            '0'..='9' => {
                let mut number = c.to_string();
                while let Some((_, d)) =
                    self.chars.next_if(|(_, d)| d.is_ascii_digit() || *d == '.')
                {
                    number.push(d);
                }
                Node::atom(number, Class::Ord)
            }
            '+' => Node::atom("+", Class::Bin),
            '-' => Node::atom("−", Class::Bin),
            '*' => Node::atom("∗", Class::Bin),
            '/' => Node::atom("/", Class::Ord),
            '=' => Node::atom("=", Class::Rel),
            '<' => Node::atom("<", Class::Rel),
            '>' => Node::atom(">", Class::Rel),
            ':' => Node::atom(":", Class::Rel),
            ',' | ';' => Node::atom(c, Class::Punct),
            '(' | '[' => Node::atom(c, Class::Open),
            ')' | ']' => Node::atom(c, Class::Close),
            '|' | '.' | '!' | '?' | '@' | '"' => Node::atom(c, Class::Ord),
            '~' => Node::Space(6),
            '&' => return error("Alignment (&) needs a display block"),
            '#' => return error("Unexpected #"),
            '$' => return error("Unexpected $"),
            '%' => return error("Comments (%) are not supported in inline math"),
            c if c.is_alphabetic() => Node::atom(c, Class::Var),
            c => Node::atom(c, Class::Ord),
        })
    }
}

fn attach(base: Node, sub: Option<Vec<Node>>, sup: Option<Vec<Node>>) -> Result<Node, Error> {
    match base {
        Node::Scripts {
            base,
            sub: old_sub,
            sup: old_sup,
        } => {
            if (sub.is_some() && old_sub.is_some()) || (sup.is_some() && old_sup.is_some()) {
                // `x''` is the one legal double superscript: primes merge.
                if let (Some(old), Some(new)) = (&old_sup, &sup) {
                    if primes(old) && primes(new) {
                        let mut merged = old.clone();
                        merged.extend(new.iter().cloned());
                        return Ok(Node::Scripts {
                            base,
                            sub: old_sub,
                            sup: Some(merged),
                        });
                    }
                }
                return error(if sup.is_some() {
                    "Double superscript"
                } else {
                    "Double subscript"
                });
            }
            Ok(Node::Scripts {
                base,
                sub: old_sub.or(sub),
                sup: old_sup.or(sup),
            })
        }
        base => Ok(Node::Scripts {
            base: Box::new(base),
            sub,
            sup,
        }),
    }
}

fn primes(nodes: &[Node]) -> bool {
    nodes
        .iter()
        .all(|n| matches!(n, Node::Atom { text, .. } if text == "′"))
}

fn restyle(nodes: Vec<Node>, change: impl Fn(&mut Font) + Copy) -> Node {
    fn walk(node: &mut Node, change: impl Fn(&mut Font) + Copy) {
        match node {
            Node::Atom { font, .. } => change(font),
            Node::Group(nodes)
            | Node::Fenced { body: nodes, .. }
            | Node::Accent { body: nodes, .. } => nodes.iter_mut().for_each(|n| walk(n, change)),
            Node::Scripts { base, sub, sup } => {
                walk(base, change);
                for nodes in [sub, sup].into_iter().flatten() {
                    nodes.iter_mut().for_each(|n| walk(n, change));
                }
            }
            Node::Frac { num, den, .. } => num
                .iter_mut()
                .chain(den.iter_mut())
                .for_each(|n| walk(n, change)),
            Node::Sqrt { index, body } => {
                body.iter_mut()
                    .chain(index.iter_mut().flatten())
                    .for_each(|n| walk(n, change));
            }
            Node::Space(_) => {}
        }
    }
    let mut group = Node::Group(nodes);
    walk(&mut group, change);
    match group {
        Node::Group(mut nodes) if nodes.len() == 1 => nodes.remove(0),
        group => group,
    }
}

/// Letter-only alphabets: `\mathbb{R}`. Anything else in them is an error.
fn letters(nodes: Vec<Node>, map: fn(char) -> Option<char>) -> Result<Node, Error> {
    let mut text = String::new();
    for node in &nodes {
        let Node::Atom { text: t, .. } = node else {
            return error("This alphabet takes letters only");
        };
        for c in t.chars() {
            text.push(map(c).ok_or_else(|| Error("This alphabet takes letters only".into()))?);
        }
    }
    Ok(Node::Atom {
        text,
        class: Class::Ord,
        font: Font {
            upright: true,
            bold: false,
        },
    })
}

fn double_struck(c: char) -> Option<char> {
    Some(match c {
        'C' => 'ℂ',
        'H' => 'ℍ',
        'N' => 'ℕ',
        'P' => 'ℙ',
        'Q' => 'ℚ',
        'R' => 'ℝ',
        'Z' => 'ℤ',
        'A'..='Z' => char::from_u32(0x1D538 + (c as u32 - 'A' as u32))?,
        'a'..='z' => char::from_u32(0x1D552 + (c as u32 - 'a' as u32))?,
        '0'..='9' => char::from_u32(0x1D7D8 + (c as u32 - '0' as u32))?,
        _ => return None,
    })
}

fn script(c: char) -> Option<char> {
    Some(match c {
        'B' => 'ℬ',
        'E' => 'ℰ',
        'F' => 'ℱ',
        'H' => 'ℋ',
        'I' => 'ℐ',
        'L' => 'ℒ',
        'M' => 'ℳ',
        'R' => 'ℛ',
        'A'..='Z' => char::from_u32(0x1D49C + (c as u32 - 'A' as u32))?,
        _ => return None,
    })
}

fn fraktur(c: char) -> Option<char> {
    Some(match c {
        'C' => 'ℭ',
        'H' => 'ℌ',
        'I' => 'ℑ',
        'R' => 'ℜ',
        'Z' => 'ℨ',
        'A'..='Z' => char::from_u32(0x1D504 + (c as u32 - 'A' as u32))?,
        'a'..='z' => char::from_u32(0x1D51E + (c as u32 - 'a' as u32))?,
        _ => return None,
    })
}

fn negate(text: &str) -> String {
    match text {
        "=" => "≠".into(),
        "<" => "≮".into(),
        ">" => "≯".into(),
        "≤" => "≰".into(),
        "≥" => "≱".into(),
        "∈" => "∉".into(),
        "⊂" => "⊄".into(),
        "⊆" => "⊈".into(),
        "≡" => "≢".into(),
        "∼" => "≁".into(),
        "≈" => "≉".into(),
        other => format!("{other}\u{338}"),
    }
}

fn accent(name: &str) -> Option<char> {
    Some(match name {
        "hat" | "widehat" => '\u{302}',
        "check" => '\u{30C}',
        "tilde" | "widetilde" => '\u{303}',
        "acute" => '\u{301}',
        "grave" => '\u{300}',
        "dot" => '\u{307}',
        "ddot" => '\u{308}',
        "breve" => '\u{306}',
        "bar" | "overline" => '\u{305}',
        "underline" => '\u{332}',
        "vec" | "overrightarrow" => '\u{20D7}',
        _ => return None,
    })
}

fn space(name: &str) -> Option<u8> {
    Some(match name {
        "," | "thinspace" => 3,
        ":" | ">" | "medspace" => 4,
        ";" | "thickspace" => 5,
        " " => 6,
        "quad" => 18,
        "qquad" => 36,
        "!" | "negthinspace" => 0,
        _ => return None,
    })
}

fn delimiter_command(name: &str) -> Option<&'static str> {
    Some(match name {
        "{" | "lbrace" => "{",
        "}" | "rbrace" => "}",
        "|" | "Vert" => "‖",
        "vert" => "|",
        "langle" => "⟨",
        "rangle" => "⟩",
        "lfloor" => "⌊",
        "rfloor" => "⌋",
        "lceil" => "⌈",
        "rceil" => "⌉",
        "lvert" | "rvert" => "|",
        "lVert" | "rVert" => "‖",
        "backslash" => "\\",
        _ => return None,
    })
}

const FUNCTIONS: &[&str] = &[
    "sin", "cos", "tan", "cot", "sec", "csc", "arcsin", "arccos", "arctan", "sinh", "cosh", "tanh",
    "coth", "log", "ln", "lg", "exp", "lim", "liminf", "limsup", "max", "min", "sup", "inf", "det",
    "gcd", "arg", "deg", "dim", "ker", "hom", "Pr", "mod", "bmod",
];

fn symbol(name: &str) -> Option<(&'static str, Class)> {
    use Class::*;
    Some(match name {
        // Greek.
        "alpha" => ("α", Var),
        "beta" => ("β", Var),
        "gamma" => ("γ", Var),
        "delta" => ("δ", Var),
        "epsilon" => ("ϵ", Var),
        "varepsilon" => ("ε", Var),
        "zeta" => ("ζ", Var),
        "eta" => ("η", Var),
        "theta" => ("θ", Var),
        "vartheta" => ("ϑ", Var),
        "iota" => ("ι", Var),
        "kappa" => ("κ", Var),
        "lambda" => ("λ", Var),
        "mu" => ("μ", Var),
        "nu" => ("ν", Var),
        "xi" => ("ξ", Var),
        "pi" => ("π", Var),
        "varpi" => ("ϖ", Var),
        "rho" => ("ρ", Var),
        "varrho" => ("ϱ", Var),
        "sigma" => ("σ", Var),
        "varsigma" => ("ς", Var),
        "tau" => ("τ", Var),
        "upsilon" => ("υ", Var),
        "phi" => ("ϕ", Var),
        "varphi" => ("φ", Var),
        "chi" => ("χ", Var),
        "psi" => ("ψ", Var),
        "omega" => ("ω", Var),
        "Gamma" => ("Γ", Var),
        "Delta" => ("Δ", Var),
        "Theta" => ("Θ", Var),
        "Lambda" => ("Λ", Var),
        "Xi" => ("Ξ", Var),
        "Pi" => ("Π", Var),
        "Sigma" => ("Σ", Var),
        "Upsilon" => ("Υ", Var),
        "Phi" => ("Φ", Var),
        "Psi" => ("Ψ", Var),
        "Omega" => ("Ω", Var),
        // Binary operators.
        "pm" => ("±", Bin),
        "mp" => ("∓", Bin),
        "times" => ("×", Bin),
        "div" => ("÷", Bin),
        "cdot" => ("·", Bin),
        "ast" => ("∗", Bin),
        "star" => ("⋆", Bin),
        "circ" => ("∘", Bin),
        "bullet" => ("•", Bin),
        "cup" => ("∪", Bin),
        "cap" => ("∩", Bin),
        "setminus" => ("∖", Bin),
        "wedge" | "land" => ("∧", Bin),
        "vee" | "lor" => ("∨", Bin),
        "oplus" => ("⊕", Bin),
        "ominus" => ("⊖", Bin),
        "otimes" => ("⊗", Bin),
        "odot" => ("⊙", Bin),
        // Relations.
        "le" | "leq" => ("≤", Rel),
        "ge" | "geq" => ("≥", Rel),
        "ne" | "neq" => ("≠", Rel),
        "leqslant" => ("⩽", Rel),
        "geqslant" => ("⩾", Rel),
        "ll" => ("≪", Rel),
        "gg" => ("≫", Rel),
        "approx" => ("≈", Rel),
        "equiv" => ("≡", Rel),
        "sim" => ("∼", Rel),
        "simeq" => ("≃", Rel),
        "cong" => ("≅", Rel),
        "propto" => ("∝", Rel),
        "in" => ("∈", Rel),
        "notin" => ("∉", Rel),
        "ni" => ("∋", Rel),
        "subset" => ("⊂", Rel),
        "supset" => ("⊃", Rel),
        "subseteq" => ("⊆", Rel),
        "supseteq" => ("⊇", Rel),
        "to" | "rightarrow" => ("→", Rel),
        "gets" | "leftarrow" => ("←", Rel),
        "leftrightarrow" => ("↔", Rel),
        "Rightarrow" | "implies" => ("⇒", Rel),
        "Leftarrow" => ("⇐", Rel),
        "Leftrightarrow" | "iff" => ("⇔", Rel),
        "mapsto" => ("↦", Rel),
        "uparrow" => ("↑", Rel),
        "downarrow" => ("↓", Rel),
        "mid" => ("∣", Rel),
        "parallel" => ("∥", Rel),
        "perp" => ("⊥", Rel),
        "vdash" => ("⊢", Rel),
        "models" => ("⊨", Rel),
        "coloneqq" => ("≔", Rel),
        // Large operators.
        "sum" => ("∑", Large),
        "prod" => ("∏", Large),
        "coprod" => ("∐", Large),
        "int" => ("∫", Large),
        "iint" => ("∬", Large),
        "iiint" => ("∭", Large),
        "oint" => ("∮", Large),
        "bigcup" => ("⋃", Large),
        "bigcap" => ("⋂", Large),
        "bigoplus" => ("⨁", Large),
        "bigotimes" => ("⨂", Large),
        // Ordinary symbols.
        "infty" => ("∞", Ord),
        "partial" => ("∂", Ord),
        "nabla" => ("∇", Ord),
        "forall" => ("∀", Ord),
        "exists" => ("∃", Ord),
        "nexists" => ("∄", Ord),
        "neg" | "lnot" => ("¬", Ord),
        "emptyset" | "varnothing" => ("∅", Ord),
        "ldots" | "dots" | "dotsc" => ("…", Ord),
        "cdots" | "dotsb" => ("⋯", Ord),
        "vdots" => ("⋮", Ord),
        "ddots" => ("⋱", Ord),
        "prime" => ("′", Ord),
        "hbar" => ("ℏ", Ord),
        "ell" => ("ℓ", Ord),
        "Re" => ("ℜ", Ord),
        "Im" => ("ℑ", Ord),
        "aleph" => ("ℵ", Ord),
        "angle" => ("∠", Ord),
        "triangle" => ("△", Ord),
        "degree" => ("°", Ord),
        "top" => ("⊤", Ord),
        "bot" => ("⊥", Ord),
        "checkmark" => ("✓", Ord),
        // Escapes and delimiters written as commands.
        "%" => ("%", Ord),
        "$" => ("$", Ord),
        "&" => ("&", Ord),
        "#" => ("#", Ord),
        "_" => ("_", Ord),
        "{" | "lbrace" => ("{", Open),
        "}" | "rbrace" => ("}", Close),
        "langle" => ("⟨", Open),
        "rangle" => ("⟩", Close),
        "lfloor" => ("⌊", Open),
        "rfloor" => ("⌋", Close),
        "lceil" => ("⌈", Open),
        "rceil" => ("⌉", Close),
        "|" | "Vert" => ("‖", Ord),
        "vert" => ("|", Ord),
        "backslash" => ("\\", Ord),
        "colon" => (":", Punct),
        _ => return None,
    })
}

/// One-line rendering: the formula drawn in place of its source. Letters
/// and bold glyphs are listed as runs for the editor's text styles.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Linear {
    pub text: String,
    /// Byte ranges of `text`, sorted and disjoint.
    pub runs: Vec<Run>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Run {
    pub range: Range<usize>,
    pub italic: bool,
    pub bold: bool,
}

/// Render `source` on one line, or the reason it cannot be drawn.
pub fn linear(source: &str) -> Result<Linear, Error> {
    let nodes = parse(source)?;
    let mut out = Linear::default();
    write_row(&nodes, &mut out);
    let text = out.text.trim().to_owned();
    if text.is_empty() {
        return error("Empty formula");
    }
    let lead = out.text.len() - out.text.trim_start().len();
    out.runs.retain_mut(|run| {
        run.range.start = run.range.start.saturating_sub(lead).min(text.len());
        run.range.end = run.range.end.saturating_sub(lead).min(text.len());
        !run.range.is_empty()
    });
    out.text = text;
    Ok(out)
}

impl Linear {
    fn push(&mut self, text: &str, font: Font, italic: bool) {
        let start = self.text.len();
        self.text.push_str(text);
        let italic = italic && !font.upright;
        if !(italic || font.bold) || text.is_empty() {
            return;
        }
        let end = self.text.len();
        match self.runs.last_mut() {
            Some(last)
                if last.range.end == start && last.italic == italic && last.bold == font.bold =>
            {
                last.range.end = end
            }
            _ => self.runs.push(Run {
                range: start..end,
                italic,
                bold: font.bold,
            }),
        }
    }
    fn plain(&mut self, text: &str) {
        self.push(text, Font::default(), false);
    }
}

fn write_row(nodes: &[Node], out: &mut Linear) {
    let mut previous: Option<Class> = None;
    for (index, node) in nodes.iter().enumerate() {
        let mut class = node.class();
        // A binary operator with no left operand is unary: `-x`, `(+1)`.
        if class == Class::Bin
            && matches!(
                previous,
                None | Some(Class::Bin | Class::Rel | Class::Open | Class::Punct | Class::Large)
            )
        {
            class = Class::Ord;
        }
        let spaced = matches!(class, Class::Bin | Class::Rel)
            && index + 1 < nodes.len()
            && previous.is_some();
        if spaced && !out.text.ends_with(' ') {
            out.plain(" ");
        }
        write_node(node, out);
        if spaced || (class == Class::Punct && index + 1 < nodes.len()) {
            out.plain(" ");
        } else if matches!(class, Class::Function | Class::Large)
            && nodes.get(index + 1).is_some_and(|next| {
                !matches!(next.class(), Class::Open | Class::Punct | Class::Close)
            })
        {
            out.plain("\u{2009}");
        }
        previous = Some(class);
    }
}

fn write_node(node: &Node, out: &mut Linear) {
    match node {
        Node::Atom { text, class, font } => {
            out.push(text, *font, matches!(class, Class::Var));
        }
        Node::Group(nodes) => write_row(nodes, out),
        Node::Scripts { base, sub, sup } => {
            write_node(base, out);
            if let Some(sub) = sub {
                write_script(sub, out, subscript_char, "_");
            }
            if let Some(sup) = sup {
                write_script(sup, out, superscript_char, "^");
            }
        }
        Node::Frac { num, den, bar } => {
            let (n, d) = (plain_text(num), plain_text(den));
            if *bar {
                if let Some(vulgar) = n.as_deref().zip(d.as_deref()).and_then(vulgar) {
                    out.plain(vulgar);
                    return;
                }
            }
            write_operand(num, out);
            out.plain(if *bar { "/" } else { " " });
            write_operand(den, out);
        }
        Node::Sqrt { index, body } => {
            let root = match index.as_deref().map(plain_text) {
                None => "√".to_owned(),
                Some(Some(i)) if i == "3" => "∛".to_owned(),
                Some(Some(i)) if i == "4" => "∜".to_owned(),
                Some(index) => match index.as_deref().and_then(|i| convert(i, superscript_char)) {
                    Some(small) => format!("{small}√"),
                    None => format!("({})√", index.unwrap_or_default()),
                },
            };
            out.plain(&root);
            write_operand(body, out);
        }
        Node::Fenced { open, close, body } => {
            out.plain(open);
            write_row(body, out);
            out.plain(close);
        }
        Node::Accent { mark, body } => {
            let start = out.text.len();
            write_row(body, out);
            // One combining mark after each base character it spans.
            let written = out.text.split_off(start);
            let marked: String = written
                .chars()
                .flat_map(|c| {
                    let combine = !c.is_whitespace() && !is_combining(c);
                    std::iter::once(c).chain(combine.then_some(*mark))
                })
                .collect();
            let mut runs: Vec<Run> = out
                .runs
                .iter()
                .filter(|r| r.range.end > start)
                .cloned()
                .collect();
            out.runs.retain(|r| r.range.end <= start);
            out.text.push_str(&marked);
            // Grown text keeps the runs' styling over the accented span.
            if let Some(first) = runs.first_mut() {
                let italic = first.italic;
                let bold = first.bold;
                out.runs.push(Run {
                    range: start..out.text.len(),
                    italic,
                    bold,
                });
            }
        }
        Node::Space(mu) => out.plain(match mu {
            0 => "",
            1..=4 => "\u{2009}",
            5..=12 => " ",
            13..=24 => "\u{2003}",
            _ => "\u{2003}\u{2003}",
        }),
    }
}

fn is_combining(c: char) -> bool {
    matches!(c as u32, 0x300..=0x36F | 0x20D0..=0x20FF)
}

/// Parenthesised unless it is a single item: `(a+b)/2`, `x/y`.
fn write_operand(nodes: &[Node], out: &mut Linear) {
    let single = matches!(
        nodes,
        [Node::Atom { .. } | Node::Scripts { .. } | Node::Fenced { .. } | Node::Sqrt { .. }]
    ) || matches!(nodes, [Node::Group(inner)] if inner.len() == 1);
    if single {
        write_row(nodes, out);
    } else {
        out.plain("(");
        write_row(nodes, out);
        out.plain(")");
    }
}

fn write_script(nodes: &[Node], out: &mut Linear, map: fn(char) -> Option<char>, marker: &str) {
    if let Some(small) = plain_text(nodes).and_then(|text| convert(&text, map)) {
        out.plain(&small);
        return;
    }
    out.plain(marker);
    let single = matches!(nodes, [Node::Atom { text, .. }] if text.chars().count() == 1);
    if single {
        write_row(nodes, out);
    } else {
        out.plain("(");
        write_row(nodes, out);
        out.plain(")");
    }
}

/// The text of atoms only, with no layout: `{i=1}` → `i=1`.
fn plain_text(nodes: &[Node]) -> Option<String> {
    let mut text = String::new();
    for node in nodes {
        match node {
            Node::Atom { text: t, font, .. } if !font.bold => text.push_str(t),
            Node::Group(inner) => text.push_str(&plain_text(inner)?),
            _ => return None,
        }
    }
    Some(text)
}

fn convert(text: &str, map: fn(char) -> Option<char>) -> Option<String> {
    text.chars().map(map).collect()
}

fn vulgar((num, den): (&str, &str)) -> Option<&'static str> {
    Some(match (num, den) {
        ("1", "2") => "½",
        ("1", "3") => "⅓",
        ("2", "3") => "⅔",
        ("1", "4") => "¼",
        ("3", "4") => "¾",
        ("1", "5") => "⅕",
        ("2", "5") => "⅖",
        ("3", "5") => "⅗",
        ("4", "5") => "⅘",
        ("1", "6") => "⅙",
        ("5", "6") => "⅚",
        ("1", "8") => "⅛",
        ("3", "8") => "⅜",
        ("5", "8") => "⅝",
        ("7", "8") => "⅞",
        _ => return None,
    })
}

fn superscript_char(c: char) -> Option<char> {
    Some(match c {
        '0' => '⁰',
        '1' => '¹',
        '2' => '²',
        '3' => '³',
        '4'..='9' => char::from_u32(0x2074 + (c as u32 - '4' as u32))?,
        '+' => '⁺',
        '−' | '-' => '⁻',
        '=' => '⁼',
        '(' => '⁽',
        ')' => '⁾',
        '′' => '′',
        'a' => 'ᵃ',
        'b' => 'ᵇ',
        'c' => 'ᶜ',
        'd' => 'ᵈ',
        'e' => 'ᵉ',
        'f' => 'ᶠ',
        'g' => 'ᵍ',
        'h' => 'ʰ',
        'i' => 'ⁱ',
        'j' => 'ʲ',
        'k' => 'ᵏ',
        'l' => 'ˡ',
        'm' => 'ᵐ',
        'n' => 'ⁿ',
        'o' => 'ᵒ',
        'p' => 'ᵖ',
        'r' => 'ʳ',
        's' => 'ˢ',
        't' => 'ᵗ',
        'u' => 'ᵘ',
        'v' => 'ᵛ',
        'w' => 'ʷ',
        'x' => 'ˣ',
        'y' => 'ʸ',
        'z' => 'ᶻ',
        'A' => 'ᴬ',
        'B' => 'ᴮ',
        'D' => 'ᴰ',
        'E' => 'ᴱ',
        'G' => 'ᴳ',
        'H' => 'ᴴ',
        'I' => 'ᴵ',
        'J' => 'ᴶ',
        'K' => 'ᴷ',
        'L' => 'ᴸ',
        'M' => 'ᴹ',
        'N' => 'ᴺ',
        'O' => 'ᴼ',
        'P' => 'ᴾ',
        'R' => 'ᴿ',
        'T' => 'ᵀ',
        'U' => 'ᵁ',
        'V' => 'ⱽ',
        'W' => 'ᵂ',
        'α' => 'ᵅ',
        'β' => 'ᵝ',
        'γ' => 'ᵞ',
        'δ' => 'ᵟ',
        'θ' => 'ᶿ',
        'φ' | 'ϕ' => 'ᵠ',
        'χ' => 'ᵡ',
        '∗' => '*',
        _ => return None,
    })
}

fn subscript_char(c: char) -> Option<char> {
    Some(match c {
        '0'..='9' => char::from_u32(0x2080 + (c as u32 - '0' as u32))?,
        '+' => '₊',
        '−' | '-' => '₋',
        '=' => '₌',
        '(' => '₍',
        ')' => '₎',
        'a' => 'ₐ',
        'e' => 'ₑ',
        'h' => 'ₕ',
        'i' => 'ᵢ',
        'j' => 'ⱼ',
        'k' => 'ₖ',
        'l' => 'ₗ',
        'm' => 'ₘ',
        'n' => 'ₙ',
        'o' => 'ₒ',
        'p' => 'ₚ',
        'r' => 'ᵣ',
        's' => 'ₛ',
        't' => 'ₜ',
        'u' => 'ᵤ',
        'v' => 'ᵥ',
        'x' => 'ₓ',
        'β' => 'ᵦ',
        'γ' => 'ᵧ',
        'ρ' => 'ᵨ',
        'φ' | 'ϕ' => 'ᵩ',
        'χ' => 'ᵪ',
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(source: &str) -> String {
        linear(source).unwrap().text
    }

    #[test]
    fn money_and_bare_numbers_are_not_formulas() {
        for amount in ["2", "1.5k", "1,500", "-3.50", "12%", "3 M", "~40", "1'000"] {
            assert!(!is_inline_formula(amount), "{amount}");
        }
        for formula in ["x", "2x", "x^2", "1+1", "\\pi", "2^{10}", "a_1"] {
            assert!(is_inline_formula(formula), "{formula}");
        }
        assert!(!is_inline_formula("5 and $2"));
        assert!(is_inline_formula("\\$5 + x"));
        assert!(!is_inline_formula(""));
        assert!(!is_inline_formula("a\nb"));
    }

    #[test]
    fn renders_scripts_symbols_and_spacing_on_one_line() {
        assert_eq!(line("x^2"), "x²");
        assert_eq!(line("a_i + b_{ij}"), "aᵢ + bᵢⱼ");
        assert_eq!(line("\\alpha \\le \\beta"), "α ≤ β");
        assert_eq!(line("-x"), "−x");
        assert_eq!(line("e^{i\\pi} = -1"), "e^(iπ) = −1");
        assert_eq!(line("\\frac{1}{2}"), "½");
        assert_eq!(line("\\frac{a+b}{c}"), "(a + b)/c");
        assert_eq!(line("\\sqrt{x}"), "√x");
        assert_eq!(line("\\sqrt[3]{x}"), "∛x");
        assert_eq!(line("\\sum_{i=1}^{n} i"), "∑ᵢ₌₁ⁿ\u{2009}i");
        assert_eq!(line("\\sin x"), "sin\u{2009}x");
        assert_eq!(line("f'(x)"), "f′(x)");
        assert_eq!(line("\\mathbb{R}^n"), "ℝⁿ");
        assert_eq!(line("\\left( x \\right)"), "(x)");
        assert_eq!(line("\\hat{x}"), "x\u{302}");
        assert_eq!(line("\\text{if } x"), "if x");
        assert_eq!(line("x \\not= y"), "x ≠ y");
        assert_eq!(line("f(x, y)"), "f(x, y)");
    }

    #[test]
    fn letters_are_italic_runs_and_upright_text_is_not() {
        let out = linear("x + \\mathrm{d}y").unwrap();
        assert_eq!(out.text, "x + dy");
        let italic: Vec<&str> = out
            .runs
            .iter()
            .filter(|r| r.italic)
            .map(|r| &out.text[r.range.clone()])
            .collect();
        assert_eq!(italic, ["x", "y"]);
        let bold = linear("\\mathbf{v}").unwrap();
        assert_eq!(
            bold.runs,
            [Run {
                range: 0..1,
                italic: false,
                bold: true
            }]
        );
    }

    #[test]
    fn errors_name_the_problem() {
        assert_eq!(linear("\\foo").unwrap_err().0, "Unknown command \\foo");
        assert_eq!(linear("{x").unwrap_err().0, "Missing closing brace");
        assert_eq!(linear("x}").unwrap_err().0, "Unexpected closing brace");
        assert_eq!(linear("x^").unwrap_err().0, "Missing argument");
        assert_eq!(linear("x^2^3").unwrap_err().0, "Double superscript");
        assert_eq!(linear("\\frac{1}").unwrap_err().0, "Missing argument");
        assert_eq!(linear("\\left( x").unwrap_err().0, "\\left without \\right");
        assert_eq!(
            linear("a & b").unwrap_err().0,
            "Alignment (&) needs a display block"
        );
        assert!(linear(&"{".repeat(100)).is_err());
        assert!(linear(&"x".repeat(MAX_FORMULA_BYTES + 1)).is_err());
    }

    #[test]
    fn layout_tree_keeps_two_dimensional_structure() {
        let nodes = parse("\\frac{a}{b^2}").unwrap();
        let [Node::Frac {
            num,
            den,
            bar: true,
        }] = nodes.as_slice()
        else {
            panic!("{nodes:?}");
        };
        assert_eq!(num.len(), 1);
        assert!(matches!(
            &den[0],
            Node::Scripts {
                sup: Some(_),
                sub: None,
                ..
            }
        ));
        assert!(matches!(
            parse("x''").unwrap().as_slice(),
            [Node::Scripts { sup: Some(primes), .. }] if primes.len() == 2
        ));
    }
}

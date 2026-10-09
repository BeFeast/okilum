//! The log viewer's query language (#602, slice 4): a hand-written parser for
//! a subset of hl's query syntax, so a query can move between hl and Okilum.
//! hl (github.com/pamburus/hl, MIT) is the design reference for the syntax
//! only; no hl code is used. See docs/research/602-hl-log-viewer.md §3.3.
//!
//! # Grammar
//!
//! ```text
//! query   = [ or ]                          blank matches every entry
//! or      = and { ("or" | "||") and }
//! and     = unary { ("and" | "&&") unary }
//! unary   = ("not" | "!") unary | primary
//! primary = "(" or ")"
//!         | "exists" "(" field ")"
//!         | field ["?"] op value
//!         | field ["?"] ["not"] "in" "(" value { "," value } ")"
//! op      = "=" | "!=" | "~=" | "!~=" | "~~=" | "!~~=" | "<" | "<=" | ">" | ">="
//! field   = word | "." word | string
//! value   = word | string
//! string  = '"' … '"' | "'" … "'"
//! ```
//!
//! Precedence, tightest first: `not`, `and`, `or`. Keywords are ASCII
//! case-insensitive.
//!
//! **Words** run until whitespace or one of `( ) , = < > ~`; `!` ends a word
//! only before `=` or `~`, `?` only before whitespace, an operator or a
//! parenthesis, and `&&`/`||` end it. A quote opens a string only at the start
//! of a token, so `msg=don't` is one word. Unicode is fine anywhere.
//!
//! **Strings** use JSON escapes (`\" \' \\ \/ \b \f \n \r \t \uXXXX`, with
//! surrogate pairs). Any other escape is an error, never passed through.
//!
//! **Fields.** Unquoted `level`, `msg`/`message` and `logger` are the record's
//! roles, resolved through the field aliases the indexer uses: `level` is the
//! normalised severity and takes level names (`level>=warn`). Every other name
//! is an exact, case-sensitive key; nested JSON objects are dotted
//! (`user.id`). A leading `.` or quotes force a raw key: `.level="NOTICE"`
//! tests the field named `level` as written.
//!
//! **Values.** An unquoted value that is a number (`-12`, `4.5`, `1e3`)
//! compares numerically: `=` matches `812.50` against `812.5`, and `<`, `<=`,
//! `>`, `>=` need a numeric field value. Any other value compares as text:
//! `=` is exact, the order operators are lexicographic by bytes (which suits
//! ISO 8601 timestamps), `~=` is a substring test and `~~=` an unanchored
//! regular expression (Rust `regex` syntax, compiled once at parse time).
//! Arrays and objects compare as their raw JSON text, `null`/`true`/`false`
//! as those words.
//!
//! **Absent fields.** A test on a field the record does not have is false,
//! and so is every test on a line that is not a record. That includes the
//! negated operators (`!=`, `!~=`, `!~~=`, `not in`): they mean "present and
//! not matching". `field?` includes records without the field, and
//! `exists(field)` tests presence alone. For `level`, absent means a record
//! without a level field or an unparsed line; a level field with an unmapped
//! spelling is present, matches `!=` and `not in`, and never orders.
//! `not` is plain logical negation, so `not status=500` also matches a
//! record without `status`. A key that occurs more than once matches when
//! any occurrence does (for the negated operators: when none does).
//!
//! Not supported from hl: `like`/glob, `contain`, and `in @file`, which would
//! let a query read arbitrary files.

use std::cmp::Ordering;
use std::fmt::{self, Write as _};
use std::ops::Range;
use std::str::FromStr;

use regex::{Regex, RegexBuilder};

use super::{Field, Format, Level, LogEntry, Record, Role};

/// Parentheses and `not`s deeper than this are an error, not a stack overflow.
const MAX_DEPTH: usize = 64;
/// Compiled-size cap per regular expression (the regex crate's default is 10x).
const REGEX_SIZE_LIMIT: usize = 1 << 20;
/// Names that cannot be written bare as a raw key.
const RESERVED: [&str; 9] = [
    "level", "msg", "message", "logger", "and", "or", "not", "in", "exists",
];

/// A parse error: a byte span into the query and a message for an inline
/// error under the query field. End-of-input errors have an empty span at
/// the end.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueryError {
    pub span: Range<usize>,
    pub message: String,
}

impl QueryError {
    fn new(span: Range<usize>, message: impl Into<String>) -> Self {
        QueryError {
            span,
            message: message.into(),
        }
    }

    /// Characters, not bytes, before the error in `source`: where a caret goes.
    pub fn column(&self, source: &str) -> usize {
        source
            .get(..self.span.start)
            .map_or(0, |head| head.chars().count())
    }
}

impl fmt::Display for QueryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for QueryError {}

/// A parsed query. `Display` writes a canonical form that parses back to an
/// equal query.
#[derive(Clone, Debug, PartialEq)]
pub struct Query {
    expr: Expr,
    needs_record: bool,
}

impl Query {
    pub fn parse(source: &str) -> Result<Query, QueryError> {
        let tokens = lex(source)?;
        let mut parser = Parser {
            source,
            tokens,
            pos: 0,
            depth: 0,
        };
        let expr = if parser.tokens.is_empty() {
            Expr::And(Vec::new())
        } else {
            parser.or()?
        };
        if let Some(token) = parser.bump() {
            return Err(parser.after_condition(token));
        }
        Ok(Query {
            needs_record: expr.needs_record(),
            expr,
        })
    }

    pub fn expr(&self) -> &Expr {
        &self.expr
    }

    /// True for the blank query, which matches every entry.
    pub fn is_empty(&self) -> bool {
        matches!(&self.expr, Expr::And(items) if items.is_empty())
    }

    /// Whether matching needs the decoded record, or the index entry is
    /// enough (level and presence-of-level tests only).
    pub fn needs_record(&self) -> bool {
        self.needs_record
    }

    /// `record` is the entry's decoded line, `None` for an unparsed line.
    pub fn matches(&self, entry: &LogEntry, record: Option<&Record>) -> bool {
        self.expr.matches(entry, record)
    }

    /// Decodes `line` only when the query looks at fields.
    pub fn matches_line(&self, entry: &LogEntry, line: &[u8], format: Format) -> bool {
        let record = if self.needs_record {
            Record::parse(line, format)
        } else {
            None
        };
        self.matches(entry, record.as_ref())
    }
}

impl FromStr for Query {
    type Err = QueryError;
    fn from_str(source: &str) -> Result<Query, QueryError> {
        Query::parse(source)
    }
}

impl fmt::Display for Query {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.expr.fmt(f)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Expr {
    /// All of the items; empty only for the blank query.
    And(Vec<Expr>),
    Or(Vec<Expr>),
    Not(Box<Expr>),
    Exists(FieldRef),
    Level {
        test: LevelTest,
        include_absent: bool,
    },
    /// Never with `FieldRef::Level`; level tests are `Expr::Level`.
    Field {
        field: FieldRef,
        test: Test,
        include_absent: bool,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FieldRef {
    Level,
    Message,
    Logger,
    /// An exact, dotted key.
    Key(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Test {
    Eq(Literal),
    Ne(Literal),
    Contains(String),
    NotContains(String),
    Regex(Pattern),
    NotRegex(Pattern),
    Compare(Cmp, Literal),
    In(Vec<Literal>),
    NotIn(Vec<Literal>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LevelTest {
    Eq(Level),
    Ne(Level),
    Compare(Cmp, Level),
    In(Vec<Level>),
    NotIn(Vec<Level>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cmp {
    Lt,
    Le,
    Gt,
    Ge,
}

/// A value as written. `number` is set only for an unquoted numeric word.
#[derive(Clone, Debug, PartialEq)]
pub struct Literal {
    pub text: String,
    pub number: Option<f64>,
}

#[derive(Clone, Debug)]
pub struct Pattern(Regex);

impl Pattern {
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl PartialEq for Pattern {
    fn eq(&self, other: &Self) -> bool {
        self.as_str() == other.as_str()
    }
}

impl Expr {
    pub fn matches(&self, entry: &LogEntry, record: Option<&Record>) -> bool {
        match self {
            Expr::And(items) => items.iter().all(|e| e.matches(entry, record)),
            Expr::Or(items) => items.iter().any(|e| e.matches(entry, record)),
            Expr::Not(inner) => !inner.matches(entry, record),
            Expr::Exists(FieldRef::Level) => level_present(entry.level()),
            Expr::Exists(field) => record.is_some_and(|r| values(r, field).next().is_some()),
            Expr::Level {
                test,
                include_absent,
            } => {
                let level = entry.level();
                if level_present(level) {
                    test.holds(level)
                } else {
                    *include_absent
                }
            }
            Expr::Field {
                field,
                test,
                include_absent,
            } => {
                let Some(record) = record else {
                    return *include_absent;
                };
                let mut found = values(record, field).peekable();
                if found.peek().is_none() {
                    *include_absent
                } else {
                    test.holds(found)
                }
            }
        }
    }

    fn needs_record(&self) -> bool {
        match self {
            Expr::And(items) | Expr::Or(items) => items.iter().any(Expr::needs_record),
            Expr::Not(inner) => inner.needs_record(),
            Expr::Exists(FieldRef::Level) | Expr::Level { .. } => false,
            Expr::Exists(_) | Expr::Field { .. } => true,
        }
    }
}

fn level_present(level: Level) -> bool {
    !matches!(level, Level::Missing | Level::Unparsed)
}

/// Every occurrence of `field` in the record.
fn values<'r>(record: &'r Record, field: &'r FieldRef) -> impl Iterator<Item = &'r Field> {
    let (role, key) = match field {
        FieldRef::Level => (Some(Role::Level), None),
        FieldRef::Message => (Some(Role::Message), None),
        FieldRef::Logger => (Some(Role::Logger), None),
        FieldRef::Key(key) => (None, Some(key)),
    };
    role.and_then(|role| record.role(role)).into_iter().chain(
        key.into_iter()
            .flat_map(|key| record.fields.iter().filter(move |f| f.key == *key)),
    )
}

impl Test {
    fn holds<'f>(&self, mut found: impl Iterator<Item = &'f Field>) -> bool {
        match self {
            Test::Eq(lit) => found.any(|f| lit.equals(f)),
            Test::Ne(lit) => !found.any(|f| lit.equals(f)),
            Test::Contains(text) => found.any(|f| f.value.contains(text.as_str())),
            Test::NotContains(text) => !found.any(|f| f.value.contains(text.as_str())),
            Test::Regex(pattern) => found.any(|f| pattern.0.is_match(&f.value)),
            Test::NotRegex(pattern) => !found.any(|f| pattern.0.is_match(&f.value)),
            Test::Compare(cmp, lit) => found.any(|f| lit.order(f).is_some_and(|o| cmp.holds(o))),
            Test::In(lits) => found.any(|f| lits.iter().any(|lit| lit.equals(f))),
            Test::NotIn(lits) => !found.any(|f| lits.iter().any(|lit| lit.equals(f))),
        }
    }
}

impl LevelTest {
    /// `level` is present: a severity or `Unknown`.
    fn holds(&self, level: Level) -> bool {
        match self {
            LevelTest::Eq(want) => level == *want,
            LevelTest::Ne(want) => level != *want,
            LevelTest::Compare(cmp, want) => level.is_severity() && cmp.holds(level.cmp(want)),
            LevelTest::In(set) => set.contains(&level),
            LevelTest::NotIn(set) => !set.contains(&level),
        }
    }
}

impl Cmp {
    /// `ordering` is the field's value relative to the literal.
    fn holds(self, ordering: Ordering) -> bool {
        match self {
            Cmp::Lt => ordering.is_lt(),
            Cmp::Le => ordering.is_le(),
            Cmp::Gt => ordering.is_gt(),
            Cmp::Ge => ordering.is_ge(),
        }
    }

    fn symbol(self) -> &'static str {
        match self {
            Cmp::Lt => "<",
            Cmp::Le => "<=",
            Cmp::Gt => ">",
            Cmp::Ge => ">=",
        }
    }
}

impl Literal {
    fn word(text: String) -> Literal {
        let number = if is_number(&text) {
            text.parse().ok()
        } else {
            None
        };
        Literal { text, number }
    }

    fn equals(&self, field: &Field) -> bool {
        match (self.number, field_number(field)) {
            (Some(want), Some(have)) => have == want,
            _ => field.value == self.text,
        }
    }

    fn order(&self, field: &Field) -> Option<Ordering> {
        match self.number {
            Some(want) => field_number(field)?.partial_cmp(&want),
            None => Some(field.value.as_str().cmp(self.text.as_str())),
        }
    }
}

fn field_number(field: &Field) -> Option<f64> {
    use super::ValueKind;
    match field.kind {
        ValueKind::Number | ValueKind::String if is_number(&field.value) => {
            field.value.parse().ok()
        }
        _ => None,
    }
}

/// `-?digits[.digits][(e|E)[+-]digits]`. Stricter than `f64::from_str`, which
/// would also take `inf`, `NaN` and `+1`.
fn is_number(text: &str) -> bool {
    fn digits(bytes: &[u8], mut i: usize) -> Option<usize> {
        let start = i;
        while bytes.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
        (i > start).then_some(i)
    }
    let bytes = text.as_bytes();
    let sign = usize::from(bytes.first() == Some(&b'-'));
    let Some(mut i) = digits(bytes, sign) else {
        return false;
    };
    if bytes.get(i) == Some(&b'.') {
        let Some(end) = digits(bytes, i + 1) else {
            return false;
        };
        i = end;
    }
    if matches!(bytes.get(i), Some(b'e' | b'E')) {
        let sign = usize::from(matches!(bytes.get(i + 1), Some(b'+' | b'-')));
        let Some(end) = digits(bytes, i + 1 + sign) else {
            return false;
        };
        i = end;
    }
    i == bytes.len()
}

// ---------------------------------------------------------------------------
// Lexer

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    LParen,
    RParen,
    Comma,
    Question,
    /// `!` on its own.
    Bang,
    AndAnd,
    OrOr,
    Op(OpTok),
    Word(String),
    Str(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OpTok {
    Eq,
    Ne,
    Contains,
    NotContains,
    Regex,
    NotRegex,
    Lt,
    Le,
    Gt,
    Ge,
}

/// Longest first, so `!~~=` is not read as `!` `~~=`.
const OPERATORS: [(&str, OpTok); 10] = [
    ("!~~=", OpTok::NotRegex),
    ("~~=", OpTok::Regex),
    ("!~=", OpTok::NotContains),
    ("~=", OpTok::Contains),
    ("!=", OpTok::Ne),
    ("<=", OpTok::Le),
    (">=", OpTok::Ge),
    ("=", OpTok::Eq),
    ("<", OpTok::Lt),
    (">", OpTok::Gt),
];

#[derive(Clone, Debug)]
struct Token {
    tok: Tok,
    span: Range<usize>,
}

fn lex(source: &str) -> Result<Vec<Token>, QueryError> {
    let mut tokens = Vec::new();
    let mut i = 0;
    while let Some(c) = source[i..].chars().next() {
        if c.is_whitespace() {
            i += c.len_utf8();
            continue;
        }
        let rest = &source[i..];
        let (tok, end) = match c {
            '(' => (Tok::LParen, i + 1),
            ')' => (Tok::RParen, i + 1),
            ',' => (Tok::Comma, i + 1),
            '?' => (Tok::Question, i + 1),
            '"' | '\'' => {
                let (text, end) = lex_string(source, i, c)?;
                (Tok::Str(text), end)
            }
            _ => {
                if let Some((text, op)) = OPERATORS.iter().find(|(text, _)| rest.starts_with(text))
                {
                    (Tok::Op(*op), i + text.len())
                } else if rest.starts_with("&&") {
                    (Tok::AndAnd, i + 2)
                } else if rest.starts_with("||") {
                    (Tok::OrOr, i + 2)
                } else if c == '!' {
                    (Tok::Bang, i + 1)
                } else {
                    let end = word_end(source, i);
                    if end == i {
                        let span = i..i + c.len_utf8();
                        let message = if c == '~' {
                            "Unexpected `~`: use `~=` for contains or `~~=` for a regular expression"
                                .to_string()
                        } else {
                            format!("Unexpected `{c}`")
                        };
                        return Err(QueryError::new(span, message));
                    }
                    (Tok::Word(source[i..end].to_string()), end)
                }
            }
        };
        tokens.push(Token { tok, span: i..end });
        i = end;
    }
    Ok(tokens)
}

fn word_end(source: &str, start: usize) -> usize {
    let mut chars = source[start..].char_indices().peekable();
    while let Some((offset, c)) = chars.next() {
        let next = chars.peek().map(|(_, n)| *n);
        let stop = match c {
            '(' | ')' | ',' | '=' | '<' | '>' | '~' => true,
            '!' => matches!(next, Some('=' | '~')),
            '?' => next.is_none_or(|n| n.is_whitespace() || "=!<>~()".contains(n)),
            '&' => next == Some('&'),
            '|' => next == Some('|'),
            c => c.is_whitespace(),
        };
        if stop {
            return start + offset;
        }
    }
    source.len()
}

/// `start` is the opening quote. Returns the decoded text and the byte after
/// the closing quote.
fn lex_string(source: &str, start: usize, quote: char) -> Result<(String, usize), QueryError> {
    let unterminated = || {
        QueryError::new(
            start..source.len(),
            "Unterminated string: add a closing quote",
        )
    };
    let mut text = String::new();
    let mut i = start + 1;
    loop {
        let c = source[i..].chars().next().ok_or_else(unterminated)?;
        if c == quote {
            return Ok((text, i + 1));
        }
        if c != '\\' {
            text.push(c);
            i += c.len_utf8();
            continue;
        }
        let escape = source[i + 1..].chars().next().ok_or_else(unterminated)?;
        let end = i + 1 + escape.len_utf8();
        match escape {
            '"' | '\'' | '\\' | '/' => text.push(escape),
            'b' => text.push('\u{8}'),
            'f' => text.push('\u{c}'),
            'n' => text.push('\n'),
            'r' => text.push('\r'),
            't' => text.push('\t'),
            'u' => {
                let (c, after) = unicode_escape(source, i)?;
                text.push(c);
                i = after;
                continue;
            }
            _ => {
                return Err(QueryError::new(
                    i..end,
                    format!("Unknown escape `\\{escape}`: write `\\\\` for a backslash"),
                ))
            }
        }
        i = end;
    }
}

/// `at` is the backslash of `\uXXXX`; a high surrogate must be followed by
/// a low one.
fn unicode_escape(source: &str, at: usize) -> Result<(char, usize), QueryError> {
    let hex4 = |from: usize| {
        let digits = source.get(from..from + 4)?;
        digits
            .bytes()
            .all(|b| b.is_ascii_hexdigit())
            .then(|| u32::from_str_radix(digits, 16).ok())
            .flatten()
    };
    let unit = hex4(at + 2).ok_or_else(|| {
        QueryError::new(at..at + 2, "Invalid `\\u` escape: expected four hex digits")
    })?;
    let unpaired = || QueryError::new(at..at + 6, "Unpaired surrogate in `\\u` escape");
    let (code, end) = match unit {
        0xD800..=0xDBFF => {
            let low = source
                .get(at + 6..at + 8)
                .filter(|s| *s == "\\u")
                .and_then(|_| hex4(at + 8))
                .filter(|low| (0xDC00..=0xDFFF).contains(low))
                .ok_or_else(unpaired)?;
            (0x10000 + ((unit - 0xD800) << 10) + (low - 0xDC00), at + 12)
        }
        0xDC00..=0xDFFF => return Err(unpaired()),
        _ => (unit, at + 6),
    };
    let c = char::from_u32(code).ok_or_else(unpaired)?;
    Ok((c, end))
}

// ---------------------------------------------------------------------------
// Parser

struct Parser<'s> {
    source: &'s str,
    tokens: Vec<Token>,
    pos: usize,
    depth: usize,
}

enum OpKind {
    Op(OpTok),
    In,
    NotIn,
}

fn is_keyword(token: Option<&Token>, keyword: &str) -> bool {
    matches!(token, Some(Token { tok: Tok::Word(word), .. }) if word.eq_ignore_ascii_case(keyword))
}

impl Parser<'_> {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn bump(&mut self) -> Option<Token> {
        let token = self.tokens.get(self.pos).cloned();
        self.pos += usize::from(token.is_some());
        token
    }

    fn end(&self) -> Range<usize> {
        self.source.len()..self.source.len()
    }

    fn text(&self, span: &Range<usize>) -> &str {
        &self.source[span.clone()]
    }

    fn enter(&mut self, span: &Range<usize>) -> Result<(), QueryError> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(QueryError::new(span.clone(), "Query is nested too deeply"));
        }
        Ok(())
    }

    /// A token where only `and`, `or` or the end could follow a condition.
    fn after_condition(&self, token: Token) -> QueryError {
        match token.tok {
            Tok::RParen => QueryError::new(token.span, "Unexpected `)`"),
            Tok::Word(_) | Tok::Str(_) | Tok::LParen | Tok::Bang => {
                QueryError::new(token.span, "Expected `and` or `or` before this condition")
            }
            _ => {
                let message = format!("Unexpected `{}`", self.text(&token.span));
                QueryError::new(token.span, message)
            }
        }
    }

    fn or(&mut self) -> Result<Expr, QueryError> {
        let mut items = vec![self.and()?];
        while matches!(self.peek(), Some(Token { tok: Tok::OrOr, .. }))
            || is_keyword(self.peek(), "or")
        {
            self.bump();
            items.push(self.and()?);
        }
        Ok(if items.len() == 1 {
            items.pop().unwrap()
        } else {
            Expr::Or(items)
        })
    }

    fn and(&mut self) -> Result<Expr, QueryError> {
        let mut items = vec![self.unary()?];
        while matches!(
            self.peek(),
            Some(Token {
                tok: Tok::AndAnd,
                ..
            })
        ) || is_keyword(self.peek(), "and")
        {
            self.bump();
            items.push(self.unary()?);
        }
        Ok(if items.len() == 1 {
            items.pop().unwrap()
        } else {
            Expr::And(items)
        })
    }

    fn unary(&mut self) -> Result<Expr, QueryError> {
        let negation = match self.peek() {
            Some(Token { tok: Tok::Bang, .. }) => true,
            // `not = 1` tests a field named `not`.
            token if is_keyword(token, "not") => !matches!(
                self.tokens.get(self.pos + 1),
                Some(Token {
                    tok: Tok::Op(_) | Tok::Question,
                    ..
                })
            ),
            _ => false,
        };
        if !negation {
            return self.primary();
        }
        let token = self.bump().unwrap();
        self.enter(&token.span)?;
        let inner = self.unary()?;
        self.depth -= 1;
        Ok(Expr::Not(Box::new(inner)))
    }

    fn primary(&mut self) -> Result<Expr, QueryError> {
        let Some(token) = self.bump() else {
            return Err(QueryError::new(self.end(), "Expected a condition"));
        };
        match &token.tok {
            Tok::LParen => {
                self.enter(&token.span)?;
                let inner = self.or()?;
                self.depth -= 1;
                match self.bump() {
                    Some(Token {
                        tok: Tok::RParen, ..
                    }) => Ok(inner),
                    Some(other) => Err(self.after_condition(other)),
                    None => Err(QueryError::new(token.span, "Unclosed parenthesis: add `)`")),
                }
            }
            Tok::Word(word)
                if word.eq_ignore_ascii_case("exists")
                    && matches!(
                        self.peek(),
                        Some(Token {
                            tok: Tok::LParen,
                            ..
                        })
                    ) =>
            {
                let open = self.bump().unwrap();
                let name = self
                    .bump()
                    .ok_or_else(|| QueryError::new(self.end(), "Expected a field name"))?;
                let field = self.field(name)?;
                match self.bump() {
                    Some(Token {
                        tok: Tok::RParen, ..
                    }) => Ok(Expr::Exists(field)),
                    Some(other) => Err(QueryError::new(
                        other.span,
                        "Expected `)` after the field name",
                    )),
                    None => Err(QueryError::new(open.span, "Unclosed parenthesis: add `)`")),
                }
            }
            Tok::Word(_) | Tok::Str(_) => self.test(token),
            Tok::RParen => Err(QueryError::new(token.span, "Unexpected `)`")),
            _ => {
                let message = format!("Expected a condition, found `{}`", self.text(&token.span));
                Err(QueryError::new(token.span, message))
            }
        }
    }

    fn field(&self, token: Token) -> Result<FieldRef, QueryError> {
        match token.tok {
            Tok::Word(word) => Ok(match word.as_str() {
                "level" => FieldRef::Level,
                "msg" | "message" => FieldRef::Message,
                "logger" => FieldRef::Logger,
                _ => match word.strip_prefix('.') {
                    Some("") => {
                        return Err(QueryError::new(
                            token.span,
                            "Expected a field name after `.`",
                        ))
                    }
                    Some(key) => FieldRef::Key(key.to_string()),
                    None => FieldRef::Key(word),
                },
            }),
            Tok::Str(key) if key.is_empty() => {
                Err(QueryError::new(token.span, "A field name cannot be empty"))
            }
            Tok::Str(key) => Ok(FieldRef::Key(key)),
            _ => {
                let message = format!("Expected a field name, found `{}`", self.text(&token.span));
                Err(QueryError::new(token.span, message))
            }
        }
    }

    fn test(&mut self, name: Token) -> Result<Expr, QueryError> {
        let name_span = name.span.clone();
        let field = self.field(name)?;
        let include_absent = matches!(
            self.peek(),
            Some(Token {
                tok: Tok::Question,
                ..
            })
        );
        if include_absent {
            self.bump();
        }
        let missing_operator = |parser: &Self, span: Range<usize>| {
            let name = parser.text(&name_span);
            if name.eq_ignore_ascii_case("and") || name.eq_ignore_ascii_case("or") {
                let message = format!("Expected a condition before `{name}`");
                QueryError::new(name_span.clone(), message)
            } else {
                let message =
                    format!("Expected an operator after `{name}`: = != ~= ~~= < <= > >= or in");
                QueryError::new(span, message)
            }
        };
        let Some(op) = self.bump() else {
            return Err(missing_operator(self, self.end()));
        };
        let kind = match &op.tok {
            Tok::Op(op) => OpKind::Op(*op),
            Tok::Word(word) if word.eq_ignore_ascii_case("in") => OpKind::In,
            Tok::Word(word) if word.eq_ignore_ascii_case("not") => {
                if is_keyword(self.peek(), "in") {
                    self.bump();
                    OpKind::NotIn
                } else {
                    let span = self.peek().map_or(self.end(), |t| t.span.clone());
                    return Err(QueryError::new(span, "Expected `in` after `not`"));
                }
            }
            _ => return Err(missing_operator(self, op.span)),
        };
        if field == FieldRef::Level {
            let test = match kind {
                OpKind::Op(OpTok::Eq) => LevelTest::Eq(self.level(&op.span)?),
                OpKind::Op(OpTok::Ne) => LevelTest::Ne(self.level(&op.span)?),
                OpKind::Op(OpTok::Lt) => LevelTest::Compare(Cmp::Lt, self.level(&op.span)?),
                OpKind::Op(OpTok::Le) => LevelTest::Compare(Cmp::Le, self.level(&op.span)?),
                OpKind::Op(OpTok::Gt) => LevelTest::Compare(Cmp::Gt, self.level(&op.span)?),
                OpKind::Op(OpTok::Ge) => LevelTest::Compare(Cmp::Ge, self.level(&op.span)?),
                OpKind::In => LevelTest::In(self.list(|p, after| p.level(after))?),
                OpKind::NotIn => LevelTest::NotIn(self.list(|p, after| p.level(after))?),
                OpKind::Op(_) => {
                    return Err(QueryError::new(
                        op.span,
                        "`level` compares with = != < <= > >= or in",
                    ))
                }
            };
            return Ok(Expr::Level {
                test,
                include_absent,
            });
        }
        let test = match kind {
            OpKind::Op(OpTok::Eq) => Test::Eq(self.value(&op.span)?.0),
            OpKind::Op(OpTok::Ne) => Test::Ne(self.value(&op.span)?.0),
            OpKind::Op(OpTok::Contains) => Test::Contains(self.value(&op.span)?.0.text),
            OpKind::Op(OpTok::NotContains) => Test::NotContains(self.value(&op.span)?.0.text),
            OpKind::Op(OpTok::Regex) => Test::Regex(self.pattern(&op.span)?),
            OpKind::Op(OpTok::NotRegex) => Test::NotRegex(self.pattern(&op.span)?),
            OpKind::Op(OpTok::Lt) => Test::Compare(Cmp::Lt, self.value(&op.span)?.0),
            OpKind::Op(OpTok::Le) => Test::Compare(Cmp::Le, self.value(&op.span)?.0),
            OpKind::Op(OpTok::Gt) => Test::Compare(Cmp::Gt, self.value(&op.span)?.0),
            OpKind::Op(OpTok::Ge) => Test::Compare(Cmp::Ge, self.value(&op.span)?.0),
            OpKind::In => Test::In(self.list(|p, after| Ok(p.value(after)?.0))?),
            OpKind::NotIn => Test::NotIn(self.list(|p, after| Ok(p.value(after)?.0))?),
        };
        Ok(Expr::Field {
            field,
            test,
            include_absent,
        })
    }

    /// `after` is the token before the value, named when the query ends.
    fn value(&mut self, after: &Range<usize>) -> Result<(Literal, Range<usize>), QueryError> {
        match self.bump() {
            Some(Token {
                tok: Tok::Word(word),
                span,
            }) => Ok((Literal::word(word), span)),
            Some(Token {
                tok: Tok::Str(text),
                span,
            }) => Ok((Literal { text, number: None }, span)),
            Some(other) => Err(QueryError::new(
                other.span,
                "Expected a value: quote values that contain spaces or ( ) , = < > ~",
            )),
            None => {
                let message = format!("Expected a value after `{}`", self.text(after));
                Err(QueryError::new(self.end(), message))
            }
        }
    }

    fn level(&mut self, after: &Range<usize>) -> Result<Level, QueryError> {
        let (literal, span) = self.value(after)?;
        let level = if literal.number.is_some() {
            Level::from_number(literal.text.as_bytes())
        } else {
            Level::from_text(&literal.text)
        };
        if !level.is_severity() {
            let message = format!(
                "Unknown level `{}`: use trace, debug, info, warn, error or fatal",
                literal.text
            );
            return Err(QueryError::new(span, message));
        }
        Ok(level)
    }

    fn pattern(&mut self, after: &Range<usize>) -> Result<Pattern, QueryError> {
        let (literal, span) = self.value(after)?;
        RegexBuilder::new(&literal.text)
            .size_limit(REGEX_SIZE_LIMIT)
            .build()
            .map(Pattern)
            .map_err(|err| {
                let detail = match &err {
                    // The syntax error text is a multi-line diagram; keep its
                    // final `error: …` line, which is the reason.
                    regex::Error::Syntax(text) => text
                        .lines()
                        .rev()
                        .find_map(|line| line.trim().strip_prefix("error: "))
                        .unwrap_or("syntax error")
                        .to_string(),
                    regex::Error::CompiledTooBig(_) => "the pattern is too large".to_string(),
                    other => other.to_string(),
                };
                QueryError::new(span, format!("Invalid regular expression: {detail}"))
            })
    }

    /// `( item { , item } )` after `in`.
    fn list<T>(
        &mut self,
        mut item: impl FnMut(&mut Self, &Range<usize>) -> Result<T, QueryError>,
    ) -> Result<Vec<T>, QueryError> {
        let open = match self.bump() {
            Some(
                token @ Token {
                    tok: Tok::LParen, ..
                },
            ) => token,
            Some(other) => return Err(QueryError::new(other.span, "Expected `(` after `in`")),
            None => return Err(QueryError::new(self.end(), "Expected `(` after `in`")),
        };
        if let Some(Token {
            tok: Tok::RParen,
            span,
        }) = self.peek()
        {
            return Err(QueryError::new(span.clone(), "Expected at least one value"));
        }
        let mut items = Vec::new();
        let mut after = open.span.clone();
        loop {
            items.push(item(self, &after)?);
            match self.bump() {
                Some(Token {
                    tok: Tok::Comma,
                    span,
                }) => after = span,
                Some(Token {
                    tok: Tok::RParen, ..
                }) => return Ok(items),
                Some(other) => return Err(QueryError::new(other.span, "Expected `,` or `)`")),
                None => return Err(QueryError::new(open.span, "Unclosed parenthesis: add `)`")),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Canonical text

/// True when `text` lexes back as exactly this one word.
fn is_plain_word(text: &str) -> bool {
    matches!(lex(text).as_deref(), Ok([Token { tok: Tok::Word(word), .. }]) if word == text)
}

fn write_quoted(f: &mut fmt::Formatter<'_>, text: &str) -> fmt::Result {
    f.write_char('"')?;
    for c in text.chars() {
        match c {
            '"' => f.write_str("\\\"")?,
            '\\' => f.write_str("\\\\")?,
            '\n' => f.write_str("\\n")?,
            '\r' => f.write_str("\\r")?,
            '\t' => f.write_str("\\t")?,
            c if c.is_control() => write!(f, "\\u{:04x}", u32::from(c))?,
            c => f.write_char(c)?,
        }
    }
    f.write_char('"')
}

fn level_name(level: Level) -> &'static str {
    match level {
        Level::Trace => "trace",
        Level::Debug => "debug",
        Level::Info => "info",
        Level::Warn => "warn",
        Level::Error => "error",
        Level::Fatal => "fatal",
        Level::Unknown | Level::Missing | Level::Unparsed => "unknown",
    }
}

impl fmt::Display for FieldRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FieldRef::Level => f.write_str("level"),
            FieldRef::Message => f.write_str("msg"),
            FieldRef::Logger => f.write_str("logger"),
            FieldRef::Key(key)
                if is_plain_word(key)
                    && !key.starts_with('.')
                    && !RESERVED.iter().any(|r| r.eq_ignore_ascii_case(key)) =>
            {
                f.write_str(key)
            }
            FieldRef::Key(key) => write_quoted(f, key),
        }
    }
}

impl fmt::Display for Literal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.number.is_some() || (is_plain_word(&self.text) && !is_number(&self.text)) {
            f.write_str(&self.text)
        } else {
            write_quoted(f, &self.text)
        }
    }
}

fn write_list<T>(
    f: &mut fmt::Formatter<'_>,
    items: &[T],
    mut item: impl FnMut(&mut fmt::Formatter<'_>, &T) -> fmt::Result,
) -> fmt::Result {
    f.write_char('(')?;
    for (i, value) in items.iter().enumerate() {
        if i > 0 {
            f.write_str(", ")?;
        }
        item(f, value)?;
    }
    f.write_char(')')
}

impl fmt::Display for Expr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let child = |f: &mut fmt::Formatter<'_>, expr: &Expr| match expr {
            Expr::And(_) | Expr::Or(_) => write!(f, "({expr})"),
            _ => write!(f, "{expr}"),
        };
        let absent = |include: bool| if include { "?" } else { "" };
        match self {
            Expr::And(items) | Expr::Or(items) => {
                let joiner = if matches!(self, Expr::And(_)) {
                    " and "
                } else {
                    " or "
                };
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        f.write_str(joiner)?;
                    }
                    child(f, item)?;
                }
                Ok(())
            }
            Expr::Not(inner) => {
                f.write_str("not ")?;
                child(f, inner)
            }
            Expr::Exists(field) => write!(f, "exists({field})"),
            Expr::Level {
                test,
                include_absent,
            } => {
                write!(f, "level{} ", absent(*include_absent))?;
                match test {
                    LevelTest::Eq(level) => write!(f, "= {}", level_name(*level)),
                    LevelTest::Ne(level) => write!(f, "!= {}", level_name(*level)),
                    LevelTest::Compare(cmp, level) => {
                        write!(f, "{} {}", cmp.symbol(), level_name(*level))
                    }
                    LevelTest::In(levels) | LevelTest::NotIn(levels) => {
                        if matches!(test, LevelTest::NotIn(_)) {
                            f.write_str("not ")?;
                        }
                        f.write_str("in ")?;
                        write_list(f, levels, |f, level| f.write_str(level_name(*level)))
                    }
                }
            }
            Expr::Field {
                field,
                test,
                include_absent,
            } => {
                write!(f, "{field}{} ", absent(*include_absent))?;
                match test {
                    Test::Eq(lit) => write!(f, "= {lit}"),
                    Test::Ne(lit) => write!(f, "!= {lit}"),
                    Test::Contains(text) => {
                        f.write_str("~= ")?;
                        write_quoted(f, text)
                    }
                    Test::NotContains(text) => {
                        f.write_str("!~= ")?;
                        write_quoted(f, text)
                    }
                    Test::Regex(pattern) => {
                        f.write_str("~~= ")?;
                        write_quoted(f, pattern.as_str())
                    }
                    Test::NotRegex(pattern) => {
                        f.write_str("!~~= ")?;
                        write_quoted(f, pattern.as_str())
                    }
                    Test::Compare(cmp, lit) => write!(f, "{} {lit}", cmp.symbol()),
                    Test::In(lits) => {
                        f.write_str("in ")?;
                        write_list(f, lits, |f, lit| write!(f, "{lit}"))
                    }
                    Test::NotIn(lits) => {
                        f.write_str("not in ")?;
                        write_list(f, lits, |f, lit| write!(f, "{lit}"))
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;

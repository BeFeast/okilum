use super::*;
use crate::log::LogIndex;

fn parse(source: &str) -> Query {
    Query::parse(source).unwrap_or_else(|err| panic!("{source:?}: {err} at {:?}", err.span))
}

fn canonical(source: &str) -> String {
    parse(source).to_string()
}

fn error(source: &str) -> QueryError {
    match Query::parse(source) {
        Ok(query) => panic!("{source:?} parsed as {query}"),
        Err(err) => err,
    }
}

struct Fixture {
    data: &'static [u8],
    index: LogIndex,
}

impl Fixture {
    fn new(data: &'static [u8]) -> Self {
        Fixture {
            data,
            index: LogIndex::build_with_threads(data, 1),
        }
    }

    fn records(&self) -> Vec<(LogEntry, Option<Record>)> {
        (0..self.index.len())
            .map(|i| {
                let line = self.index.raw(self.data, i).unwrap();
                (
                    self.index.entries()[i],
                    Record::parse(line, self.index.format()),
                )
            })
            .collect()
    }

    /// One-based line numbers of the entries `source` matches.
    fn hits(&self, source: &str) -> Vec<u64> {
        let query = parse(source);
        (0..self.index.len())
            .filter(|&i| {
                let line = self.index.raw(self.data, i).unwrap();
                query.matches_line(&self.index.entries()[i], line, self.index.format())
            })
            .map(|i| self.index.entries()[i].line())
            .collect()
    }
}

const SERVICE_JSONL: &[u8] = include_bytes!("../../../tests/fixtures/log/service.jsonl");
const SERVICE_LOGFMT: &[u8] = include_bytes!("../../../tests/fixtures/log/service.logfmt");
const DIAGNOSTIC: &[u8] = include_bytes!("../../../tests/fixtures/log/reader-diagnostic.log");

#[test]
fn every_operator_parses_to_its_test() {
    let field = |source: &str| match parse(source).expr {
        Expr::Field {
            field,
            test,
            include_absent,
        } => (field, test, include_absent),
        other => panic!("{source}: {other:?}"),
    };
    let key = |k: &str| FieldRef::Key(k.to_string());
    let lit = |t: &str| Literal::word(t.to_string());
    let text = |t: &str| Literal {
        text: t.to_string(),
        number: None,
    };
    assert_eq!(
        field("status=500"),
        (key("status"), Test::Eq(lit("500")), false)
    );
    assert_eq!(lit("500").number, Some(500.0));
    assert_eq!(field("status != \"500\"").1, Test::Ne(text("500")));
    assert_eq!(field("msg~=boom").1, Test::Contains("boom".into()));
    assert_eq!(field("msg !~= boom").1, Test::NotContains("boom".into()));
    assert!(matches!(field("msg~~=\"^b.+m$\"").1, Test::Regex(p) if p.as_str() == "^b.+m$"));
    assert!(matches!(field("msg!~~=x").1, Test::NotRegex(p) if p.as_str() == "x"));
    assert_eq!(field("n<-1.5e3").1, Test::Compare(Cmp::Lt, lit("-1.5e3")));
    assert_eq!(field("n<=1").1, Test::Compare(Cmp::Le, lit("1")));
    assert_eq!(field("n>1").1, Test::Compare(Cmp::Gt, lit("1")));
    assert_eq!(field("n >= 1").1, Test::Compare(Cmp::Ge, lit("1")));
    assert_eq!(
        field("logger in (http, \"db pool\")"),
        (
            FieldRef::Logger,
            Test::In(vec![lit("http"), text("db pool")]),
            false
        )
    );
    assert_eq!(field("message NOT IN (a)").1, Test::NotIn(vec![lit("a")]));
    assert_eq!(field("message=a").0, FieldRef::Message);
    assert_eq!(field("user.id?>2000").0, key("user.id"));
    assert!(field("user.id?>2000").2);
    assert_eq!(field(".level=NOTICE").0, key("level"));
    assert_eq!(field("\"level\"=NOTICE").0, key("level"));
    assert_eq!(field("@timestamp>x").0, key("@timestamp"));
    assert_eq!(
        field("Level=x").0,
        key("Level"),
        "only lowercase is the role"
    );
    // Numbers are recognised strictly; everything else stays text.
    for word in [
        "inf", "NaN", "+1", "1.", ".5", "1e", "0x10", "97c04822", "1-2",
    ] {
        assert_eq!(lit(word).number, None, "{word}");
    }
    for word in ["0", "-0", "007", "4.10", "1E+9", "2e-3"] {
        assert!(lit(word).number.is_some(), "{word}");
    }
}

#[test]
fn level_shortcuts() {
    let level = |source: &str| match parse(source).expr {
        Expr::Level {
            test,
            include_absent,
        } => (test, include_absent),
        other => panic!("{source}: {other:?}"),
    };
    assert_eq!(
        level("level>=warn"),
        (LevelTest::Compare(Cmp::Ge, Level::Warn), false)
    );
    assert_eq!(level("level = ERR").0, LevelTest::Eq(Level::Error));
    assert_eq!(level("level!=Warning").0, LevelTest::Ne(Level::Warn));
    assert_eq!(
        level("level<notice").0,
        LevelTest::Compare(Cmp::Lt, Level::Info)
    );
    assert_eq!(
        level("level>40").0,
        LevelTest::Compare(Cmp::Gt, Level::Warn)
    );
    assert_eq!(
        level("level? in (debug, \"trace\")"),
        (LevelTest::In(vec![Level::Debug, Level::Trace]), true)
    );
    assert_eq!(
        level("level not in (fatal)").0,
        LevelTest::NotIn(vec![Level::Fatal])
    );
    assert_eq!(parse("exists(level)").expr, Expr::Exists(FieldRef::Level));
    assert!(!parse("level>=warn or not exists(level)").needs_record());
    assert!(parse("level>=warn and msg~=x").needs_record());
    assert!(parse("exists(x)").needs_record());
}

#[test]
fn precedence_and_grouping() {
    assert_eq!(
        canonical("a=1 or b=2 and c=3"),
        "a = 1 or (b = 2 and c = 3)"
    );
    assert_eq!(
        canonical("a=1 and b=2 or c=3"),
        "(a = 1 and b = 2) or c = 3"
    );
    assert_eq!(
        canonical("(a=1 or b=2) and c=3"),
        "(a = 1 or b = 2) and c = 3"
    );
    assert_eq!(
        canonical("a=1 || b=2 && !c=3"),
        "a = 1 or (b = 2 and not c = 3)"
    );
    assert_eq!(
        canonical("A=1 AND NOT b=2 Or c=3"),
        "(A = 1 and not b = 2) or c = 3"
    );
    // `not` binds tighter than `and`.
    let Expr::And(items) = parse("not a=1 and b=2").expr else {
        panic!()
    };
    assert!(matches!(items[0], Expr::Not(_)));
    assert_eq!(canonical("not (a=1 and b=2)"), "not (a = 1 and b = 2)");
    assert_eq!(canonical("not not a=1"), "not not a = 1");
    assert_eq!(
        canonical("a=1 and b=2 and c=3"),
        "a = 1 and b = 2 and c = 3"
    );
    // Explicit nesting is kept, not flattened.
    assert_eq!(
        canonical("a=1 and (b=2 and c=3)"),
        "a = 1 and (b = 2 and c = 3)"
    );
    assert_eq!(canonical("((a=1))"), "a = 1");
    // Keywords are only keywords where one can stand.
    assert_eq!(canonical("not = 1"), "\"not\" = 1");
    assert_eq!(
        canonical("msg = and or in != or"),
        "msg = and or \"in\" != or"
    );
    assert_eq!(canonical("exists = 1"), "\"exists\" = 1");
    assert!(Query::parse("").unwrap().is_empty());
    assert!(Query::parse(" \t ").unwrap().is_empty());
    assert_eq!(canonical(""), "");
}

#[test]
fn quoting_escapes_and_unicode() {
    let value = |source: &str| match parse(source).expr {
        Expr::Field {
            test: Test::Eq(lit),
            ..
        } => lit.text,
        other => panic!("{source}: {other:?}"),
    };
    assert_eq!(
        value(r#"msg="line with \"quotes\" and é""#),
        "line with \"quotes\" and é"
    );
    assert_eq!(value(r"msg='it\'s'"), "it's");
    assert_eq!(value(r#"msg='say "hi"'"#), "say \"hi\"");
    assert_eq!(
        value(r#"msg="a\\b\/c\n\t\r\b\f""#),
        "a\\b/c\n\t\r\u{8}\u{c}"
    );
    assert_eq!(value(r#"msg="éé""#), "éé");
    assert_eq!(value(r#"msg="😀""#), "😀");
    assert_eq!(value("msg=don't"), "don't");
    assert_eq!(value("msg=héllo"), "héllo");
    assert_eq!(value("ключ=значение"), "значение");
    assert_eq!(value("path=/v1/notes/a?x"), "/v1/notes/a?x");
    assert_eq!(value("q=a&b|c!d"), "a&b|c!d");
    assert_eq!(value("msg=\"\""), "");
    assert_eq!(
        parse("\"weird key\" = 1").expr,
        Expr::Field {
            field: FieldRef::Key("weird key".into()),
            test: Test::Eq(Literal::word("1".into())),
            include_absent: false,
        }
    );
    assert_eq!(parse("ключ=1").to_string(), "ключ = 1");
    // `&&`, `||` and an operator end a word without spaces.
    assert_eq!(canonical("a=1&&b=2||c=3"), "(a = 1 and b = 2) or c = 3");
    assert_eq!(canonical("a!=1"), "a != 1");
    assert_eq!(canonical("a?!=1"), "a? != 1");
    // The canonical form quotes what a bare word would change.
    assert_eq!(
        canonical(r#"msg="tab\there" or msg="500" or msg="a b" or msg="(""#),
        r#"msg = "tab\there" or msg = "500" or msg = "a b" or msg = "(""#
    );
    // A quote inside a word is part of it; a leading dot would be read as
    // the raw-key prefix, so such a key is quoted.
    assert_eq!(canonical(r#""a\"b"=1 or ".x"=1"#), r#"a"b = 1 or ".x" = 1"#);
}

#[test]
fn errors_carry_a_span_and_a_message() {
    let cases: &[(&str, Range<usize>, &str)] = &[
        ("status >=", 9..9, "Expected a value after `>=`"),
        ("status 500", 7..10, "Expected an operator after `status`"),
        ("status", 6..6, "Expected an operator after `status`"),
        ("(a=1", 0..1, "Unclosed parenthesis"),
        ("a=1)", 3..4, "Unexpected `)`"),
        (
            "a=1 b=2",
            4..5,
            "Expected `and` or `or` before this condition",
        ),
        (
            "(a=1 b=2)",
            5..6,
            "Expected `and` or `or` before this condition",
        ),
        ("msg=\"abc", 4..8, "Unterminated string"),
        ("msg=\"abc\\", 4..9, "Unterminated string"),
        (r#"msg="a\qb""#, 6..8, "Unknown escape `\\q`"),
        (r#"msg="\u12""#, 5..7, "Invalid `\\u` escape"),
        (r#"msg="\ud800""#, 5..11, "Unpaired surrogate"),
        (r#"msg="\udc00""#, 5..11, "Unpaired surrogate"),
        ("level>=loud", 7..11, "Unknown level `loud`"),
        ("level=5", 6..7, "Unknown level `5`"),
        ("level~=warn", 5..7, "`level` compares with"),
        (
            "msg~~=\"(\"",
            6..9,
            "Invalid regular expression: unclosed group",
        ),
        ("a in ()", 6..7, "Expected at least one value"),
        ("a in (1, 2", 5..6, "Unclosed parenthesis"),
        ("a in (1 2)", 8..9, "Expected `,` or `)`"),
        ("a in (1,)", 8..9, "Expected a value"),
        ("a in 1", 5..6, "Expected `(` after `in`"),
        ("a not b", 6..7, "Expected `in` after `not`"),
        ("a not", 5..5, "Expected `in` after `not`"),
        ("~ a", 0..1, "Unexpected `~`: use `~=`"),
        ("exists(a", 6..7, "Unclosed parenthesis"),
        ("exists(a b)", 9..10, "Expected `)` after the field name"),
        ("and a=1", 0..3, "Expected a condition before `and`"),
        ("a=1 and", 7..7, "Expected a condition"),
        ("a=1 or )", 7..8, "Unexpected `)`"),
        ("a=1 && || b=2", 7..9, "Expected a condition, found `||`"),
        ("a=(", 2..3, "Expected a value: quote values"),
        (".=1", 0..1, "Expected a field name after `.`"),
        ("\"\"=1", 0..2, "A field name cannot be empty"),
        ("= 1", 0..1, "Expected a condition, found `=`"),
        ("a=1 ?", 4..5, "Unexpected `?`"),
    ];
    for (source, span, message) in cases {
        let err = error(source);
        assert_eq!(&err.span, span, "{source:?}: {err}");
        assert!(
            err.message.starts_with(message),
            "{source:?}: {:?} does not start with {message:?}",
            err.message
        );
        // Every span can be sliced out of the query for highlighting.
        assert!(source.get(err.span.clone()).is_some(), "{source:?}");
    }
}

#[test]
fn error_columns_count_characters() {
    let source = "\"é\" > ";
    let err = error(source);
    assert_eq!(err.span, 7..7);
    assert_eq!(err.column(source), 6);
    let source = "ключ=значение and ~";
    let err = error(source);
    assert_eq!(err.column(source), 18);
    assert_eq!(&source[err.span.clone()], "~");
    assert_eq!(err.to_string(), err.message);
}

#[test]
fn nesting_is_bounded() {
    let deep = format!("{}a=1{}", "(".repeat(100), ")".repeat(100));
    let err = error(&deep);
    assert_eq!(err.span, MAX_DEPTH..MAX_DEPTH + 1);
    assert!(err.message.contains("nested too deeply"));
    let nots = format!("{}a=1", "not ".repeat(100));
    assert!(error(&nots).message.contains("nested too deeply"));
    let fine = format!("{}a=1{}", "(".repeat(MAX_DEPTH), ")".repeat(MAX_DEPTH));
    assert_eq!(canonical(&fine), "a = 1");
}

#[test]
fn json_records_including_missing_fields() {
    let file = Fixture::new(SERVICE_JSONL);
    // Line 4 is an unparsed panic line, line 6 is blank, line 9 says NOTICE.
    let cases: &[(&str, &[u64])] = &[
        ("", &[1, 2, 3, 4, 5, 7, 8, 9]),
        ("level>=warn", &[1, 2, 8]),
        ("level=info", &[3, 9]),
        ("level in (debug, trace)", &[5, 7]),
        ("level?<info", &[4, 5, 7]),
        ("exists(level)", &[1, 2, 3, 5, 7, 8, 9]),
        ("not exists(level)", &[4]),
        ("status>=500", &[1, 2]),
        ("status=200", &[5]),
        ("status!=500", &[2, 5]),
        ("status?=500", &[1, 3, 4, 7, 8, 9]),
        ("not status=500", &[2, 3, 4, 5, 7, 8, 9]),
        ("status>=\"500\"", &[1, 2]),
        ("exists(request_id)", &[1, 3]),
        ("request_id=97c0482241e0de67", &[1, 3]),
        ("user.id>2000", &[1]),
        ("user.role in (owner, admin)", &[1]),
        ("duration_ms=812.5", &[2]),
        ("duration_ms=\"812.5\"", &[]),
        ("duration_ms=\"812.50\"", &[2]),
        ("tags~=cold", &[3]),
        ("msg~~=\"^re\"", &[2]),
        ("msg!~~=\"^re\"", &[1, 3, 5, 7, 8, 9]),
        (r#"msg="line with \"quotes\" and é""#, &[5]),
        ("message~=é", &[5]),
        ("logger in (http, reader)", &[2, 3, 5]),
        ("logger not in (http)", &[1, 3, 5]),
        ("logger?!=http", &[1, 3, 4, 5, 7, 8, 9]),
        (".level=\"NOTICE\"", &[9]),
        ("\"level\"=fatal", &[8]),
        ("Level=shadowed", &[8]),
        ("ts>=\"2025-10-01T00:00:00.262Z\"", &[5, 7, 8, 9]),
        ("level>=error or (logger=reader and status=200)", &[1, 5, 8]),
        ("(level>=error or logger=reader) and status=200", &[5]),
    ];
    for (source, expected) in cases {
        assert_eq!(&file.hits(source), expected, "{source}");
    }
}

#[test]
fn logfmt_and_level_less_records() {
    let logfmt = Fixture::new(SERVICE_LOGFMT);
    let cases: &[(&str, &[u64])] = &[
        ("retry=true", &[4]),
        ("status=200 and duration_ms>100", &[2]),
        ("duration_ms<5", &[1]),
        ("msg=\"\"", &[5]),
        (r#"msg~="\"timeout\"""#, &[4]),
        ("logger=db", &[4]),
        ("empty_message=yes and level=debug", &[5]),
    ];
    for (source, expected) in cases {
        assert_eq!(&logfmt.hits(source), expected, "{source}");
    }
    // Okilum's own diagnostic log has no level field anywhere: a level
    // filter hides everything unless it includes absent levels.
    let diagnostic = Fixture::new(DIAGNOSTIC);
    let cases: &[(&str, &[u64])] = &[
        ("level>=warn", &[]),
        ("level?>=warn", &[1, 2, 3, 4, 5, 6, 7]),
        ("details.found=false", &[3]),
        ("details.notes>=5000", &[5]),
        ("phase~~=\"(?i)warm\"", &[3]),
        ("vault=null", &[1, 2]),
        ("time>=1759276802", &[5, 6, 7]),
        ("unreadable~=\"Permission denied\"", &[4]),
        ("exists(error)", &[6]),
        ("elapsed_ms>14 and elapsed_ms<200", &[3, 5]),
    ];
    for (source, expected) in cases {
        assert_eq!(&diagnostic.hits(source), expected, "{source}");
    }
}

#[test]
fn duplicate_keys_match_any_occurrence() {
    let data: &'static [u8] = b"tag=a tag=b\n";
    let file = Fixture::new(data);
    assert_eq!(file.hits("tag=b"), [1]);
    assert_eq!(file.hits("tag!=b"), [] as [u64; 0]);
    assert_eq!(file.hits("tag!=c"), [1]);
    assert_eq!(file.hits("tag not in (a, c)"), [] as [u64; 0]);
}

/// A small deterministic generator (xorshift), so the property test needs no
/// new dependency and every failure reproduces.
struct Rng(u64);

impl Rng {
    fn below(&mut self, n: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % n as u64) as usize
    }
}

/// Atoms that each reach at least one fixture entry (checked below).
const ATOMS: &[&str] = &[
    "level>=warn",
    "level=info",
    "level!=error",
    "level in (debug, trace)",
    "level not in (info)",
    "level?<info",
    "status>=500",
    "status=200",
    "status!=500",
    "status?=500",
    "status in (200, 503)",
    "msg~=\"re\"",
    "msg~~=\"^[a-s]\"",
    "message!~=\"o\"",
    "logger=http",
    "logger not in (http, reader)",
    "logger?!=db",
    "request_id=97c0482241e0de67",
    "exists(request_id)",
    "exists(level)",
    "user.id>2000",
    "duration_ms<=812.5",
    "tags~=cold",
    "retry=true",
    "details.found=false",
    "vault~=Notes",
    "phase~~=\"(?i)warm\"",
    ".level=\"NOTICE\"",
    "time>=1759276802",
    "ts>=\"2025-10-01T00:00:00.25\"",
];

/// Equivalences the documented semantics promise.
const EQUIVALENT: &[(&str, &str)] = &[
    ("status!=500", "exists(status) and not status=500"),
    ("status in (200, 503)", "status=200 or status=503"),
    (
        "logger not in (http, reader)",
        "exists(logger) and not (logger=http or logger=reader)",
    ),
    ("level>=warn", "level in (warn, error, fatal)"),
    (
        "level?<info",
        "not exists(level) or level in (trace, debug)",
    ),
    ("status?=500", "not exists(status) or status=500"),
    ("message!~=\"o\"", "exists(msg) and not msg~=\"o\""),
    ("logger?!=db", "not exists(logger) or logger!=db"),
    ("not (a=1 or b=2)", "not a=1 and not b=2"),
];

enum Gen {
    Atom(usize),
    And(Box<Gen>, Box<Gen>),
    Or(Box<Gen>, Box<Gen>),
    Not(Box<Gen>),
}

impl Gen {
    fn random(rng: &mut Rng, depth: usize) -> Gen {
        match if depth == 0 { 0 } else { rng.below(4) } {
            0 => Gen::Atom(rng.below(ATOMS.len())),
            1 => Gen::And(
                Box::new(Gen::random(rng, depth - 1)),
                Box::new(Gen::random(rng, depth - 1)),
            ),
            2 => Gen::Or(
                Box::new(Gen::random(rng, depth - 1)),
                Box::new(Gen::random(rng, depth - 1)),
            ),
            _ => Gen::Not(Box::new(Gen::random(rng, depth - 1))),
        }
    }

    fn text(&self) -> String {
        match self {
            Gen::Atom(i) => ATOMS[*i].to_string(),
            Gen::And(a, b) => format!("({}) and ({})", a.text(), b.text()),
            Gen::Or(a, b) => format!("({}) || ({})", a.text(), b.text()),
            Gen::Not(a) => format!("not ({})", a.text()),
        }
    }

    /// The oracle: boolean structure evaluated here, atoms by the matcher.
    fn eval(&self, atoms: &[bool]) -> bool {
        match self {
            Gen::Atom(i) => atoms[*i],
            Gen::And(a, b) => a.eval(atoms) && b.eval(atoms),
            Gen::Or(a, b) => a.eval(atoms) || b.eval(atoms),
            Gen::Not(a) => !a.eval(atoms),
        }
    }
}

#[test]
fn property_style_over_fixtures() {
    let fixtures = [
        Fixture::new(SERVICE_JSONL),
        Fixture::new(SERVICE_LOGFMT),
        Fixture::new(DIAGNOSTIC),
    ];
    let entries: Vec<(LogEntry, Option<Record>, &[u8], Format)> = fixtures
        .iter()
        .flat_map(|fixture| {
            fixture
                .records()
                .into_iter()
                .enumerate()
                .map(|(i, (entry, record))| {
                    let line = fixture.index.raw(fixture.data, i).unwrap();
                    (entry, record, line, fixture.index.format())
                })
        })
        .collect();
    let atoms: Vec<Query> = ATOMS.iter().map(|a| parse(a)).collect();
    let truth: Vec<Vec<bool>> = entries
        .iter()
        .map(|(entry, record, line, format)| {
            atoms
                .iter()
                .map(|atom| {
                    let hit = atom.matches(entry, record.as_ref());
                    assert_eq!(hit, atom.matches_line(entry, line, *format), "{atom}");
                    hit
                })
                .collect()
        })
        .collect();
    // Positive control: every atom reaches some entry and misses another, so
    // the properties below are not satisfied vacuously.
    for (i, atom) in ATOMS.iter().enumerate() {
        let hits = truth.iter().filter(|row| row[i]).count();
        assert!(hits > 0 && hits < entries.len(), "{atom}: {hits}");
    }

    for (left, right) in EQUIVALENT {
        let (left, right) = (parse(left), parse(right));
        for (entry, record, ..) in &entries {
            assert_eq!(
                left.matches(entry, record.as_ref()),
                right.matches(entry, record.as_ref()),
                "{left} vs {right}"
            );
        }
    }

    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let (mut matched, mut missed) = (0, 0);
    for _ in 0..400 {
        let gen = Gen::random(&mut rng, 4);
        let source = gen.text();
        let query = parse(&source);
        // The canonical form parses back to the same query.
        let printed = query.to_string();
        assert_eq!(parse(&printed), query, "{source} -> {printed}");
        let negated = parse(&format!("not ({source})"));
        for ((entry, record, line, format), row) in entries.iter().zip(&truth) {
            let hit = query.matches(entry, record.as_ref());
            assert_eq!(hit, gen.eval(row), "{source}");
            assert_eq!(hit, query.matches_line(entry, line, *format), "{source}");
            assert_eq!(!hit, negated.matches(entry, record.as_ref()), "{source}");
            if hit {
                matched += 1;
            } else {
                missed += 1;
            }
        }
    }
    assert!(matched > 1_000 && missed > 1_000, "{matched} / {missed}");
}

//! Built-in text line formats (#1162): spdlog's default pattern, glog,
//! Apache/nginx access logs and syslog. Like the JSON and logfmt scanners,
//! each one accepts a whole line and reports its fields, or rejects it.
//!
//! Values are slices of the line. Two formats encode a name as a code, and
//! those are decoded to the name the format itself uses, the way JSON
//! escapes are decoded: glog's level letter (`I` is `INFO`) and syslog's PRI
//! (`<11>` is facility `user`, severity `err`). A nil value (`-`) is not
//! reported as a field.

use std::borrow::Cow;

use super::record::{Escape, RawValue, ValueKind};
use super::timestamp;

type Visit<'v, 'a> = dyn FnMut(&[Cow<'a, str>], RawValue<'a>) + 'v;

fn emit<'a>(visit: &mut Visit<'_, 'a>, key: &'static str, raw: &'a [u8]) {
    emit_kind(visit, key, raw, ValueKind::String, Escape::None);
}

fn emit_kind<'a>(
    visit: &mut Visit<'_, 'a>,
    key: &'static str,
    raw: &'a [u8],
    kind: ValueKind,
    escape: Escape,
) {
    visit(
        std::slice::from_ref(&Cow::Borrowed(key)),
        RawValue { kind, raw, escape },
    );
}

fn emit_number<'a>(visit: &mut Visit<'_, 'a>, key: &'static str, raw: &'a [u8]) {
    emit_kind(visit, key, raw, ValueKind::Number, Escape::None);
}

/// Reported unless it is the nil value `-`.
fn emit_present<'a>(visit: &mut Visit<'_, 'a>, key: &'static str, raw: &'a [u8]) {
    if raw != b"-" {
        emit(visit, key, raw);
    }
}

/// A line without its line ending.
fn content(line: &[u8]) -> &[u8] {
    line.strip_suffix(b"\r").unwrap_or(line)
}

/// `[inner]` at the start of `b`: the inner text and what follows.
fn bracket(b: &[u8]) -> Option<(&[u8], &[u8])> {
    let rest = b.strip_prefix(b"[")?;
    let close = memchr::memchr(b']', rest)?;
    Some((&rest[..close], &rest[close + 1..]))
}

/// A space-free token and what follows its single separating space, or the
/// end of the line.
fn token(b: &[u8]) -> Option<(&[u8], &[u8])> {
    let end = memchr::memchr(b' ', b).unwrap_or(b.len());
    (end > 0).then(|| (&b[..end], b.get(end + 1..).unwrap_or_default()))
}

fn all_digits(b: &[u8]) -> bool {
    !b.is_empty() && b.iter().all(u8::is_ascii_digit)
}

/// `file:line`, as spdlog's `%s:%#` and glog print it.
fn is_source(b: &[u8]) -> bool {
    memchr::memrchr(b':', b)
        .is_some_and(|colon| colon > 0 && all_digits(&b[colon + 1..]) && !b.contains(&b' '))
}

/// The message after the header: empty, or everything past one space.
fn message(rest: &[u8]) -> Option<&[u8]> {
    match rest {
        [] => Some(rest),
        [b' ', message @ ..] => Some(message),
        _ => None,
    }
}

/// spdlog's default pattern `[%Y-%m-%d %H:%M:%S.%e] [%n] [%l] %v`, without
/// the logger when it has no name, and the common variants that add `[%t]`
/// and `[%s:%#]` after the level.
pub(super) fn spdlog<'a>(line: &'a [u8], visit: &mut Visit<'_, 'a>) -> Result<(), ()> {
    const LEVELS: [&[u8]; 7] = [
        b"trace",
        b"debug",
        b"info",
        b"warning",
        b"error",
        b"critical",
        b"off",
    ];
    let line = content(line);
    let (time, mut rest) = bracket(line).ok_or(())?;
    timestamp::parse_text(time).ok_or(())?;
    let (mut logger, mut level, mut tid, mut source) = (None, None, None, None);
    while let Some((group, after)) = rest.strip_prefix(b" ").and_then(bracket) {
        if level.is_none() && LEVELS.contains(&group) {
            level = Some(group);
        } else if level.is_none() && logger.is_none() {
            logger = Some(group);
        } else if level.is_some() && tid.is_none() && source.is_none() && all_digits(group) {
            tid = Some(group);
        } else if level.is_some() && source.is_none() && is_source(group) {
            source = Some(group);
        } else {
            break;
        }
        rest = after;
    }
    let level = level.ok_or(())?;
    let msg = message(rest).ok_or(())?;
    emit(visit, "time", time);
    if let Some(logger) = logger {
        emit(visit, "logger", logger);
    }
    emit(visit, "level", level);
    if let Some(tid) = tid {
        emit_number(visit, "tid", tid);
    }
    if let Some(source) = source {
        emit(visit, "src", source);
    }
    emit(visit, "msg", msg);
    Ok(())
}

/// glog's `Lyyyymmdd hh:mm:ss.uuuuuu threadid file:line] msg`, the older
/// `Lmmdd` form without a year, and the four header lines glog writes at the
/// top of every file.
pub(super) fn glog<'a>(line: &'a [u8], visit: &mut Visit<'_, 'a>) -> Result<(), ()> {
    const HEADERS: [&[u8]; 4] = [
        b"Log file created at: ",
        b"Running on machine: ",
        b"Running duration (h:mm:ss): ",
        b"Log line format: ",
    ];
    let line = content(line);
    if HEADERS.iter().any(|header| line.starts_with(header)) {
        emit(visit, "msg", line);
        return Ok(());
    }
    let level: &'static [u8] = match line.first() {
        Some(b'I') => b"INFO",
        Some(b'W') => b"WARNING",
        Some(b'E') => b"ERROR",
        Some(b'F') => b"FATAL",
        _ => return Err(()),
    };
    let date = line[1..].iter().take_while(|c| c.is_ascii_digit()).count();
    if date != 8 && date != 4 {
        return Err(());
    }
    // ` hh:mm:ss` then an optional fraction.
    let clock = 1 + date;
    let shape = line.get(clock..clock + 9).ok_or(())?;
    if shape[0] != b' '
        || shape[3] != b':'
        || shape[6] != b':'
        || ![1, 2, 4, 5, 7, 8]
            .iter()
            .all(|&i| shape[i].is_ascii_digit())
    {
        return Err(());
    }
    let mut end = clock + 9;
    if line.get(end) == Some(&b'.') {
        end += 1;
        end += line[end..]
            .iter()
            .take_while(|c| c.is_ascii_digit())
            .count();
    }
    let time = &line[1..end];
    let rest = &line[end..];
    let spaces = rest.iter().take_while(|&&c| c == b' ').count();
    if spaces == 0 {
        return Err(());
    }
    let (tid, rest) = token(&rest[spaces..]).ok_or(())?;
    let close = memchr::memchr(b']', rest).ok_or(())?;
    let source = &rest[..close];
    if !all_digits(tid) || !is_source(source) {
        return Err(());
    }
    let msg = message(&rest[close + 1..]).ok_or(())?;
    emit(visit, "level", level);
    emit(visit, "time", time);
    emit_number(visit, "tid", tid);
    emit(visit, "src", source);
    emit(visit, "msg", msg);
    Ok(())
}

/// A double-quoted value at the start of `b`, backslash escapes allowed:
/// the content, whether it has escapes, and what follows the closing quote.
fn quoted(b: &[u8]) -> Option<(&[u8], bool, &[u8])> {
    let rest = b.strip_prefix(b"\"")?;
    let mut i = 0;
    let mut escaped = false;
    loop {
        i += memchr::memchr2(b'"', b'\\', &rest[i..])?;
        if rest[i] == b'"' {
            return Some((&rest[..i], escaped, &rest[i + 1..]));
        }
        escaped = true;
        i += 2;
        if i > rest.len() {
            return None;
        }
    }
}

fn emit_quoted<'a>(visit: &mut Visit<'_, 'a>, key: &'static str, raw: &'a [u8], escaped: bool) {
    if raw != b"-" {
        let escape = if escaped {
            Escape::Logfmt
        } else {
            Escape::None
        };
        emit_kind(visit, key, raw, ValueKind::String, escape);
    }
}

/// The Common and Combined Log Formats that Apache and nginx write by
/// default: `client ident user [time] "request" status bytes`, plus
/// `"referer" "user agent"` for combined. Anything after those stays in
/// `extra`, as written.
pub(super) fn access<'a>(line: &'a [u8], visit: &mut Visit<'_, 'a>) -> Result<(), ()> {
    let line = content(line);
    let (client, rest) = token(line).ok_or(())?;
    let (ident, rest) = token(rest).ok_or(())?;
    let (user, rest) = token(rest).ok_or(())?;
    let (time, rest) = bracket(rest).ok_or(())?;
    timestamp::parse_text(time).ok_or(())?;
    let (request, request_escaped, rest) = quoted(rest.strip_prefix(b" ").ok_or(())?).ok_or(())?;
    let (status, rest) = token(rest.strip_prefix(b" ").ok_or(())?).ok_or(())?;
    let (bytes, rest) = token(rest).ok_or(())?;
    if status.len() != 3 || !all_digits(status) || !(bytes == b"-" || all_digits(bytes)) {
        return Err(());
    }
    let mut combined = None;
    let mut extra = rest;
    if let Some((referer, referer_escaped, after)) = quoted(rest) {
        let (ua, ua_escaped, after) = quoted(after.strip_prefix(b" ").ok_or(())?).ok_or(())?;
        combined = Some((referer, referer_escaped, ua, ua_escaped));
        extra = after.strip_prefix(b" ").unwrap_or(after);
    }
    emit(visit, "client", client);
    emit_present(visit, "ident", ident);
    emit_present(visit, "user", user);
    emit(visit, "time", time);
    // `METHOD target protocol`; anything else (a probe's garbage, `-`) is
    // kept whole.
    let mut parts = request.splitn(3, |&c| c == b' ');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(method), Some(path), Some(proto))
            if !request_escaped
                && !method.is_empty()
                && method.iter().all(u8::is_ascii_uppercase)
                && !proto.contains(&b' ') =>
        {
            emit(visit, "method", method);
            emit(visit, "path", path);
            emit(visit, "proto", proto);
        }
        _ => emit_quoted(visit, "request", request, request_escaped),
    }
    emit_number(visit, "status", status);
    if bytes != b"-" {
        emit_number(visit, "bytes", bytes);
    }
    if let Some((referer, referer_escaped, ua, ua_escaped)) = combined {
        emit_quoted(visit, "referer", referer, referer_escaped);
        emit_quoted(visit, "ua", ua, ua_escaped);
    }
    if !extra.is_empty() {
        emit(visit, "extra", extra);
    }
    Ok(())
}

const FACILITIES: [&[u8]; 24] = [
    b"kern",
    b"user",
    b"mail",
    b"daemon",
    b"auth",
    b"syslog",
    b"lpr",
    b"news",
    b"uucp",
    b"cron",
    b"authpriv",
    b"ftp",
    b"ntp",
    b"audit",
    b"alert",
    b"clock",
    b"local0",
    b"local1",
    b"local2",
    b"local3",
    b"local4",
    b"local5",
    b"local6",
    b"local7",
];
const SEVERITIES: [&[u8]; 8] = [
    b"emerg", b"alert", b"crit", b"err", b"warning", b"notice", b"info", b"debug",
];

/// `<PRI>` at the start of `b`: facility and severity names, and the rest.
fn priority(b: &[u8]) -> Option<(&'static [u8], &'static [u8], &[u8])> {
    let rest = b.strip_prefix(b"<")?;
    let close = memchr::memchr(b'>', rest)?;
    let digits = &rest[..close];
    if digits.len() > 3 || !all_digits(digits) {
        return None;
    }
    let pri: usize = std::str::from_utf8(digits).ok()?.parse().ok()?;
    Some((
        *FACILITIES.get(pri / 8)?,
        SEVERITIES[pri % 8],
        &rest[close + 1..],
    ))
}

/// RFC 3164 `Mmm dd hh:mm:ss`, with a space-padded day.
fn bsd_time(b: &[u8]) -> bool {
    const MONTHS: [&[u8]; 12] = [
        b"Jan", b"Feb", b"Mar", b"Apr", b"May", b"Jun", b"Jul", b"Aug", b"Sep", b"Oct", b"Nov",
        b"Dec",
    ];
    b.len() == 15
        && MONTHS.contains(&&b[..3])
        && b[3] == b' '
        && (b[4] == b' ' || b[4].is_ascii_digit())
        && b[5].is_ascii_digit()
        && b[6] == b' '
        && b[9] == b':'
        && b[12] == b':'
        && [7, 8, 10, 11, 13, 14]
            .iter()
            .all(|&i| b[i].is_ascii_digit())
}

/// syslog as written to files and on the wire: RFC 5424
/// (`<PRI>1 time host app procid msgid structured-data msg`) and the BSD
/// form of RFC 3164 (`[<PRI>]time host tag[pid]: msg`), whose time is either
/// `Mmm dd hh:mm:ss` or, as current rsyslog writes it, RFC 3339. Without a
/// PRI, which files usually omit, a record has no level.
pub(super) fn syslog<'a>(line: &'a [u8], visit: &mut Visit<'_, 'a>) -> Result<(), ()> {
    let line = content(line);
    let (pri, rest) = match priority(line) {
        Some((facility, severity, rest)) => (Some((facility, severity)), rest),
        None => (None, line),
    };
    if let (Some(pri), Some(rest)) = (pri, rest.strip_prefix(b"1 ")) {
        return rfc5424(pri, rest, visit);
    }
    let (time, rest) = if rest.len() >= 15 && bsd_time(&rest[..15]) {
        (&rest[..15], rest[15..].strip_prefix(b" ").ok_or(())?)
    } else {
        let (time, rest) = token(rest).ok_or(())?;
        timestamp::parse_text(time).ok_or(())?;
        (time, rest)
    };
    let (host, rest) = token(rest).ok_or(())?;
    // The tag ends at `[`, `:` or a space; only `:` or `[pid]:` may follow.
    let tag_end = rest
        .iter()
        .position(|c| matches!(c, b'[' | b':' | b' '))
        .ok_or(())?;
    let program = &rest[..tag_end];
    let mut after = &rest[tag_end..];
    let mut procid = None;
    if let Some((pid, tail)) = bracket(after) {
        procid = Some(pid);
        after = tail;
    }
    let msg = after.strip_prefix(b":").ok_or(())?;
    if program.is_empty() || procid.is_some_and(<[u8]>::is_empty) {
        return Err(());
    }
    let msg = msg.strip_prefix(b" ").unwrap_or(msg);
    if let Some((facility, severity)) = pri {
        emit(visit, "facility", facility);
        emit(visit, "severity", severity);
    }
    emit(visit, "time", time);
    emit(visit, "host", host);
    emit(visit, "programname", program);
    if let Some(procid) = procid {
        emit(visit, "procid", procid);
    }
    emit(visit, "msg", msg);
    Ok(())
}

fn rfc5424<'a>(
    (facility, severity): (&'static [u8], &'static [u8]),
    rest: &'a [u8],
    visit: &mut Visit<'_, 'a>,
) -> Result<(), ()> {
    let (time, rest) = token(rest).ok_or(())?;
    if time != b"-" {
        timestamp::parse_text(time).ok_or(())?;
    }
    let (host, rest) = token(rest).ok_or(())?;
    let (app, rest) = token(rest).ok_or(())?;
    let (procid, rest) = token(rest).ok_or(())?;
    let (msgid, rest) = token(rest).ok_or(())?;
    let sd_len = structured_data(rest).ok_or(())?;
    let structured = &rest[..sd_len];
    let msg = message(&rest[sd_len..]).ok_or(())?;
    let msg = msg.strip_prefix("\u{feff}".as_bytes()).unwrap_or(msg);
    emit(visit, "facility", facility);
    emit(visit, "severity", severity);
    emit_present(visit, "time", time);
    emit_present(visit, "host", host);
    emit_present(visit, "programname", app);
    emit_present(visit, "procid", procid);
    emit_present(visit, "msgid", msgid);
    emit_present(visit, "sd", structured);
    if !msg.is_empty() {
        emit(visit, "msg", msg);
    }
    Ok(())
}

/// Length of RFC 5424 STRUCTURED-DATA at the start of `b`: `-`, or one or
/// more `[id param="value"]` elements, where a value may hold `\]` and `\"`.
fn structured_data(b: &[u8]) -> Option<usize> {
    if b.first() == Some(&b'-') {
        return Some(1);
    }
    let mut i = 0;
    while b.get(i) == Some(&b'[') {
        i += 1;
        let mut in_value = false;
        loop {
            match *b.get(i)? {
                b'\\' if in_value => i += 1,
                b'"' => in_value = !in_value,
                b']' if !in_value => break,
                _ => {}
            }
            i += 1;
        }
        i += 1;
    }
    (i > 0).then_some(i)
}

#[cfg(test)]
mod tests {
    use super::*;

    type Scan = for<'a> fn(&'a [u8], &mut Visit<'_, 'a>) -> Result<(), ()>;

    fn fields(scan: Scan, line: &str) -> Option<Vec<(String, String)>> {
        let mut out = Vec::new();
        scan(line.as_bytes(), &mut |path, value| {
            out.push((path.join("."), value.text().into_owned()));
        })
        .ok()?;
        Some(out)
    }

    fn pairs(list: &[(&str, &str)]) -> Option<Vec<(String, String)>> {
        Some(
            list.iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        )
    }

    #[test]
    fn spdlog_default_and_variants() {
        assert_eq!(
            fields(
                spdlog,
                "[2026-10-10 06:04:00.140] [cpu] [warning] cpu5 scaling_governor is 'powersave'"
            ),
            pairs(&[
                ("time", "2026-10-10 06:04:00.140"),
                ("logger", "cpu"),
                ("level", "warning"),
                ("msg", "cpu5 scaling_governor is 'powersave'"),
            ])
        );
        assert_eq!(
            fields(
                spdlog,
                "[2026-10-10 06:04:00.140] [fix.session] [error] [4414] [session.cpp:212] logon [rejected]\r"
            ),
            pairs(&[
                ("time", "2026-10-10 06:04:00.140"),
                ("logger", "fix.session"),
                ("level", "error"),
                ("tid", "4414"),
                ("src", "session.cpp:212"),
                ("msg", "logon [rejected]"),
            ])
        );
        // An unnamed logger leaves only the level.
        assert_eq!(
            fields(spdlog, "[2026-10-10 06:04:00.140] [info] up"),
            pairs(&[
                ("time", "2026-10-10 06:04:00.140"),
                ("level", "info"),
                ("msg", "up"),
            ])
        );
        assert_eq!(fields(spdlog, "[not a time] [x] [info] up"), None);
        assert_eq!(fields(spdlog, "[2026-10-10 06:04:00.140] [x] up"), None);
    }

    #[test]
    fn glog_lines_and_headers() {
        assert_eq!(
            fields(
                glog,
                "E20261010 06:04:01.250117   4413 fix_session.cc:212] logon rejected: MsgSeqNum too low"
            ),
            pairs(&[
                ("level", "ERROR"),
                ("time", "20261010 06:04:01.250117"),
                ("tid", "4413"),
                ("src", "fix_session.cc:212"),
                ("msg", "logon rejected: MsgSeqNum too low"),
            ])
        );
        assert_eq!(
            fields(glog, "W1010 06:04:01.250117 4413 a.cc:1] old form"),
            pairs(&[
                ("level", "WARNING"),
                ("time", "1010 06:04:01.250117"),
                ("tid", "4413"),
                ("src", "a.cc:1"),
                ("msg", "old form"),
            ])
        );
        assert_eq!(
            fields(glog, "Running on machine: trade-gw-01"),
            pairs(&[("msg", "Running on machine: trade-gw-01")])
        );
        assert_eq!(fields(glog, "X20261010 06:04:01 1 a.cc:1] no"), None);
        assert_eq!(fields(glog, "I2026101 06:04:01 1 a.cc:1] no"), None);
        assert_eq!(fields(glog, "Information: nothing"), None);
    }

    #[test]
    fn access_common_and_combined() {
        assert_eq!(
            fields(
                access,
                r#"198.51.100.44 - - [10/Oct/2026:06:00:04 +0000] "GET /account/orders HTTP/2.0" 302 - "https://shop.example.com/" "curl/8.10.1""#
            ),
            pairs(&[
                ("client", "198.51.100.44"),
                ("time", "10/Oct/2026:06:00:04 +0000"),
                ("method", "GET"),
                ("path", "/account/orders"),
                ("proto", "HTTP/2.0"),
                ("status", "302"),
                ("referer", "https://shop.example.com/"),
                ("ua", "curl/8.10.1"),
            ])
        );
        assert_eq!(
            fields(
                access,
                r#"10.0.0.1 - frank [10/Oct/2000:13:55:36 -0700] "\x16\x03\x01" 400 157"#
            ),
            pairs(&[
                ("client", "10.0.0.1"),
                ("user", "frank"),
                ("time", "10/Oct/2000:13:55:36 -0700"),
                ("request", "\\x16\\x03\\x01"),
                ("status", "400"),
                ("bytes", "157"),
            ])
        );
        // nginx with `$http_x_forwarded_for` and a request time appended.
        assert_eq!(
            fields(
                access,
                r#"10.0.0.1 - - [10/Oct/2026:06:00:04 +0000] "GET / HTTP/1.1" 200 5 "-" "a \"b\"" "203.0.113.9" 0.004"#
            ),
            pairs(&[
                ("client", "10.0.0.1"),
                ("time", "10/Oct/2026:06:00:04 +0000"),
                ("method", "GET"),
                ("path", "/"),
                ("proto", "HTTP/1.1"),
                ("status", "200"),
                ("bytes", "5"),
                ("ua", "a \"b\""),
                ("extra", "\"203.0.113.9\" 0.004"),
            ])
        );
        assert_eq!(
            fields(access, r#"h - - [yesterday] "GET / HTTP/1.1" 200 5"#),
            None
        );
        assert_eq!(
            fields(
                access,
                r#"h - - [10/Oct/2026:06:00:04 +0000] "GET / HTTP/1.1" 2000 5"#
            ),
            None
        );
    }

    #[test]
    fn syslog_bsd_and_rfc5424() {
        assert_eq!(
            fields(
                syslog,
                "<38>Oct  9 22:14:15 db-02 sshd[4123]: Failed password for root"
            ),
            pairs(&[
                ("facility", "auth"),
                ("severity", "info"),
                ("time", "Oct  9 22:14:15"),
                ("host", "db-02"),
                ("programname", "sshd"),
                ("procid", "4123"),
                ("msg", "Failed password for root"),
            ])
        );
        // A file as rsyslog writes it: no PRI, RFC 3339 time.
        assert_eq!(
            fields(
                syslog,
                "2026-10-10T06:00:05.123595+00:00 db-02 kernel: [ 12.5] md/raid1:md0: Disk failure"
            ),
            pairs(&[
                ("time", "2026-10-10T06:00:05.123595+00:00"),
                ("host", "db-02"),
                ("programname", "kernel"),
                ("msg", "[ 12.5] md/raid1:md0: Disk failure"),
            ])
        );
        assert_eq!(
            fields(
                syslog,
                r#"<165>1 2003-10-11T22:14:15.003Z mymachine.example.com evntslog - ID47 [exampleSDID@32473 iut="3" eventID="10\]11"] An application event"#
            ),
            pairs(&[
                ("facility", "local4"),
                ("severity", "notice"),
                ("time", "2003-10-11T22:14:15.003Z"),
                ("host", "mymachine.example.com"),
                ("programname", "evntslog"),
                ("msgid", "ID47"),
                ("sd", r#"[exampleSDID@32473 iut="3" eventID="10\]11"]"#),
                ("msg", "An application event"),
            ])
        );
        assert_eq!(
            fields(syslog, "<11>1 - - - - - -"),
            pairs(&[("facility", "user"), ("severity", "err")])
        );
        assert_eq!(fields(syslog, "<192>Oct  9 22:14:15 h a: x"), None);
        assert_eq!(fields(syslog, "Oct  9 22:14:15 h no tag"), None);
        assert_eq!(fields(syslog, "hello world"), None);
    }
}

//! Remote Markdown reads and explicit guarded saves over the existing OpenSSH authority.
use crate::log::sftp::{Target, Transport};
use std::{
    io,
    time::{Duration, Instant},
};

pub const BYTE_LIMIT: usize = 2 * 1024 * 1024;
pub const DEADLINE: Duration = Duration::from_secs(30);
const CHUNK: usize = 32 * 1024;

#[cfg(any(unix, windows))]
pub mod draft;
pub mod folder;
pub mod save;

#[derive(Clone, Debug)]
pub struct Source {
    url: String,
    target: Target,
}
impl Source {
    /// `ssh://[user@]host[:port]/path.md`; the host may be an SSH config
    /// alias. A URL password is refused: OpenSSH asks through the app when it
    /// needs a passphrase, password or unknown-host trust.
    pub fn parse(url: &str) -> io::Result<Self> {
        let rest = url
            .strip_prefix("ssh://")
            .ok_or_else(|| invalid("Expected ssh://host/path"))?;
        let target = Target::parse_login(&format!("sftp://{rest}"))?;
        if !rest
            .split_once('/')
            .is_some_and(|(_, path)| path.to_ascii_lowercase().ends_with(".md"))
        {
            return Err(invalid("Choose a remote .md file"));
        }
        Ok(Self {
            url: url.into(),
            target,
        })
    }
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Resolve document links only within this explicit SSH authority. Never
    /// hand a remote relative link to local vault resolution or a shell.
    /// Fragments stay visible as navigation intent for the caller.
    pub fn resolve(&self, link: &str) -> io::Result<(Self, Option<String>)> {
        let (path, fragment) = link
            .split_once('#')
            .map_or((link, None), |(p, f)| (p, Some(f.to_owned())));
        if path.contains(['?', '\\']) || path.bytes().any(|b| b.is_ascii_control()) {
            return Err(invalid("Unsupported remote link"));
        }
        if path.starts_with("ssh://") {
            return Ok((Self::parse(path)?, fragment));
        }
        if path.contains(':') || path.starts_with("//") {
            return Err(invalid("Unsupported remote link scheme or authority"));
        }
        if path.is_empty() {
            return Ok((self.clone(), fragment));
        }
        let rest = self.url.strip_prefix("ssh://").unwrap();
        let (host, current) = rest.split_once('/').unwrap();
        // SFTP uses decoded filesystem paths: encoded separators and dot
        // segments must participate in the same source-relative resolution.
        let current = decode_path(current)?;
        let path = decode_path(path)?;
        let joined = if path.starts_with('/') {
            path.to_owned()
        } else {
            let parent = current.rsplit_once('/').map_or("", |(p, _)| p);
            format!("/{parent}/{path}")
        };
        let mut segments = Vec::new();
        for segment in joined.split('/') {
            match segment {
                "" | "." => {}
                ".." => {
                    if segments.pop().is_none() {
                        return Err(invalid("Remote link escapes root"));
                    }
                }
                _ => segments.push(segment),
            }
        }
        Ok((
            Self::parse(&format!(
                "ssh://{host}/{}",
                encode_path(&segments.join("/"))
            ))?,
            fragment,
        ))
    }
}
fn decode_path(path: &str) -> io::Result<String> {
    let mut decoded = Vec::new();
    let mut bytes = path.bytes();
    while let Some(byte) = bytes.next() {
        decoded.push(if byte == b'%' {
            let hi = bytes.next().and_then(|b| (b as char).to_digit(16));
            let lo = bytes.next().and_then(|b| (b as char).to_digit(16));
            match (hi, lo) {
                (Some(hi), Some(lo)) => (hi * 16 + lo) as u8,
                _ => return Err(invalid("Invalid remote link escape")),
            }
        } else {
            byte
        });
    }
    String::from_utf8(decoded).map_err(|_| invalid("Remote link path is not UTF-8"))
}
fn encode_path(path: &str) -> String {
    let mut encoded = String::new();
    for b in path.bytes() {
        if b.is_ascii_alphanumeric() || b"/-._~".contains(&b) {
            encoded.push(b as char);
        } else {
            use std::fmt::Write;
            write!(encoded, "%{b:02X}").unwrap();
        }
    }
    encoded
}
fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

/// Must run on a background worker. The existing OpenSSH per-operation deadline
/// bounds an in-flight request; cancellation/total deadline are checked before
/// and after every operation. No partial document is published as a success.
pub fn read(
    source: &Source,
    transport: &mut impl Transport,
    cancelled: impl Fn() -> bool,
) -> io::Result<String> {
    read_until(source, transport, cancelled, Instant::now() + DEADLINE)
}
fn read_until(
    source: &Source,
    transport: &mut impl Transport,
    cancelled: impl Fn() -> bool,
    deadline: Instant,
) -> io::Result<String> {
    let deadline = crate::ssh_askpass::Deadline::at(deadline);
    let check = || {
        if cancelled() {
            Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "Remote read cancelled",
            ))
        } else if deadline.expired() {
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Remote read deadline",
            ))
        } else {
            Ok(())
        }
    };
    let result = (|| {
        check()?;
        transport.open(&source.target)?;
        check()?;
        let mut bytes = Vec::new();
        loop {
            check()?;
            // One extra byte distinguishes an exact-limit file from truncation.
            let limit = CHUNK.min(BYTE_LIMIT + 1 - bytes.len());
            let chunk = transport.read_at(bytes.len() as u64, limit)?;
            check()?;
            if chunk.len() > limit {
                return Err(invalid("Remote read exceeds requested length"));
            }
            if chunk.is_empty() {
                break;
            }
            bytes.extend_from_slice(&chunk);
            if bytes.len() > BYTE_LIMIT {
                return Err(invalid("Remote Markdown exceeds 2 MiB limit"));
            }
        }
        String::from_utf8(bytes).map_err(|_| invalid("Remote Markdown is not UTF-8"))
    })();
    transport.disconnect();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    struct Fake {
        data: Vec<u8>,
        opened: bool,
        disconnected: bool,
        reads: usize,
        fail: bool,
    }
    impl Transport for Fake {
        fn open(&mut self, _: &Target) -> io::Result<()> {
            self.opened = true;
            Ok(())
        }
        fn read_at(&mut self, offset: u64, limit: usize) -> io::Result<Vec<u8>> {
            self.reads += 1;
            if self.fail {
                return Err(io::Error::new(io::ErrorKind::BrokenPipe, "disconnected"));
            }
            let offset = offset as usize;
            Ok(self.data[offset..self.data.len().min(offset + limit.min(3))].to_vec())
        }
        fn disconnect(&mut self) {
            self.disconnected = true;
        }
    }
    fn fake(data: &[u8]) -> Fake {
        Fake {
            data: data.into(),
            opened: false,
            disconnected: false,
            reads: 0,
            fail: false,
        }
    }
    fn source() -> Source {
        Source::parse("ssh://alias/notes/start.md").unwrap()
    }
    #[test]
    fn short_reads_utf8_eof_and_disconnect() {
        let mut f = fake("# Привет\n".as_bytes());
        assert_eq!(read(&source(), &mut f, || false).unwrap(), "# Привет\n");
        assert!(f.opened && f.disconnected && f.reads > 2);
    }
    #[test]
    fn cancel_deadline_failure_and_invalid_utf8_do_not_publish() {
        let calls = Cell::new(0);
        let mut f = fake(b"abcdef");
        assert_eq!(
            read(&source(), &mut f, || {
                calls.set(calls.get() + 1);
                calls.get() > 3
            })
            .unwrap_err()
            .kind(),
            io::ErrorKind::Interrupted
        );
        assert!(f.disconnected && f.reads == 1);
        let mut f = fake(b"x");
        assert_eq!(
            read_until(&source(), &mut f, || false, Instant::now())
                .unwrap_err()
                .kind(),
            io::ErrorKind::TimedOut
        );
        assert!(!f.opened && f.disconnected);
        let mut f = fake(b"x");
        f.fail = true;
        assert_eq!(
            read(&source(), &mut f, || false).unwrap_err().kind(),
            io::ErrorKind::BrokenPipe
        );
        assert!(f.disconnected);
        assert!(read(&source(), &mut fake(&[255]), || false).is_err());
    }
    #[test]
    fn target_and_source_relative_links_keep_remote_authority() {
        for url in [
            "ssh://-oProxyCommand=x/a.md",
            "ssh://user:password@host/a.md",
            "ssh://-l@host/a.md",
            "ssh://user@-host/a.md",
            "ssh://@host/a.md",
            "ssh://host:/a.md",
            "ssh://host:0/a.md",
            "ssh://host:65536/a.md",
            "ssh://host:22x/a.md",
            "ssh://host/a.md?x",
            "ssh://host/a%00.md",
        ] {
            assert!(Source::parse(url).is_err(), "{url}");
        }
        // #1610: ssh://[user@]host[:port]/path, host may be a config alias.
        let login = Source::parse("ssh://oleg@build-box:2222/srv/notes/a.md").unwrap();
        assert_eq!(
            login.resolve("../b.md").unwrap().0.url(),
            "ssh://oleg@build-box:2222/srv/b.md"
        );
        assert!(Source::parse("ssh://user@host/a.md").is_ok());
        assert!(Source::parse("ssh://host:22/a.md").is_ok());
        assert_eq!(
            source().resolve("../other.md#Heading").unwrap().0.url(),
            "ssh://alias/other.md"
        );
        assert_eq!(
            source().resolve("sibling.md").unwrap().0.url(),
            "ssh://alias/notes/sibling.md"
        );
        assert_eq!(
            source().resolve("#Heading").unwrap().1.as_deref(),
            Some("Heading")
        );
        assert_eq!(
            source().resolve("%2e%2e/space%20name.md").unwrap().0.url(),
            "ssh://alias/space%20name.md"
        );
        let encoded_base = Source::parse("ssh://alias/notes%2Fsub/start.md").unwrap();
        assert_eq!(
            encoded_base.resolve("../next.md").unwrap().0.url(),
            "ssh://alias/notes/next.md"
        );
        for link in [
            "../../escape.md",
            "//other/a.md",
            "file:///a.md",
            "https://host/a.md",
            "bad%00.md",
        ] {
            assert!(source().resolve(link).is_err(), "{link}");
        }
    }
    #[test]
    fn oversize_and_exact_limit_are_distinguished() {
        struct Large {
            size: usize,
        }
        impl Transport for Large {
            fn open(&mut self, _: &Target) -> io::Result<()> {
                Ok(())
            }
            fn read_at(&mut self, offset: u64, limit: usize) -> io::Result<Vec<u8>> {
                Ok(vec![
                    b'a';
                    limit.min(self.size.saturating_sub(offset as usize))
                ])
            }
            fn disconnect(&mut self) {}
        }
        assert_eq!(
            read(&source(), &mut Large { size: BYTE_LIMIT }, || false)
                .unwrap()
                .len(),
            BYTE_LIMIT
        );
        assert!(read(
            &source(),
            &mut Large {
                size: BYTE_LIMIT + 1
            },
            || false
        )
        .is_err());
    }
}

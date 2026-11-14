//! Bounded on-demand remote browsing. No recursion, index, search or writes.
use super::{Source, DEADLINE};
pub use crate::log::sftp::directory::{Entry, Kind};
use crate::log::sftp::{directory::DirectoryTransport, Target};
use std::{collections::HashSet, io};
pub const ENTRY_LIMIT: usize = 1000;
pub const NAME_BYTES_LIMIT: usize = 256 * 1024;
pub const PAGE_LIMIT: usize = 128;
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Folder {
    url: String,
    target: Target,
}
impl Folder {
    pub fn parse(url: &str) -> io::Result<Self> {
        let rest = url
            .strip_prefix("ssh://")
            .ok_or_else(|| invalid("Expected ssh://alias/folder/"))?;
        let target = Target::parse_login(&format!("sftp://{rest}"))?;
        let host = rest.split_once('/').unwrap().0;
        let mut parts = Vec::new();
        for part in target.remote_path().split(|b| *b == b'/') {
            match part {
                b"" | b"." => {}
                b".." => {
                    if parts.pop().is_none() {
                        return Err(invalid("Remote folder escapes the server root"));
                    }
                }
                _ => parts.push(part),
            }
        }
        let mut path = vec![b'/'];
        for part in parts {
            path.extend(part);
            path.push(b'/');
        }
        let url = format!("ssh://{host}{}", encode(&path));
        Ok(Self {
            target: Target::parse_login(&format!(
                "sftp://{}",
                url.strip_prefix("ssh://").unwrap()
            ))?,
            url,
        })
    }
    pub fn url(&self) -> &str {
        &self.url
    }
    pub fn parent(&self) -> Option<Self> {
        let trimmed = self.url.trim_end_matches('/');
        let (parent, _) = trimmed.rsplit_once('/')?;
        if parent.ends_with(':') {
            return None;
        }
        Self::parse(&format!("{parent}/")).ok()
    }
    pub fn containing(source: &Source) -> io::Result<Self> {
        let host = source
            .url()
            .strip_prefix("ssh://")
            .unwrap()
            .split_once('/')
            .unwrap()
            .0;
        let path = source.target.remote_path();
        let end = path.iter().rposition(|&b| b == b'/').unwrap();
        Self::parse(&format!("ssh://{host}{}", encode(&path[..=end])))
    }
    pub fn child_folder(&self, entry: &Entry) -> io::Result<Self> {
        validate_name(&entry.name)?;
        if entry.kind != Kind::Directory {
            return Err(invalid("This entry is not a directory"));
        }
        Self::parse(&format!("{}{}", self.url, encode(&entry.name)))
    }
    pub fn markdown(&self, entry: &Entry) -> io::Result<Source> {
        validate_name(&entry.name)?;
        if entry.kind == Kind::Directory || !entry.is_markdown() {
            return Err(invalid("Choose a Markdown file or a folder"));
        }
        Source::parse(&format!("{}{}", self.url, encode(&entry.name)))
    }
}
impl Entry {
    pub fn is_markdown(&self) -> bool {
        self.kind != Kind::Directory && self.name.to_ascii_lowercase().ends_with(b".md")
    }
    pub fn label(&self) -> String {
        match std::str::from_utf8(&self.name) {
            Ok(name) => name
                .chars()
                .map(|c| {
                    if c.is_control() || c == '\\' {
                        c.escape_default().to_string()
                    } else {
                        c.to_string()
                    }
                })
                .collect(),
            Err(_) => self
                .name
                .iter()
                .map(|&b| {
                    if b.is_ascii_graphic() && b != b'\\' {
                        (b as char).to_string()
                    } else {
                        format!("\\x{b:02X}")
                    }
                })
                .collect(),
        }
    }
}
#[derive(Debug)]
pub struct Listing {
    pub entries: Vec<Entry>,
}
fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
fn validate_name(name: &[u8]) -> io::Result<()> {
    if name.is_empty()
        || name == b"."
        || name == b".."
        || name.contains(&0)
        || name.contains(&b'/')
        || name.len() > 4096
    {
        return Err(invalid(
            "The server returned an invalid directory entry name",
        ));
    }
    Ok(())
}
fn encode(path: &[u8]) -> String {
    let mut encoded = String::new();
    for &b in path {
        if b.is_ascii_alphanumeric() || b"/-._~".contains(&b) {
            encoded.push(b as char);
        } else {
            use std::fmt::Write;
            write!(encoded, "%{b:02X}").unwrap();
        }
    }
    encoded
}
/// Run on the existing remote IO worker. The adapter's bounded packets and
/// per-operation timeout also apply. Errors never publish a partial listing.
pub fn list(
    folder: &Folder,
    transport: &mut impl DirectoryTransport,
    cancelled: impl Fn() -> bool,
) -> io::Result<Listing> {
    let deadline =
        crate::ssh_askpass::Deadline::after(DEADLINE, transport.directory_prompt_clock());
    let check = || {
        if cancelled() {
            Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "Remote folder listing cancelled",
            ))
        } else if deadline.expired() {
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Remote folder listing timed out",
            ))
        } else {
            Ok(())
        }
    };
    let result = (|| {
        check()?;
        transport.open_directory(&folder.target)?;
        check()?;
        let mut entries = Vec::new();
        let mut names = HashSet::new();
        let mut bytes = 0;
        let mut total = 0;
        for _ in 0..PAGE_LIMIT {
            check()?;
            let page = transport.read_directory()?;
            check()?;
            let Some(page) = page else {
                entries.sort_by_cached_key(|entry: &Entry| {
                    (
                        entry.kind != Kind::Directory,
                        entry.name.to_ascii_lowercase(),
                        entry.name.clone(),
                    )
                });
                return Ok(Listing { entries });
            };
            for entry in page {
                total += 1;
                bytes += entry.name.len();
                if total > ENTRY_LIMIT || bytes > NAME_BYTES_LIMIT {
                    return Err(invalid("This folder exceeds the listing limit (1000 entries / 256 KiB of names). No partial listing is shown; open a smaller subfolder directly."));
                }
                if entry.name == b"." || entry.name == b".." {
                    continue;
                }
                validate_name(&entry.name)?;
                if !names.insert(entry.name.clone()) {
                    return Err(invalid(
                        "The folder changed during listing (duplicate names). Retry to refresh it.",
                    ));
                }
                entries.push(entry);
            }
        }
        Err(invalid("This folder exceeds the listing page limit. No partial listing is shown; open a smaller subfolder directly."))
    })();
    transport.disconnect_directory();
    result
}

#[cfg(test)]
#[path = "folder_tests.rs"]
mod tests;

//! Read-only SFTP directory operations on the existing adapter/session decoder.
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Directory,
    File,
    Other,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: Vec<u8>,
    pub kind: Kind,
}
/// Same OpenSSH adapter, with directory handles instead of a file read handle.
pub trait DirectoryTransport {
    fn open_directory(&mut self, target: &Target) -> io::Result<()>;
    fn read_directory(&mut self) -> io::Result<Option<Vec<Entry>>>;
    fn disconnect_directory(&mut self);
    /// See [`Transport::prompt_clock`].
    fn directory_prompt_clock(&self) -> Option<Arc<Clock>> {
        None
    }
}
fn status(mut body: &[u8]) -> io::Result<u32> {
    let code = take_u32(&mut body)?;
    take_string(&mut body)?;
    take_string(&mut body)?;
    if !body.is_empty() {
        return Err(invalid("Invalid SFTP directory status"));
    }
    Ok(code)
}
fn remote_error(code: u32) -> io::Error {
    match code {
        2 => io::Error::new(io::ErrorKind::NotFound, "The remote path does not exist"),
        3 => io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Permission denied for this remote folder",
        ),
        _ => io::Error::other(
            "The server could not list this remote folder (it may not be a directory)",
        ),
    }
}
fn attributes(body: &mut &[u8]) -> io::Result<Kind> {
    let flags = take_u32(body)?;
    if flags & !0x8000000f != 0 {
        return Err(invalid("Unsupported SFTP directory attributes"));
    }
    if flags & 1 != 0 {
        take_u32(body)?;
        take_u32(body)?;
    }
    if flags & 2 != 0 {
        take_u32(body)?;
        take_u32(body)?;
    }
    let mode = if flags & 4 != 0 { take_u32(body)? } else { 0 };
    if flags & 8 != 0 {
        take_u32(body)?;
        take_u32(body)?;
    }
    if flags & 0x80000000 != 0 {
        let count = take_u32(body)?;
        if count > 128 {
            return Err(invalid("Too many extended SFTP directory attributes"));
        }
        for _ in 0..count {
            take_string(body)?;
            take_string(body)?;
        }
    }
    Ok(match mode & 0o170000 {
        0o040000 => Kind::Directory,
        0o100000 => Kind::File,
        _ => Kind::Other,
    })
}
fn decode_names(kind: u8, mut body: &[u8]) -> io::Result<Option<Vec<Entry>>> {
    if kind == 101 {
        return match status(body)? {
            1 => Ok(None),
            code => Err(remote_error(code)),
        };
    }
    if kind != 104 {
        return Err(invalid("Expected an SFTP directory listing"));
    }
    let count = take_u32(&mut body)?;
    // Bound allocation even if a hostile packet advertises a huge entry count.
    if count > 1024 {
        return Err(invalid("SFTP directory page exceeds its entry limit"));
    }
    let mut entries = Vec::new();
    for _ in 0..count {
        let name = take_string(&mut body)?;
        if name.is_empty() || name.len() > 4096 || name.contains(&0) || name.contains(&b'/') {
            return Err(invalid(
                "The server returned an invalid directory entry name",
            ));
        }
        take_string(&mut body)?; // longname is a presentation hint, never authority.
        let kind = attributes(&mut body)?;
        entries.push(Entry {
            name: name.to_vec(),
            kind,
        });
    }
    if !body.is_empty() {
        return Err(invalid("Trailing SFTP directory data"));
    }
    Ok(Some(entries))
}
impl DirectoryTransport for OpenSsh {
    fn open_directory(&mut self, target: &Target) -> io::Result<()> {
        self.disconnect();
        // Reuse Target's fixed read-only subsystem arguments, packet framing,
        // response-ID checks and Session teardown. No remote shell command.
        let mut session = self.spawn(target)?;
        let version = session.exchange(&[1, 0, 0, 0, 3])?;
        if version.len() < 5 || version[0] != 2 || version[1..5] != 3u32.to_be_bytes() {
            return Err(invalid("SFTP v3 required"));
        }
        let mut request = Vec::new();
        string(&mut request, &target.path);
        let (kind, body) = session.request(11, &request)?; // SSH_FXP_OPENDIR
        if kind == 101 {
            return Err(remote_error(status(&body)?));
        }
        if kind != 102 {
            return Err(invalid("SFTP directory open did not return a handle"));
        }
        let mut body = body.as_slice();
        let handle = take_string(&mut body)?;
        if handle.is_empty() || handle.len() > 4096 || !body.is_empty() {
            return Err(invalid("Invalid SFTP directory handle"));
        }
        session.handle = handle.to_vec();
        self.session = Some(session);
        Ok(())
    }
    fn read_directory(&mut self) -> io::Result<Option<Vec<Entry>>> {
        let session = self
            .session
            .as_mut()
            .ok_or_else(|| invalid("SFTP disconnected"))?;
        let mut request = Vec::new();
        string(&mut request, &session.handle);
        let (kind, body) = session.request(12, &request)?; // SSH_FXP_READDIR only
        decode_names(kind, &body)
    }
    fn disconnect_directory(&mut self) {
        self.disconnect();
    }
    fn directory_prompt_clock(&self) -> Option<Arc<Clock>> {
        self.prompt_clock()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn folder1610_wire_attributes_and_status_errors_are_bounded() {
        let mut body = 2u32.to_be_bytes().to_vec();
        for (name, mode) in [
            (b"folder".as_slice(), 0o040755u32),
            (b"note.md".as_slice(), 0o100644),
        ] {
            string(&mut body, name);
            string(&mut body, b"untrusted longname");
            body.extend(15u32.to_be_bytes());
            body.extend(0u64.to_be_bytes());
            body.extend(0u64.to_be_bytes());
            body.extend(mode.to_be_bytes());
            body.extend(0u64.to_be_bytes());
        }
        let names = decode_names(104, &body).unwrap().unwrap();
        assert_eq!(names[0].kind, Kind::Directory);
        assert_eq!(names[1].name, b"note.md");
        for (code, expected) in [
            (2u32, io::ErrorKind::NotFound),
            (3, io::ErrorKind::PermissionDenied),
        ] {
            let mut body = code.to_be_bytes().to_vec();
            string(&mut body, b"remote diagnostic");
            string(&mut body, b"");
            assert_eq!(decode_names(101, &body).unwrap_err().kind(), expected);
        }
        assert!(decode_names(104, &u32::MAX.to_be_bytes()).is_err());
        assert!(decode_names(104, &body[..body.len() - 1]).is_err());
        let mut invalid_name = 1u32.to_be_bytes().to_vec();
        string(&mut invalid_name, b"../redirect.md");
        assert!(decode_names(104, &invalid_name).is_err());
        let unknown = 16u32.to_be_bytes().as_slice().to_vec();
        assert!(attributes(&mut unknown.as_slice()).is_err());
    }
}

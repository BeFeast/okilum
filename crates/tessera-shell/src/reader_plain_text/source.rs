//! Bounded, lossless UTF-8 prefixes; no parsing or writes to canonical files.
use std::{
    fs::File,
    io::{self, Read},
    path::Path,
};

pub(super) const LIMIT: usize = 64 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Preview {
    Content { text: String, partial: bool },
    Unsupported,
}

pub(super) fn load(path: &Path) -> io::Result<Preview> {
    read(File::open(path)?)
}

fn read(reader: impl Read) -> io::Result<Preview> {
    let mut bytes = Vec::new();
    reader.take((LIMIT + 1) as u64).read_to_end(&mut bytes)?;
    let partial = bytes.len() > LIMIT;
    bytes.truncate(LIMIT);
    if bytes.contains(&0) {
        return Ok(Preview::Unsupported);
    }
    let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(&bytes);
    let text = match std::str::from_utf8(bytes) {
        Ok(text) => text,
        Err(error) if partial && error.error_len().is_none() => {
            // A character split by our own bound is not an encoding error.
            std::str::from_utf8(&bytes[..error.valid_up_to()]).expect("valid UTF-8 prefix")
        }
        Err(_) => return Ok(Preview::Unsupported),
    };
    Ok(Preview::Content {
        text: text.to_owned(),
        partial,
    })
}

/// Reuse Reader selection without interpreting any authored Markdown or HTML.
pub(super) fn fenced(text: &str) -> String {
    let longest = text
        .as_bytes()
        .split(|byte| *byte != b'`')
        .map(<[u8]>::len)
        .max()
        .unwrap_or(0);
    let fence = "`".repeat(3.max(longest + 1));
    let newline = if text.ends_with('\n') { "" } else { "\n" };
    format!("{fence}\n{text}{newline}{fence}\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn preserves_literal_text_and_bom_without_parsing() {
        let text = "# Heading\n[[note]] **bold** <script> &\nname,value\n";
        let bytes = [b"\xef\xbb\xbf".as_slice(), text.as_bytes()].concat();
        assert_eq!(
            read(Cursor::new(bytes)).unwrap(),
            Preview::Content {
                text: text.into(),
                partial: false
            }
        );
        assert_eq!(
            read(Cursor::new(b"\n  a\t b\r\n")).unwrap(),
            Preview::Content {
                text: "\n  a\t b\r\n".into(),
                partial: false
            }
        );
    }

    #[test]
    fn truncation_keeps_utf8_whole_and_reports_partial() {
        let text = format!("{}Жremaining", "a".repeat(LIMIT - 1));
        assert_eq!(
            read(Cursor::new(text)).unwrap(),
            Preview::Content {
                text: "a".repeat(LIMIT - 1),
                partial: true
            }
        );
        let exact = "a".repeat(LIMIT);
        assert_eq!(
            read(Cursor::new(&exact)).unwrap(),
            Preview::Content {
                text: exact,
                partial: false
            }
        );
    }

    #[test]
    fn empty_binary_and_invalid_encoding_are_distinct() {
        assert_eq!(
            read(Cursor::new([])).unwrap(),
            Preview::Content {
                text: String::new(),
                partial: false
            }
        );
        for bytes in [b"a\0b".as_slice(), &[0xff, 0xfe], &[0xd0]] {
            assert_eq!(read(Cursor::new(bytes)).unwrap(), Preview::Unsupported);
        }
    }

    #[test]
    fn literal_fences_cannot_end_the_preview_block() {
        let text = "```\n[link](https://example.com)\n````\n~~~\n";
        let rendered = fenced(text);
        assert_eq!(rendered, format!("`````\n{text}`````\n"));
    }

    #[test]
    fn bounds_reads_even_if_the_reader_keeps_producing_data() {
        struct Endless(usize);
        impl Read for &mut Endless {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                buf.fill(b'a');
                self.0 += buf.len();
                Ok(buf.len())
            }
        }
        let mut source = Endless(0);
        assert!(matches!(
            read(&mut source).unwrap(),
            Preview::Content { partial: true, .. }
        ));
        assert_eq!(source.0, LIMIT + 1);
    }

    #[test]
    fn propagates_read_failure_instead_of_showing_empty_text() {
        struct Broken;
        impl Read for Broken {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::new(io::ErrorKind::PermissionDenied, "fixture"))
            }
        }
        assert_eq!(
            read(Broken).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
    }

    #[test]
    fn loading_a_file_preserves_its_bytes() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("tessera-plain-{}-{stamp}.txt", std::process::id()));
        let bytes = b"\xef\xbb\xbf# Literal\r\n  a\tb\r\n";
        std::fs::write(&path, bytes).unwrap();
        assert!(matches!(
            load(&path).unwrap(),
            Preview::Content { partial: false, .. }
        ));
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        std::fs::remove_file(path).unwrap();
    }
}

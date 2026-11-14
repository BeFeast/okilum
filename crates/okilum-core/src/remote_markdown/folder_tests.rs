use super::*;
use std::collections::VecDeque;
struct Offline {
    pages: VecDeque<io::Result<Option<Vec<Entry>>>>,
    opens: usize,
    reads: usize,
    closed: usize,
}
impl Offline {
    fn new(pages: Vec<Vec<Entry>>) -> Self {
        Self {
            pages: pages
                .into_iter()
                .map(|p| Ok(Some(p)))
                .chain([Ok(None)])
                .collect(),
            opens: 0,
            reads: 0,
            closed: 0,
        }
    }
}
impl DirectoryTransport for Offline {
    fn open_directory(&mut self, _: &Target) -> io::Result<()> {
        self.opens += 1;
        Ok(())
    }
    fn read_directory(&mut self) -> io::Result<Option<Vec<Entry>>> {
        self.reads += 1;
        self.pages
            .pop_front()
            .unwrap_or_else(|| panic!("no unbounded repeated requests"))
    }
    fn disconnect_directory(&mut self) {
        self.closed += 1;
    }
}
fn entry(name: &str, kind: Kind) -> Entry {
    Entry {
        name: name.as_bytes().to_vec(),
        kind,
    }
}
#[test]
fn folder1610_sorted_listing_directories_first_and_exact_markdown_navigation() {
    let folder = Folder::parse("ssh://alias/notes/").unwrap();
    let mut transport = Offline::new(vec![
        vec![
            entry("z.md", Kind::File),
            entry("a.txt", Kind::File),
            entry("B", Kind::Directory),
        ],
        vec![
            entry("a", Kind::Directory),
            entry("Alpha.MD", Kind::File),
            entry(".", Kind::Directory),
            entry("..", Kind::Directory),
        ],
    ]);
    let listing = list(&folder, &mut transport, || false).unwrap();
    assert_eq!(transport.reads, 3);
    assert_eq!(transport.closed, 1);
    assert_eq!(
        listing.entries.iter().map(Entry::label).collect::<Vec<_>>(),
        ["a", "B", "a.txt", "Alpha.MD", "z.md"]
    );
    assert_eq!(
        folder.child_folder(&listing.entries[0]).unwrap().url(),
        "ssh://alias/notes/a/"
    );
    assert_eq!(
        folder.markdown(&listing.entries[3]).unwrap().url(),
        "ssh://alias/notes/Alpha.MD"
    );
    assert!(listing.entries[3].is_markdown());
    assert!(!listing.entries[2].is_markdown());
    assert!(folder.markdown(&listing.entries[2]).is_err());
}
#[test]
fn folder1610_bounds_entries_bytes_and_empty_pages_without_partial_success() {
    let folder = Folder::parse("ssh://alias/notes/").unwrap();
    let entries = (0..ENTRY_LIMIT)
        .map(|i| entry(&format!("{i}.md"), Kind::File))
        .collect::<Vec<_>>();
    let mut accepted = Offline::new(vec![entries.clone()]);
    assert_eq!(
        list(&folder, &mut accepted, || false)
            .unwrap()
            .entries
            .len(),
        ENTRY_LIMIT
    );
    let mut too_many = Offline::new(vec![entries, vec![entry("one-more.md", Kind::File)]]);
    assert!(list(&folder, &mut too_many, || false)
        .unwrap_err()
        .to_string()
        .contains("1000 entries"));
    assert_eq!(too_many.closed, 1);
    let mut bytes = Offline::new(vec![(0..100)
        .map(|i| entry(&format!("{i}{}", "x".repeat(3000)), Kind::File))
        .collect()]);
    assert!(list(&folder, &mut bytes, || false)
        .unwrap_err()
        .to_string()
        .contains("256 KiB"));
    assert_eq!(bytes.closed, 1);
    let mut empty = Offline::new(vec![vec![]; PAGE_LIMIT]);
    assert!(list(&folder, &mut empty, || false)
        .unwrap_err()
        .to_string()
        .contains("page limit"));
    assert_eq!(empty.reads, PAGE_LIMIT);
    assert_eq!(empty.closed, 1);
}
#[test]
fn folder1610_missing_permission_and_late_error_never_publish_partial_rows() {
    let folder = Folder::parse("ssh://alias/private/").unwrap();
    for kind in [io::ErrorKind::NotFound, io::ErrorKind::PermissionDenied] {
        let mut transport = Offline::new(vec![vec![entry("accepted.md", Kind::File)]]);
        transport.pages = VecDeque::from([
            Ok(Some(vec![entry("partial.md", Kind::File)])),
            Err(io::Error::new(kind, "offline error")),
        ]);
        assert_eq!(
            list(&folder, &mut transport, || false).unwrap_err().kind(),
            kind
        );
        assert_eq!(transport.closed, 1);
    }
    let calls = std::cell::Cell::new(0);
    let mut transport = Offline::new(vec![vec![entry("one.md", Kind::File)]]);
    assert_eq!(
        list(&folder, &mut transport, || {
            let n = calls.get();
            calls.set(n + 1);
            n >= 4
        })
        .unwrap_err()
        .kind(),
        io::ErrorKind::Interrupted
    );
    assert_eq!(transport.reads, 1);
    assert_eq!(transport.closed, 1);
}
#[test]
fn folder1610_parent_and_encoded_child_preserve_exact_alias_and_path() {
    let folder = Folder::parse("ssh://same_alias/a%2Fb/c/../").unwrap();
    assert_eq!(folder.url(), "ssh://same_alias/a/b/");
    let up = folder.parent().unwrap();
    assert_eq!(up.url(), "ssh://same_alias/a/");
    let root = up.parent().unwrap();
    assert_eq!(root.url(), "ssh://same_alias/");
    assert!(root.parent().is_none());
    let source = Source::parse("ssh://same_alias/a%2Fb/note.md").unwrap();
    assert_eq!(Folder::containing(&source).unwrap(), folder);
    let selected = entry("same name#?$(id).md", Kind::File);
    let source = folder.markdown(&selected).unwrap();
    assert_eq!(
        source.url(),
        "ssh://same_alias/a/b/same%20name%23%3F%24%28id%29.md"
    );
    // #1610: user and port from the link stay part of the authority.
    let login = Folder::parse("ssh://user@alias:2222/a/b/").unwrap();
    assert_eq!(login.parent().unwrap().url(), "ssh://user@alias:2222/a/");
    for invalid in [
        "ssh://user:password@alias/a/",
        "ssh://alias:0/a/",
        "ssh://alias/a/?q",
        "ssh://alias/a/#heading",
        "ssh://alias/../../",
        "ssh://alias/%00/",
    ] {
        assert!(Folder::parse(invalid).is_err(), "{invalid}");
    }
}
#[test]
fn folder1610_invalid_duplicate_and_control_names_do_not_redirect() {
    let folder = Folder::parse("ssh://alias/").unwrap();
    for rows in [
        vec![entry("../other.md", Kind::File)],
        vec![entry("same.md", Kind::File), entry("same.md", Kind::File)],
    ] {
        assert!(list(&folder, &mut Offline::new(vec![rows]), || false).is_err());
    }
    let raw = Entry {
        name: b"invalid\xff.md".to_vec(),
        kind: Kind::File,
    };
    assert_eq!(raw.label(), "invalid\\xFF.md");
    assert_eq!(
        folder.markdown(&raw).unwrap().url(),
        "ssh://alias/invalid%FF.md"
    );
    let newline = entry("line\nname.md", Kind::File);
    assert_eq!(newline.label(), "line\\nname.md");
    assert_eq!(
        folder.markdown(&newline).unwrap().url(),
        "ssh://alias/line%0Aname.md"
    );
    assert!(folder.child_folder(&entry("../", Kind::Directory)).is_err());
}

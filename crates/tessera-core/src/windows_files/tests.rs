use super::*;
use std::fs;

fn fixture() -> (tempfile::TempDir, Directory) {
    let temp = tempfile::tempdir().unwrap();
    let directory = Directory::open(temp.path()).unwrap();
    (temp, directory)
}

#[test]
fn windows_save_lossless_utf8_and_retained_preimage() {
    let (temp, directory) = fixture();
    for (n, source) in [
        "\u{feff}---\r\ntitle: Привет\r\n---\r\nשלום 🧠\r\n",
        "English without newline",
    ]
    .into_iter()
    .enumerate()
    {
        let name = format!("note-{n}.md");
        directory
            .create(OsStr::new(&name), source.as_bytes())
            .unwrap();
        let proposed = format!("{source}תוספת / added\r\n");
        let Replacement::Saved { preimage } = directory
            .replace(OsStr::new(&name), source.as_bytes(), proposed.as_bytes())
            .unwrap()
        else {
            panic!("expected an acknowledged native replacement");
        };
        assert_eq!(
            fs::read(temp.path().join(name)).unwrap(),
            proposed.as_bytes()
        );
        assert_eq!(fs::read(preimage).unwrap(), source.as_bytes());
    }
}

#[test]
fn windows_save_external_change_and_replacement_race_are_conflicts() {
    let (temp, directory) = fixture();
    let path = temp.path().join("note.md");
    directory.create(OsStr::new("note.md"), b"base").unwrap();
    fs::write(&path, b"external").unwrap();
    assert!(matches!(
        directory
            .replace(OsStr::new("note.md"), b"base", b"mine")
            .unwrap(),
        Replacement::Conflict
    ));
    assert_eq!(fs::read(&path).unwrap(), b"external");
    fs::write(&path, b"base").unwrap();
    let result = directory
        .replace_before(OsStr::new("note.md"), b"base", b"mine", || {
            // A competing atomic publisher can replace the name, even while the
            // checked inode denies in-place writes. Exercise that reachable race.
            fs::rename(&path, temp.path().join("external-old.md")).unwrap();
            fs::write(&path, [0xff, 0xfe]).unwrap();
        })
        .unwrap();
    assert!(matches!(result, Replacement::Conflict));
    assert_eq!(fs::read(&path).unwrap(), [0xff, 0xfe]);
    assert!(
        fs::read_dir(temp.path())
            .unwrap()
            .any(|entry| fs::read(entry.unwrap().path()).is_ok_and(|bytes| bytes == b"mine")),
        "the proposed version remains recoverable"
    );
}

#[test]
fn windows_save_sharing_violation_keeps_source_and_proposed_bytes() {
    let (temp, directory) = fixture();
    let path = temp.path().join("note.md");
    directory.create(OsStr::new("note.md"), b"base").unwrap();
    let blocker = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .open(&path)
        .unwrap();
    let started = std::time::Instant::now();
    assert!(directory
        .replace(OsStr::new("note.md"), b"base", b"mine")
        .is_err());
    assert!(
        started.elapsed() >= Duration::from_millis(300),
        "sharing retry positive control"
    );
    assert_eq!(fs::read(&path).unwrap(), b"base");
    assert!(fs::read_dir(temp.path())
        .unwrap()
        .any(|entry| fs::read(entry.unwrap().path()).is_ok_and(|bytes| bytes == b"mine")));
    drop(blocker);
    assert!(matches!(
        directory
            .replace(OsStr::new("note.md"), b"base", b"mine")
            .unwrap(),
        Replacement::Saved { .. }
    ));
}

#[test]
fn windows_save_crash_before_acknowledgement_retains_draft_and_preimage() {
    let (temp, directory) = fixture();
    directory.create(OsStr::new("note.md"), b"base").unwrap();
    // The shared editor will own this journal. Here the native primitives
    // establish its persistence before publication and preserve the old note.
    directory
        .create(
            OsStr::new("draft.json"),
            br#"{"base":"base","text":"mine"}"#,
        )
        .unwrap();
    let Replacement::Saved { preimage } = directory
        .replace(OsStr::new("note.md"), b"base", b"mine")
        .unwrap()
    else {
        panic!("save");
    };
    drop(directory);
    let reopened = Directory::open(temp.path()).unwrap();
    assert_eq!(fs::read(temp.path().join("note.md")).unwrap(), b"mine");
    assert_eq!(
        fs::read(temp.path().join("draft.json")).unwrap(),
        br#"{"base":"base","text":"mine"}"#
    );
    assert_eq!(fs::read(preimage).unwrap(), b"base");
    drop(reopened);
}

#[test]
fn windows_save_create_and_rename_never_overwrite() {
    let (temp, directory) = fixture();
    directory.create(OsStr::new("a.md"), b"A").unwrap();
    directory.create(OsStr::new("b.md"), b"B").unwrap();
    assert!(directory
        .create(OsStr::new("b.md"), b"replacement")
        .is_err());
    assert!(directory
        .rename(OsStr::new("a.md"), &directory, OsStr::new("b.md"), b"A")
        .is_err());
    assert_eq!(fs::read(temp.path().join("a.md")).unwrap(), b"A");
    assert_eq!(fs::read(temp.path().join("b.md")).unwrap(), b"B");
    directory
        .rename(OsStr::new("a.md"), &directory, OsStr::new("שלום.md"), b"A")
        .unwrap();
    assert_eq!(fs::read(temp.path().join("שלום.md")).unwrap(), b"A");
    for name in [
        "CON.md",
        "note.md:stream",
        "note.md.",
        "LPT¹.md",
        "../escape.md",
    ] {
        assert!(
            directory.create(OsStr::new(name), b"unsafe").is_err(),
            "{name}"
        );
    }
}

#[test]
fn windows_save_parent_identity_hardlinks_and_readonly_fail_closed() {
    let temp = tempfile::tempdir().unwrap();
    let folder = temp.path().join("folder");
    fs::create_dir(&folder).unwrap();
    let directory = Directory::open(&folder).unwrap();
    assert!(
        fs::rename(&folder, temp.path().join("moved")).is_err(),
        "ancestor handle excludes parent rename"
    );
    directory.create(OsStr::new("note.md"), b"base").unwrap();
    let path = folder.join("note.md");
    fs::hard_link(&path, folder.join("alias.md")).unwrap();
    assert!(directory
        .replace(OsStr::new("note.md"), b"base", b"mine")
        .is_err());
    fs::remove_file(folder.join("alias.md")).unwrap();
    let original = fs::metadata(&path).unwrap().permissions();
    let mut permissions = original.clone();
    permissions.set_readonly(true);
    fs::set_permissions(&path, permissions.clone()).unwrap();
    assert!(directory
        .replace(OsStr::new("note.md"), b"base", b"mine")
        .is_err());
    assert_eq!(fs::read(&path).unwrap(), b"base");
    fs::set_permissions(&path, original).unwrap();
}

#[test]
fn windows_save_preserves_hidden_attributes() {
    let (temp, directory) = fixture();
    directory.create(OsStr::new("note.md"), b"base").unwrap();
    let path = temp.path().join("note.md");
    let text = wide(&path).unwrap();
    assert_ne!(
        unsafe {
            SetFileAttributesW(
                text.as_ptr(),
                FILE_ATTRIBUTE_HIDDEN | FILE_ATTRIBUTE_ARCHIVE,
            )
        },
        0
    );
    assert!(matches!(
        directory
            .replace(OsStr::new("note.md"), b"base", b"mine")
            .unwrap(),
        Replacement::Saved { .. }
    ));
    assert_ne!(
        unsafe { GetFileAttributesW(text.as_ptr()) } & FILE_ATTRIBUTE_HIDDEN,
        0
    );
}

fn dacl(path: &Path) -> Vec<u8> {
    use windows_sys::Win32::Security::{GetFileSecurityW, DACL_SECURITY_INFORMATION};
    let text = wide(path).unwrap();
    let mut size = 0;
    unsafe {
        GetFileSecurityW(
            text.as_ptr(),
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            0,
            &mut size,
        );
    }
    assert!(size > 0, "DACL size positive control");
    let mut bytes = vec![0; size as usize];
    assert_ne!(
        unsafe {
            GetFileSecurityW(
                text.as_ptr(),
                DACL_SECURITY_INFORMATION,
                bytes.as_mut_ptr().cast(),
                size,
                &mut size,
            )
        },
        0
    );
    bytes
}

#[test]
fn windows_save_preserves_custom_dacl_in_source_and_recovery() {
    use windows_sys::Win32::Security::{
        Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW, SetFileSecurityW,
        PROTECTED_DACL_SECURITY_INFORMATION,
    };
    let (temp, directory) = fixture();
    directory.create(OsStr::new("note.md"), b"base").unwrap();
    let path = temp.path().join("note.md");
    // Owner Rights gets full access; System only read access. This protected
    // DACL deliberately differs from the test directory's inherited grants.
    let text: Vec<_> = "D:P(A;;FA;;;OW)(A;;FR;;;SY)"
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let mut raw = std::ptr::null_mut();
    assert_ne!(
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                text.as_ptr(),
                1,
                &mut raw,
                std::ptr::null_mut(),
            )
        },
        0
    );
    let descriptor = Descriptor(raw);
    let name = wide(&path).unwrap();
    assert_ne!(
        unsafe {
            SetFileSecurityW(
                name.as_ptr(),
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                descriptor.0,
            )
        },
        0
    );
    let before = dacl(&path);
    assert_ne!(before, dacl(temp.path()), "custom ACL positive control");
    let Replacement::Saved { preimage } = directory
        .replace(OsStr::new("note.md"), b"base", b"mine")
        .unwrap()
    else {
        panic!("native save");
    };
    assert_eq!(dacl(&path), before);
    assert_eq!(dacl(&preimage), before);
}

#[test]
fn windows_save_reparse_source_and_parent_are_refused() {
    let temp = tempfile::tempdir().unwrap();
    let outside = temp.path().join("outside");
    let vault = temp.path().join("vault");
    fs::create_dir(&outside).unwrap();
    fs::create_dir(&vault).unwrap();
    fs::write(outside.join("original.md"), b"outside").unwrap();
    let junction = temp.path().join("junction");
    let status = std::process::Command::new("cmd.exe")
        .args(["/C", "mklink", "/J"])
        .arg(&junction)
        .arg(&outside)
        .status()
        .unwrap();
    assert!(status.success(), "native junction positive control");
    assert!(Directory::open(&junction).is_err());
    let directory = Directory::open(&vault).unwrap();
    std::os::windows::fs::symlink_file(outside.join("original.md"), vault.join("link.md")).expect(
        "native symbolic-link positive control requires Windows developer/admin privileges",
    );
    assert!(directory
        .replace(OsStr::new("link.md"), b"outside", b"mine")
        .is_err());
    assert_eq!(fs::read(outside.join("original.md")).unwrap(), b"outside");
    fs::remove_dir(&junction).unwrap();
}

#[test]
fn windows_save_cloud_tags_allow_resident_files_without_path_redirection() {
    for variant in 0..=15 {
        let tag = IO_REPARSE_TAG_CLOUD | (variant << 12);
        assert!(resident_cloud(tag, FILE_ATTRIBUTE_REPARSE_POINT, false));
        for unavailable in [
            FILE_ATTRIBUTE_OFFLINE,
            FILE_ATTRIBUTE_RECALL_ON_OPEN,
            FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS,
        ] {
            assert!(!resident_cloud(
                tag,
                FILE_ATTRIBUTE_REPARSE_POINT | unavailable,
                false
            ));
            assert!(resident_cloud(
                tag,
                FILE_ATTRIBUTE_REPARSE_POINT | unavailable,
                true
            ));
        }
    }
    // Name-surrogate tags and unknown non-surrogate tags fail closed.
    assert!(!resident_cloud(
        0xA000000C,
        FILE_ATTRIBUTE_REPARSE_POINT,
        false
    ));
    assert!(!resident_cloud(
        0xA0000003,
        FILE_ATTRIBUTE_REPARSE_POINT,
        true
    ));
    assert!(!resident_cloud(
        0x80000021,
        FILE_ATTRIBUTE_REPARSE_POINT,
        false
    ));
}

#[test]
fn windows_save_last_moment_symlink_cannot_modify_its_target() {
    let (temp, directory) = fixture();
    let path = temp.path().join("note.md");
    let outside = temp.path().join("outside.md");
    fs::write(&outside, b"outside").unwrap();
    directory.create(OsStr::new("note.md"), b"base").unwrap();
    let result = directory.replace_before(OsStr::new("note.md"), b"base", b"mine", || {
        fs::rename(&path, temp.path().join("old.md")).unwrap();
        std::os::windows::fs::symlink_file(&outside, &path)
            .expect("native symbolic-link positive control");
    });
    assert!(!matches!(result, Ok(Replacement::Saved { .. })));
    assert_eq!(fs::read(&outside).unwrap(), b"outside");
    assert_eq!(fs::read(temp.path().join("old.md")).unwrap(), b"base");
}

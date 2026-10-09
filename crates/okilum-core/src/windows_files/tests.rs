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
    let parent = temp.path().join("parent");
    let folder = parent.join("folder");
    fs::create_dir_all(&folder).unwrap();
    let directory = Directory::open(&folder).unwrap();
    for ancestor in [&folder, &parent] {
        let error = fs::rename(ancestor, temp.path().join("moved")).unwrap_err();
        assert_eq!(
            error.raw_os_error(),
            Some(ERROR_SHARING_VIOLATION as i32),
            "ancestor handle excludes parent rename: {error}"
        );
        let error = OpenOptions::new()
            .access_mode(DELETE)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(ancestor)
            .unwrap_err();
        assert_eq!(
            error.raw_os_error(),
            Some(ERROR_SHARING_VIOLATION as i32),
            "ancestor handle excludes delete access: {error}"
        );
        // Publication needs a writable parent handle in this same process.
        // The guard must deny DELETE sharing, while allowing this open.
        let writer = OpenOptions::new()
            .write(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(ancestor)
            .expect("ancestor guard must allow our writable parent open");
        drop(writer);
    }
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
    drop(directory);
    fs::rename(&parent, temp.path().join("moved")).unwrap();
    assert_eq!(
        fs::read(temp.path().join("moved/folder/note.md")).unwrap(),
        b"base",
        "rename succeeds after releasing the ancestor guard"
    );
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

#[derive(Debug, PartialEq, Eq)]
struct Permissions {
    owner: Vec<u8>,
    group: Vec<u8>,
    dacl_present: bool,
    dacl_protected: bool,
    // None distinguishes a null DACL (unrestricted) from an empty DACL.
    // ACE order, masks, flags and SIDs all affect access and must match.
    aces: Option<Vec<Vec<u8>>>,
}

fn permissions(path: &Path) -> Permissions {
    use windows_sys::Win32::Security::{
        GetAce, GetFileSecurityW, GetLengthSid, GetSecurityDescriptorControl,
        GetSecurityDescriptorDacl, GetSecurityDescriptorGroup, GetSecurityDescriptorOwner,
        IsValidSid, ACE_HEADER, DACL_SECURITY_INFORMATION, GROUP_SECURITY_INFORMATION,
        OWNER_SECURITY_INFORMATION, SE_DACL_PROTECTED,
    };
    let requested =
        DACL_SECURITY_INFORMATION | OWNER_SECURITY_INFORMATION | GROUP_SECURITY_INFORMATION;
    let text = wide(path).unwrap();
    let mut size = 0;
    unsafe {
        GetFileSecurityW(text.as_ptr(), requested, std::ptr::null_mut(), 0, &mut size);
    }
    assert!(size > 0, "DACL size positive control");
    let mut bytes = vec![0; size as usize];
    assert_ne!(
        unsafe {
            GetFileSecurityW(
                text.as_ptr(),
                requested,
                bytes.as_mut_ptr().cast(),
                size,
                &mut size,
            )
        },
        0
    );
    let descriptor = bytes.as_mut_ptr().cast();
    let mut owner = std::ptr::null_mut();
    let mut group = std::ptr::null_mut();
    let mut defaulted = 0;
    let mut present = 0;
    let mut control = 0;
    let mut revision = 0;
    let mut acl = std::ptr::null_mut();
    unsafe {
        assert_ne!(
            GetSecurityDescriptorOwner(descriptor, &mut owner, &mut defaulted),
            0
        );
        assert_ne!(
            GetSecurityDescriptorGroup(descriptor, &mut group, &mut defaulted),
            0
        );
        assert_ne!(
            GetSecurityDescriptorControl(descriptor, &mut control, &mut revision),
            0
        );
        assert_ne!(
            GetSecurityDescriptorDacl(descriptor, &mut present, &mut acl, &mut defaulted),
            0
        );
        assert_ne!(IsValidSid(owner), 0);
        assert_ne!(IsValidSid(group), 0);
        let aces = if acl.is_null() {
            None
        } else {
            let mut aces = Vec::new();
            for index in 0..u32::from((*acl).AceCount) {
                let mut ace = std::ptr::null_mut();
                assert_ne!(GetAce(acl, index, &mut ace), 0);
                let size = usize::from((*ace.cast::<ACE_HEADER>()).AceSize);
                assert!(size >= std::mem::size_of::<ACE_HEADER>());
                aces.push(std::slice::from_raw_parts(ace.cast::<u8>(), size).to_vec());
            }
            Some(aces)
        };
        Permissions {
            owner: std::slice::from_raw_parts(owner.cast::<u8>(), GetLengthSid(owner) as usize)
                .to_vec(),
            group: std::slice::from_raw_parts(group.cast::<u8>(), GetLengthSid(group) as usize)
                .to_vec(),
            dacl_present: present != 0,
            dacl_protected: control & SE_DACL_PROTECTED != 0,
            // SE_DACL_AUTO_INHERITED records Windows inheritance processing,
            // not a grant or the protected/unprotected inheritance policy.
            aces,
        }
    }
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
    let before = permissions(&path);
    assert!(before.dacl_protected, "protected DACL positive control");
    assert_ne!(
        before,
        permissions(temp.path()),
        "custom ACL positive control"
    );
    let plan = directory
        .prepare_replace(OsStr::new("note.md"), b"base", b"mine")
        .unwrap()
        .unwrap();
    assert_eq!(
        permissions(plan.prepared_path()),
        before,
        "proposed bytes retain the source permissions before publication"
    );
    let Replacement::Saved { preimage } = plan.commit().unwrap() else {
        panic!("native save");
    };
    assert_eq!(permissions(&path), before);
    assert_eq!(permissions(&preimage), before);
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

#[path = "tests/owner.rs"]
mod owner;

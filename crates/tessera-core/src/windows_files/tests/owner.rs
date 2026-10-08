//! Mandatory non-elevated controls: an elevated runner alone cannot catch #820.
use super::*;
use std::os::windows::fs::MetadataExt;
use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE},
    Security::{
        Authorization::{
            ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
        },
        CreateRestrictedToken, CreateWellKnownSid, GetLengthSid, GetTokenInformation,
        ImpersonateLoggedOnUser, RevertToSelf, SetFileSecurityW, SetTokenInformation, TokenOwner,
        TokenUser, WinBuiltinAdministratorsSid, DISABLE_MAX_PRIVILEGE, OWNER_SECURITY_INFORMATION,
        PROTECTED_DACL_SECURITY_INFORMATION, SID_AND_ATTRIBUTES, TOKEN_ALL_ACCESS, TOKEN_OWNER,
        TOKEN_USER,
    },
    System::Threading::{GetCurrentProcess, OpenProcessToken},
};

struct Token(HANDLE);
impl Drop for Token {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}
struct Impersonation;
impl Drop for Impersonation {
    fn drop(&mut self) {
        if unsafe { RevertToSelf() } == 0 {
            std::process::abort();
        }
    }
}
struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    state: PathBuf,
    user: Vec<u8>,
    admin: Vec<u8>,
    sid_text: String,
    limited: Token,
}
fn descriptor(sddl: &str) -> Descriptor {
    let text: Vec<_> = sddl.encode_utf16().chain(Some(0)).collect();
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
    Descriptor(raw)
}
fn apply_security(path: &Path, sddl: &str) {
    let descriptor = descriptor(sddl);
    assert_ne!(
        unsafe {
            SetFileSecurityW(
                wide(path).unwrap().as_ptr(),
                OWNER_SECURITY_INFORMATION
                    | DACL_SECURITY_INFORMATION
                    | PROTECTED_DACL_SECURITY_INFORMATION,
                descriptor.0,
            )
        },
        0,
        "fixture security: {}",
        std::io::Error::last_os_error()
    );
}
impl Fixture {
    fn new() -> Self {
        let mut raw = std::ptr::null_mut();
        assert_ne!(
            unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_ALL_ACCESS, &mut raw) },
            0
        );
        let token = Token(raw);
        let mut size = 0;
        unsafe {
            GetTokenInformation(token.0, TokenUser, std::ptr::null_mut(), 0, &mut size);
        }
        assert!(size > 0);
        let mut buffer = vec![0usize; (size as usize).div_ceil(std::mem::size_of::<usize>())];
        assert_ne!(
            unsafe {
                GetTokenInformation(
                    token.0,
                    TokenUser,
                    buffer.as_mut_ptr().cast(),
                    size,
                    &mut size,
                )
            },
            0
        );
        let user_sid = unsafe { (*buffer.as_ptr().cast::<TOKEN_USER>()).User.Sid };
        let user = unsafe {
            std::slice::from_raw_parts(user_sid.cast::<u8>(), GetLengthSid(user_sid) as usize)
                .to_vec()
        };
        let mut sid_text = std::ptr::null_mut();
        assert_ne!(
            unsafe { ConvertSidToStringSidW(user_sid, &mut sid_text) },
            0
        );
        let mut length = 0;
        unsafe {
            while *sid_text.add(length) != 0 {
                length += 1;
            }
        }
        let sid =
            unsafe { String::from_utf16(std::slice::from_raw_parts(sid_text, length)).unwrap() };
        unsafe {
            LocalFree(sid_text.cast());
        }
        let mut admin = [0usize; 16];
        let mut size = std::mem::size_of_val(&admin) as u32;
        assert_ne!(
            unsafe {
                CreateWellKnownSid(
                    WinBuiltinAdministratorsSid,
                    std::ptr::null_mut(),
                    admin.as_mut_ptr().cast(),
                    &mut size,
                )
            },
            0
        );
        let disabled = SID_AND_ATTRIBUTES {
            Sid: admin.as_mut_ptr().cast(),
            Attributes: 0,
        };
        let mut restricted = std::ptr::null_mut();
        // Administrators becomes deny-only. All privileges except traverse are
        // disabled, including take-ownership/restore. No restricted SID list:
        // an ordinary user's explicit file grant must remain effective.
        assert_ne!(
            unsafe {
                CreateRestrictedToken(
                    token.0,
                    DISABLE_MAX_PRIVILEGE,
                    1,
                    &disabled,
                    0,
                    std::ptr::null(),
                    0,
                    std::ptr::null(),
                    &mut restricted,
                )
            },
            0
        );
        let limited = Token(restricted);
        let owner = TOKEN_OWNER { Owner: user_sid };
        assert_ne!(
            unsafe {
                SetTokenInformation(
                    limited.0,
                    TokenOwner,
                    (&owner as *const TOKEN_OWNER).cast(),
                    std::mem::size_of::<TOKEN_OWNER>() as u32,
                )
            },
            0,
            "ordinary-user default owner: {}",
            std::io::Error::last_os_error()
        );
        let temp = tempfile::tempdir().unwrap();
        // Grant the explicit user, not Administrators, access to all fixture
        // directories (including draft/history state) under impersonation.
        apply_security(
            temp.path(),
            &format!("O:{sid}D:P(A;OICI;FA;;;{sid})(A;OICI;FA;;;SY)"),
        );
        let root = temp.path().join("vault");
        let state = temp.path().join("state");
        fs::create_dir(&root).unwrap();
        fs::create_dir(&state).unwrap();
        let admin = unsafe {
            std::slice::from_raw_parts(admin.as_ptr().cast::<u8>(), size as usize).to_vec()
        };
        Self {
            _temp: temp,
            root,
            state,
            user,
            admin,
            sid_text: sid,
            limited,
        }
    }
    fn source(&self, name: &str, text: &str) -> PathBuf {
        let path = self.root.join(name);
        fs::write(&path, text).unwrap();
        apply_security(
            &path,
            &format!("O:BAD:P(A;;FA;;;{})(A;;FR;;;SY)(A;;FR;;;OW)", self.sid_text),
        );
        assert_eq!(
            permissions(&path).owner,
            self.admin,
            "Administrators owner positive control"
        );
        path
    }
    fn impersonate(&self) -> Impersonation {
        assert_ne!(unsafe { ImpersonateLoggedOnUser(self.limited.0) }, 0);
        let guard = Impersonation;
        // Mandatory negative control: the limited token must actually reject
        // assigning Administrators, despite running inside an elevated CI host.
        let path = self.root.join("owner-negative-control.md");
        let sd = descriptor(&format!("O:BAD:P(A;;FA;;;{})", self.sid_text));
        let attrs = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: sd.0,
            bInheritHandle: 0,
        };
        let raw = unsafe {
            CreateFileW(
                wide(&path).unwrap().as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ,
                &attrs,
                CREATE_NEW,
                FILE_ATTRIBUTE_NORMAL,
                std::ptr::null_mut(),
            )
        };
        let error = std::io::Error::last_os_error();
        if raw != INVALID_HANDLE_VALUE {
            unsafe {
                CloseHandle(raw);
            }
        }
        assert_eq!(
            raw, INVALID_HANDLE_VALUE,
            "non-elevated owner assignment must fail"
        );
        assert_eq!(error.raw_os_error(), Some(ERROR_INVALID_OWNER as i32));
        assert!(!path.exists(), "failed creation must publish no file");
        eprintln!("Windows owner fixture: restricted token active; CreateFileW rejected Administrators owner with ERROR_INVALID_OWNER (1307)");
        guard
    }
}
fn assert_access_and_dacl(path: &Path, before: &Permissions) {
    let after = permissions(path);
    assert_eq!(after.dacl_present, before.dacl_present);
    assert_eq!(after.dacl_protected, before.dacl_protected);
    assert_eq!(after.aces, before.aces, "no ACE/order/mask/flag changes");
    // These opens run under the limited token, so effective access is exercised
    // both before and after owner fallback, not inferred from a matching ACL.
    OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .expect("effective read/write access");
}
#[test]
fn windows_save_administrators_owner_with_restricted_token() {
    let fixture = Fixture::new();
    let path = fixture.source("note.md", "base");
    let before = permissions(&path);
    assert!(before.dacl_protected);
    assert_ne!(
        unsafe { SetFileAttributesW(wide(&path).unwrap().as_ptr(), FILE_ATTRIBUTE_HIDDEN) },
        0
    );
    let _limited = fixture.impersonate();
    assert_access_and_dacl(&path, &before);
    let directory = Directory::open(&fixture.root).unwrap();
    let plan = directory
        .prepare_replace(OsStr::new("note.md"), b"base", b"mine")
        .unwrap()
        .unwrap();
    assert_eq!(
        permissions(plan.prepared_path()).owner,
        fixture.user,
        "fallback uses current user before writing proposed bytes"
    );
    assert_access_and_dacl(plan.prepared_path(), &before);
    let Replacement::Saved { preimage } = plan.commit().unwrap() else {
        panic!("native replacement");
    };
    assert_eq!(fs::read(&path).unwrap(), b"mine");
    assert_access_and_dacl(&path, &before);
    assert_eq!(permissions(&preimage), before);
    assert!(fs::metadata(&path).unwrap().file_attributes() & FILE_ATTRIBUTE_HIDDEN != 0);
}
#[test]
fn windows_editor_administrators_owner_rename_preserves_access_and_links() {
    use crate::{
        file_editor::{FileEditor, Save},
        link_rewrite::Preview,
    };
    let fixture = Fixture::new();
    let source = fixture.source("qa-source.md", "[[qa-source]] [self](qa-source.md)\n");
    let refs = fixture.source(
        "refs.md",
        "[[qa-source]] [[qa-source|alias]] [relative](qa-source.md)\n",
    );
    let source_permissions = permissions(&source);
    let refs_permissions = permissions(&refs);
    let _limited = fixture.impersonate();
    let drafts = fixture.state.join("editor-drafts");
    let mut editor = FileEditor::open(&source, &drafts).unwrap();
    editor
        .set_text("[[qa-source]] [self](qa-source.md)\nEdit\n".into())
        .unwrap();
    assert_eq!(editor.save().unwrap(), Save::Saved);
    drop(editor);
    let preview = Preview::prepare(&fixture.root, "qa-source.md", "qa-target-w.md").unwrap();
    let result = preview
        .apply(
            &fixture.root,
            &fixture.state,
            &mut std::collections::BTreeMap::new(),
        )
        .unwrap();
    assert!(result.moved, "{:?}", result.warning);
    assert!(!source.exists());
    let target = fixture.root.join("qa-target-w.md");
    assert_eq!(
        fs::read_to_string(&target).unwrap(),
        "[[qa-target-w]] [self](./qa-target-w.md)\nEdit\n"
    );
    assert_eq!(
        fs::read_to_string(&refs).unwrap(),
        "[[qa-target-w]] [[qa-target-w|alias]] [relative](./qa-target-w.md)\n"
    );
    assert_access_and_dacl(&target, &source_permissions);
    assert_access_and_dacl(&refs, &refs_permissions);
}
#[test]
fn windows_editor_failed_rename_restores_disk_links_and_drafts() {
    use crate::{file_editor::FileEditor, link_rewrite::Preview};
    let fixture = Fixture::new();
    let source = fixture.source("source.md", "# Source\n");
    let refs = fixture.source("0-refs.md", "[[source]] [relative](source.md)\n");
    let _limited = fixture.impersonate();
    let before = fs::read_to_string(&refs).unwrap();
    let preview = Preview::prepare(&fixture.root, "source.md", "target.md").unwrap();
    // Permit reading and writing, deny DELETE: references save first, and the
    // final source rename is refused. Rollback must undo those link writes.
    let locked = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .open(&source)
        .unwrap();
    let result = preview
        .apply(
            &fixture.root,
            &fixture.state,
            &mut std::collections::BTreeMap::new(),
        )
        .unwrap();
    assert!(!result.moved);
    assert!(source.exists());
    assert!(!fixture.root.join("target.md").exists());
    assert_eq!(fs::read_to_string(&refs).unwrap(), before);
    let drafts = fixture.state.join("editor-drafts");
    let editor = FileEditor::open(&refs, &drafts).unwrap();
    assert_eq!(editor.text(), before);
    assert!(!editor.dirty());
    drop(editor);
    drop(locked);
    // A retry must not fail with stale/generated editor text.
    assert!(
        Preview::prepare(&fixture.root, "source.md", "target.md")
            .unwrap()
            .apply(
                &fixture.root,
                &fixture.state,
                &mut std::collections::BTreeMap::new()
            )
            .unwrap()
            .moved
    );
}

#[test]
fn windows_editor_failed_link_save_restores_original_draft_and_allows_retry() {
    use crate::{file_editor::FileEditor, link_rewrite::Preview};
    let fixture = Fixture::new();
    fs::create_dir(fixture.root.join("sub")).unwrap();
    let source = fixture.source("source.md", "[[source]] [self](source.md)\n");
    let before = fs::read_to_string(&source).unwrap();
    let _limited = fixture.impersonate();
    let locked = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .open(&source)
        .unwrap();
    let preview = Preview::prepare(&fixture.root, "source.md", "sub/target.md").unwrap();
    let result = preview
        .apply(
            &fixture.root,
            &fixture.state,
            &mut std::collections::BTreeMap::new(),
        )
        .unwrap();
    assert!(!result.moved);
    assert_eq!(fs::read_to_string(&source).unwrap(), before);
    assert!(!fixture.root.join("sub/target.md").exists());
    let editor = FileEditor::open(&source, &fixture.state.join("editor-drafts")).unwrap();
    assert_eq!(editor.text(), before);
    assert!(!editor.dirty());
    drop(editor);
    drop(locked);
    assert!(
        Preview::prepare(&fixture.root, "source.md", "sub/target.md")
            .unwrap()
            .apply(
                &fixture.root,
                &fixture.state,
                &mut std::collections::BTreeMap::new()
            )
            .unwrap()
            .moved
    );
}

#[test]
fn windows_save_owner_fallback_refuses_effective_access_loss_before_publication() {
    let fixture = Fixture::new();
    let path = fixture.source("note.md", "base");
    // The user can write as a non-owner, but becoming owner would activate a
    // deny ACE. Keeping the ACL unchanged must never publish an unusable note.
    apply_security(
        &path,
        &format!(
            "O:BAD:P(D;;FW;;;OW)(A;;FA;;;{})(A;;FR;;;SY)",
            fixture.sid_text
        ),
    );
    // FILE_GENERIC_WRITE also contains READ_CONTROL. The elevated fixture
    // owner therefore cannot inspect this ACL after installing the deny ACE.
    // Prove it is active, then read/check the source as the non-owner user who
    // is actually allowed to edit it before fallback changes ownership.
    let denied = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .unwrap_err();
    assert_eq!(denied.kind(), std::io::ErrorKind::PermissionDenied);
    let _limited = fixture.impersonate();
    let before = permissions(&path);
    assert_eq!(before.owner, fixture.admin);
    assert_access_and_dacl(&path, &before);
    let directory = Directory::open(&fixture.root).unwrap();
    assert!(directory
        .prepare_replace(OsStr::new("note.md"), b"base", b"mine")
        .is_err());
    assert_eq!(fs::read(&path).unwrap(), b"base");
    assert_eq!(permissions(&path), before);
    assert_access_and_dacl(&path, &before);
}

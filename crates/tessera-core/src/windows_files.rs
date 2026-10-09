//! Native NTFS mutation primitives for desktop source editing.
//!
//! Callers must persist their revision-aware draft before replacement and keep
//! it until this module reports Saved. All prepared/displaced files survive an
//! error; cleanup belongs to the durable history/recovery owner, never Drop.
use anyhow::{bail, ensure, Context, Result};
use std::{
    ffi::OsStr,
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    os::windows::{
        ffi::OsStrExt,
        fs::OpenOptionsExt,
        io::{AsRawHandle, FromRawHandle},
    },
    path::{Component, Path, PathBuf, Prefix},
    time::Duration,
};
use windows_sys::Win32::{
    Foundation::{
        LocalFree, ERROR_INVALID_OWNER, ERROR_LOCK_VIOLATION, ERROR_SHARING_VIOLATION,
        GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE,
    },
    Security::{
        Authorization::{GetSecurityInfo, SetSecurityInfo, SE_FILE_OBJECT},
        GetAce, GetSecurityDescriptorControl, GetSecurityDescriptorDacl,
        InitializeSecurityDescriptor, SetSecurityDescriptorControl, SetSecurityDescriptorDacl,
        ACE_HEADER, DACL_SECURITY_INFORMATION, GROUP_SECURITY_INFORMATION,
        OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
        SECURITY_ATTRIBUTES, SECURITY_DESCRIPTOR, SE_DACL_AUTO_INHERITED, SE_DACL_AUTO_INHERIT_REQ,
        SE_DACL_PROTECTED, UNPROTECTED_DACL_SECURITY_INFORMATION,
    },
    Storage::FileSystem::*,
    System::{
        SystemServices::{IO_REPARSE_TAG_CLOUD, IO_REPARSE_TAG_CLOUD_MASK},
        WindowsProgramming::DRIVE_FIXED,
    },
};

fn wide(path: &Path) -> Result<Vec<u16>> {
    let mut text: Vec<_> = path.as_os_str().encode_wide().collect();
    ensure!(!text.contains(&0), "A file path contains a NUL");
    text.push(0);
    Ok(text)
}
fn retry<T>(mut operation: impl FnMut() -> std::io::Result<T>) -> std::io::Result<T> {
    for delay in [20, 40, 80, 160] {
        match operation() {
            Err(error) if matches!(error.raw_os_error(), Some(code) if code == ERROR_SHARING_VIOLATION as i32 || code == ERROR_LOCK_VIOLATION as i32) =>
            {
                std::thread::sleep(Duration::from_millis(delay));
            }
            result => return result,
        }
    }
    operation()
}
pub(crate) fn information(file: &File) -> Result<BY_HANDLE_FILE_INFORMATION> {
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    // File owns the handle for the complete call; info has the required layout.
    ensure!(
        unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } != 0,
        "Read native file identity: {}",
        std::io::Error::last_os_error()
    );
    Ok(info)
}
fn plain(info: &BY_HANDLE_FILE_INFORMATION, directory: bool, file: &File) -> Result<()> {
    if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        let mut tag = FILE_ATTRIBUTE_TAG_INFO::default();
        ensure!(
            unsafe {
                GetFileInformationByHandleEx(
                    file.as_raw_handle(),
                    FileAttributeTagInfo,
                    (&mut tag as *mut FILE_ATTRIBUTE_TAG_INFO).cast(),
                    std::mem::size_of::<FILE_ATTRIBUTE_TAG_INFO>() as u32,
                )
            } != 0,
            "Read reparse identity: {}",
            std::io::Error::last_os_error()
        );
        // Hydrated FileProvider files are reparse points too, but CLOUD tags
        // never redirect a name. Reject symlinks, junctions and unknown tags;
        // permit only resident cloud files, without initiating a download.
        ensure!(
            resident_cloud(tag.ReparseTag, info.dwFileAttributes, directory),
            "Redirecting reparse points or undownloaded cloud notes are not editable"
        );
    }
    ensure!(
        (info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0) == directory,
        "Unexpected file type"
    );
    if !directory {
        ensure!(
            info.nNumberOfLinks == 1,
            "Hard-linked notes are not editable"
        );
    }
    Ok(())
}
fn resident_cloud(tag: u32, attributes: u32, directory: bool) -> bool {
    tag & !IO_REPARSE_TAG_CLOUD_MASK == IO_REPARSE_TAG_CLOUD
        && (directory
            || attributes
                & (FILE_ATTRIBUTE_OFFLINE
                    | FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS
                    | FILE_ATTRIBUTE_RECALL_ON_OPEN)
                == 0)
}
pub(crate) fn open_regular(path: &Path) -> Result<(File, BY_HANDLE_FILE_INFORMATION)> {
    let file = retry(|| {
        OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_DELETE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
    })?;
    let info = information(&file)?;
    plain(&info, false, &file)?;
    Ok((file, info))
}
pub(crate) fn read_file(path: &Path) -> Result<(File, Vec<u8>, BY_HANDLE_FILE_INFORMATION)> {
    let (mut file, info) = open_regular(path)?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok((file, bytes, info))
}

/// Keeps every ancestor open without DELETE sharing: neither a parent nor the
/// directory itself can be renamed/replaced during an operation. Files remain
/// visible to ordinary child-file readers/writers. Directory WRITE sharing is
/// required for our own child-file publication operations too.
pub struct Directory {
    path: PathBuf,
    _ancestors: Vec<File>,
}
impl Directory {
    pub(crate) fn identities(&self) -> Result<Vec<(u32, u32, u32)>> {
        self._ancestors
            .iter()
            .map(|file| information(file).map(|info| identity(&info)))
            .collect()
    }
    pub(crate) fn information(&self) -> Result<BY_HANDLE_FILE_INFORMATION> {
        information(self._ancestors.last().unwrap())
    }
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
    pub(crate) fn read(&self, name: &OsStr) -> Result<(File, Vec<u8>, BY_HANDLE_FILE_INFORMATION)> {
        read_file(&self.child(name)?)
    }
    pub(crate) fn open_file(&self, name: &OsStr) -> Result<File> {
        Ok(open_regular(&self.child(name)?)?.0)
    }
    pub(crate) fn create_directory(&self, name: &OsStr) -> Result<()> {
        let destination = self.child(name)?;
        let prepared = self
            .path
            .join(format!(".tessera-create-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&prepared)?;
        // WRITE_THROUGH publication and NOREPLACE apply to folders as well.
        move_no_replace(&prepared, &destination)
    }
    pub fn open(path: &Path) -> Result<Self> {
        ensure!(path.is_absolute(), "Expected an absolute directory");
        let mut components = path.components();
        let Some(Component::Prefix(prefix)) = components.next() else {
            bail!("Missing local drive");
        };
        ensure!(
            matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_)),
            "Network paths are outside the NTFS editing backend"
        );
        ensure!(
            matches!(components.next(), Some(Component::RootDir)),
            "Expected a drive root"
        );
        let mut current = PathBuf::from(prefix.as_os_str());
        current.push(std::path::MAIN_SEPARATOR.to_string());
        let drive = wide(&current)?;
        // Reject mapped network drives too: SMB may report an NTFS volume name.
        ensure!(
            unsafe { GetDriveTypeW(drive.as_ptr()) } == DRIVE_FIXED,
            "Editing requires a local fixed NTFS volume"
        );
        let mut ancestors = vec![Self::pin(&current)?];
        let mut filesystem = [0u16; 32];
        let ok = unsafe {
            GetVolumeInformationByHandleW(
                ancestors[0].as_raw_handle(),
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                filesystem.as_mut_ptr(),
                filesystem.len() as u32,
            )
        };
        ensure!(
            ok != 0,
            "Read filesystem type: {}",
            std::io::Error::last_os_error()
        );
        let end = filesystem
            .iter()
            .position(|c| *c == 0)
            .unwrap_or(filesystem.len());
        ensure!(
            String::from_utf16_lossy(&filesystem[..end]).eq_ignore_ascii_case("NTFS"),
            "Editing requires NTFS"
        );
        for component in components {
            let Component::Normal(name) = component else {
                bail!("Invalid directory component");
            };
            current.push(name);
            ancestors.push(Self::pin(&current)?);
        }
        Ok(Self {
            path: current,
            _ancestors: ancestors,
        })
    }
    fn pin(path: &Path) -> Result<File> {
        let file = retry(|| {
            OpenOptions::new()
                .read(true)
                // Metadata-only handles do not participate in sharing checks.
                // Directory read access makes the omitted DELETE share exclude
                // ancestor rename. Keep WRITE sharing: publication itself may
                // open a parent for FILE_ADD_FILE, even in this process.
                .access_mode(FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES)
                .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
                .open(path)
        })?;
        plain(&information(&file)?, true, &file)?;
        Ok(file)
    }
    pub(crate) fn child(&self, name: &OsStr) -> Result<PathBuf> {
        let mut components = Path::new(name).components();
        ensure!(
            matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none(),
            "Expected one filename"
        );
        // Refuse ADS, aliases produced by Win32 trimming, and device names.
        let text = name.to_str().context("A filename is not Unicode")?;
        ensure!(
            !text
                .chars()
                .any(|c| c.is_control() || "<>:\"/\\|?*".contains(c))
                && !text.ends_with([' ', '.']),
            "Invalid Windows filename"
        );
        let stem = text
            .split('.')
            .next()
            .unwrap_or("")
            .trim_end_matches(' ')
            .to_ascii_uppercase();
        ensure!(
            !["CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$"].contains(&stem.as_str())
                && !(stem.starts_with("COM") || stem.starts_with("LPT"))
                    .then(|| &stem[3..])
                    .is_some_and(
                        |n| ["1", "2", "3", "4", "5", "6", "7", "8", "9", "¹", "²", "³"]
                            .contains(&n)
                    ),
            "Reserved Windows filename"
        );
        Ok(self.path.join(name))
    }
    fn prepared(&self, bytes: &[u8], security_source: Option<&File>) -> Result<PathBuf> {
        let path = self
            .path
            .join(format!(".tessera-save-{}.prepared", uuid::Uuid::new_v4()));
        let descriptor = security_source.map(Descriptor::from_file).transpose()?;
        let mut attrs = descriptor.as_ref().map(|descriptor| SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor.0,
            bInheritHandle: 0,
        });
        let encrypted = security_source
            .map(information)
            .transpose()?
            .map_or(0, |info| info.dwFileAttributes & FILE_ATTRIBUTE_ENCRYPTED);
        let text = wide(&path)?;
        // Apply the source DACL and encryption at creation, before exposing any draft bytes.
        let create = |attrs: &Option<SECURITY_ATTRIBUTES>| unsafe {
            CreateFileW(
                text.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ,
                attrs.as_ref().map_or(std::ptr::null(), |a| a as *const _),
                CREATE_NEW,
                FILE_FLAG_WRITE_THROUGH
                    | if encrypted == 0 {
                        FILE_ATTRIBUTE_NORMAL
                    } else {
                        encrypted
                    },
                std::ptr::null_mut(),
            )
        };
        let mut raw = create(&attrs);
        // Source ownership is not an access grant. A non-elevated token
        // cannot assign Administrators as the replacement file owner.
        // Retry creation with exactly the same ACL/protection but token-default
        // owner/group. No draft bytes have been exposed, and other errors retain
        // their normal failure behavior (never fall back to inherited access).
        let mut dacl_only;
        let mut owner_fallback = false;
        if raw == INVALID_HANDLE_VALUE
            && std::io::Error::last_os_error().raw_os_error() == Some(ERROR_INVALID_OWNER as i32)
        {
            dacl_only = descriptor
                .as_ref()
                .context("Missing source DACL")?
                .dacl_only()?;
            attrs.as_mut().unwrap().lpSecurityDescriptor =
                (&mut dacl_only as *mut SECURITY_DESCRIPTOR).cast();
            raw = create(&attrs);
            owner_fallback = true;
        }
        if raw == INVALID_HANDLE_VALUE {
            return Err(std::io::Error::last_os_error()).context("Prepare save file");
        }
        let mut file = unsafe { File::from_raw_handle(raw) };
        if owner_fallback {
            // Creation can grant the creator a handle even when OWNER RIGHTS
            // ACEs deny a later open under the new owner. Exercise effective
            // access before writing bytes or publishing a canonical replacement.
            let created = identity(&information(&file)?);
            drop(file);
            file = OpenOptions::new()
                .read(true)
                .write(true)
                .share_mode(FILE_SHARE_READ)
                .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_WRITE_THROUGH)
                .open(&path)
                .context(
                    "The replacement owner cannot retain read/write access; source was not changed",
                )?;
            let reopened = information(&file)?;
            plain(&reopened, false, &file)?;
            ensure!(
                identity(&reopened) == created,
                "Prepared file changed before permission validation"
            );
        }
        ensure!(
            encrypted == 0 || information(&file)?.dwFileAttributes & FILE_ATTRIBUTE_ENCRYPTED != 0,
            "The encrypted source needs an encrypted recovery file; no proposed bytes were written"
        );
        file.write_all(bytes)?;
        file.sync_all()?;
        // No path destructor: the complete proposed bytes remain recoverable
        // if publication or a later durability check fails.
        Ok(path)
    }
    /// Publishes a complete new file without replacing a concurrent creator.
    pub fn create(&self, name: &OsStr, bytes: &[u8]) -> Result<()> {
        let destination = self.child(name)?;
        let prepared = self.prepared(bytes, None)?;
        move_no_replace(&prepared, &destination).context("Publish new file without overwrite")?;
        Ok(())
    }
    /// Performs a same-volume, no-replace rename. The expected bytes are checked
    /// while a read handle excludes in-place writers. A competing replacement
    /// is detected after the move; both paths/versions are retained for recovery.
    pub fn rename(
        &self,
        name: &OsStr,
        destination: &Directory,
        new_name: &OsStr,
        expected: &[u8],
    ) -> Result<()> {
        let source = self.child(name)?;
        let target = destination.child(new_name)?;
        ensure!(!target.try_exists()?, "The destination already exists");
        let (guard, bytes, info) = read_file(&source)?;
        ensure!(
            bytes == expected,
            "The note changed; refresh the rename preview"
        );
        ensure!(
            information(destination._ancestors.last().unwrap())?.dwVolumeSerialNumber
                == info.dwVolumeSerialNumber,
            "Cross-volume moves are not supported"
        );
        move_no_replace(&source, &target)?;
        let (_, moved, after) = read_file(&target)?;
        ensure!(moved == expected && identity(&info) == identity(&after), "The moved source changed; inspect the destination. No automatic rollback was attempted");
        drop(guard);
        Ok(())
    }
    /// Prepare and flush proposed bytes, exposing the planned recovery names
    /// before canonical mutation. Persist draft/history with these identities
    /// before calling commit; dropping a plan preserves its prepared file.
    pub fn prepare_replace(
        &self,
        name: &OsStr,
        expected: &[u8],
        proposed: &[u8],
    ) -> Result<Option<PreparedReplacement<'_>>> {
        let path = self.child(name)?;
        let (guard, bytes, info) = read_file(&path)?;
        if bytes != expected {
            return Ok(None);
        }
        ensure!(
            info.dwFileAttributes & FILE_ATTRIBUTE_READONLY == 0,
            "The note is not writable; your draft must remain protected"
        );
        let prepared = self.prepared(proposed, Some(&guard))?;
        let backup = self
            .path
            .join(format!(".tessera-save-{}.previous", uuid::Uuid::new_v4()));
        // ReplaceFile preserves DACL, creation time and streams; never opt out
        // of merge errors. Carry the ordinary user-visible attribute bits too.
        let attributes = info.dwFileAttributes
            & (FILE_ATTRIBUTE_ARCHIVE
                | FILE_ATTRIBUTE_HIDDEN
                | FILE_ATTRIBUTE_SYSTEM
                | FILE_ATTRIBUTE_NOT_CONTENT_INDEXED);
        let text = wide(&prepared)?;
        ensure!(
            unsafe {
                SetFileAttributesW(
                    text.as_ptr(),
                    if attributes == 0 {
                        FILE_ATTRIBUTE_NORMAL
                    } else {
                        attributes
                    },
                )
            } != 0,
            "Prepare file attributes: {}",
            std::io::Error::last_os_error()
        );
        Ok(Some(PreparedReplacement {
            directory: self,
            path,
            guard,
            prepared,
            backup,
            expected: expected.to_vec(),
            proposed: proposed.to_vec(),
            info,
        }))
    }
    #[cfg(test)]
    fn replace(&self, name: &OsStr, expected: &[u8], proposed: &[u8]) -> Result<Replacement> {
        self.replace_before(name, expected, proposed, || {})
    }
    #[cfg(test)]
    fn replace_before(
        &self,
        name: &OsStr,
        expected: &[u8],
        proposed: &[u8],
        before: impl FnOnce(),
    ) -> Result<Replacement> {
        match self.prepare_replace(name, expected, proposed)? {
            Some(plan) => plan.commit_before(before),
            None => Ok(Replacement::Conflict),
        }
    }
}

pub struct PreparedReplacement<'a> {
    directory: &'a Directory,
    path: PathBuf,
    guard: File,
    prepared: PathBuf,
    backup: PathBuf,
    expected: Vec<u8>,
    proposed: Vec<u8>,
    info: BY_HANDLE_FILE_INFORMATION,
}
impl PreparedReplacement<'_> {
    pub fn note_path(&self) -> &Path {
        &self.path
    }
    pub fn prepared_path(&self) -> &Path {
        &self.prepared
    }
    pub fn preimage_path(&self) -> &Path {
        &self.backup
    }
    pub(crate) fn preimage_identity(&self) -> (u64, u64) {
        let (volume, high, low) = identity(&self.info);
        (u64::from(volume), u64::from(high) << 32 | u64::from(low))
    }
    /// The caller must have completed durable draft/history persistence first.
    pub fn commit(self) -> Result<Replacement> {
        self.commit_before(|| {})
    }
    fn commit_before(self, before: impl FnOnce()) -> Result<Replacement> {
        let Self {
            directory,
            path,
            guard,
            prepared,
            backup,
            expected,
            proposed,
            info,
        } = self;
        ensure!(
            !backup.try_exists()?,
            "A recovery destination already exists"
        );
        // The published permission repair must never target a concurrent
        // replacement, even one with exactly the same proposed text.
        let prepared_identity = identity(&open_regular(&prepared)?.1);
        before();
        replace(&path, &prepared, &backup)?;
        let (displaced_guard, displaced, displaced_info) = read_file(&backup)?;
        if displaced != expected || identity(&displaced_info) != identity(&info) {
            drop(displaced_guard);
            // Reversal also keeps the displaced proposed/late-racing source.
            // Never delete either version, including a writer racing reversal.
            let recovery = directory
                .path
                .join(format!(".tessera-save-{}.raced", uuid::Uuid::new_v4()));
            replace(&path, &backup, &recovery).context("Save raced with another replacement; all surviving recovery files have been retained")?;
            return Ok(Replacement::Conflict);
        }
        let source_security = Descriptor::from_file(&displaced_guard)?;
        drop(displaced_guard);
        drop(guard);
        // REPLACEFILE_WRITE_THROUGH is unsupported by Windows. Flush the file
        // before and after replacement instead of claiming that flag works.
        let mut committed = retry(|| {
            OpenOptions::new()
                .read(true)
                .write(true)
                .share_mode(FILE_SHARE_READ)
                .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
                .open(&path)
        })?;
        let committed_info = information(&committed)?;
        plain(&committed_info, false, &committed)?;
        if identity(&committed_info) != prepared_identity {
            return Ok(Replacement::Conflict);
        }
        committed.seek(SeekFrom::Start(0))?;
        let mut actual = Vec::new();
        committed.read_to_end(&mut actual)?;
        if actual != proposed {
            return Ok(Replacement::Conflict);
        }
        // ReplaceFileW can add an explicit grant for the displaced owner when
        // fallback ownership differs. Restore only the DACL, through a handle
        // checked against this publication; never change owner or group here.
        source_security.restore_dacl(&path, &committed).context(
            "The replacement completed but exact source permissions could not be confirmed; retain draft and recovery files",
        )?;
        committed.sync_all().context("The replacement completed but durability could not be confirmed; retain draft and recovery files")?;
        Ok(Replacement::Saved { preimage: backup })
    }
}

struct Descriptor(PSECURITY_DESCRIPTOR);
#[derive(PartialEq, Eq)]
struct Dacl {
    present: bool,
    protected: bool,
    aces: Option<Vec<Vec<u8>>>,
}
impl Descriptor {
    fn dacl(&self) -> Result<Dacl> {
        let mut present = 0;
        let mut defaulted = 0;
        let mut acl = std::ptr::null_mut();
        let mut control = 0;
        let mut revision = 0;
        ensure!(
            unsafe {
                GetSecurityDescriptorDacl(self.0, &mut present, &mut acl, &mut defaulted) != 0
                    && GetSecurityDescriptorControl(self.0, &mut control, &mut revision) != 0
            },
            "Read source DACL: {}",
            std::io::Error::last_os_error()
        );
        let aces = if acl.is_null() {
            None
        } else {
            let mut aces = Vec::new();
            for index in 0..u32::from(unsafe { (*acl).AceCount }) {
                let mut ace = std::ptr::null_mut();
                ensure!(
                    unsafe { GetAce(acl, index, &mut ace) } != 0,
                    "Read source ACE: {}",
                    std::io::Error::last_os_error()
                );
                let size = usize::from(unsafe { (*ace.cast::<ACE_HEADER>()).AceSize });
                ensure!(
                    size >= std::mem::size_of::<ACE_HEADER>(),
                    "Invalid source ACE"
                );
                aces.push(unsafe { std::slice::from_raw_parts(ace.cast::<u8>(), size) }.to_vec());
            }
            Some(aces)
        };
        Ok(Dacl {
            present: present != 0,
            protected: control & SE_DACL_PROTECTED != 0,
            aces,
        })
    }
    fn restore_dacl(&self, path: &Path, committed: &File) -> Result<()> {
        let expected = self.dacl()?;
        if Descriptor::from_file(committed)?.dacl()? == expected {
            return Ok(());
        }
        // committed excludes writers and DELETE sharing. The additional
        // security-only handle cannot redirect to another publication.
        let security = OpenOptions::new()
            .access_mode(READ_CONTROL | WRITE_DAC | FILE_READ_ATTRIBUTES)
            .share_mode(FILE_SHARE_READ)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)?;
        let info = information(&security)?;
        plain(&info, false, &security)?;
        ensure!(
            identity(&info) == identity(&information(committed)?),
            "Published file changed before permission repair"
        );
        let mut present = 0;
        let mut defaulted = 0;
        let mut acl = std::ptr::null_mut();
        ensure!(
            unsafe { GetSecurityDescriptorDacl(self.0, &mut present, &mut acl, &mut defaulted) }
                != 0,
            "Read source DACL for repair: {}",
            std::io::Error::last_os_error()
        );
        ensure!(
            present != 0,
            "An absent source DACL cannot be restored safely"
        );
        let protection = if expected.protected {
            PROTECTED_DACL_SECURITY_INFORMATION
        } else {
            UNPROTECTED_DACL_SECURITY_INFORMATION
        };
        let status = unsafe {
            SetSecurityInfo(
                security.as_raw_handle(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | protection,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                acl,
                std::ptr::null_mut(),
            )
        };
        ensure!(
            status == 0,
            "Restore source DACL: {}",
            std::io::Error::from_raw_os_error(status as i32)
        );
        ensure!(
            Descriptor::from_file(committed)?.dacl()? == expected,
            "Published DACL differs from the source; recovery is protected"
        );
        Ok(())
    }
    /// The ACL points into self's allocation, which must outlive CreateFileW.
    fn dacl_only(&self) -> Result<SECURITY_DESCRIPTOR> {
        let mut descriptor = SECURITY_DESCRIPTOR::default();
        let target = (&mut descriptor as *mut SECURITY_DESCRIPTOR).cast();
        let mut present = 0;
        let mut defaulted = 0;
        let mut dacl = std::ptr::null_mut();
        let mut control = 0;
        let mut revision = 0;
        // A fresh absolute descriptor has no owner or group. Copying a
        // self-relative descriptor and clearing pointers would be invalid.
        let inheritance = SE_DACL_PROTECTED | SE_DACL_AUTO_INHERITED | SE_DACL_AUTO_INHERIT_REQ;
        let ok = unsafe {
            InitializeSecurityDescriptor(target, 1) != 0
                && GetSecurityDescriptorDacl(self.0, &mut present, &mut dacl, &mut defaulted) != 0
                && GetSecurityDescriptorControl(self.0, &mut control, &mut revision) != 0
                && SetSecurityDescriptorDacl(target, present, dacl, defaulted) != 0
                && SetSecurityDescriptorControl(target, inheritance, control & inheritance) != 0
        };
        ensure!(ok, "Copy source DACL: {}", std::io::Error::last_os_error());
        Ok(descriptor)
    }
    fn from_file(file: &File) -> Result<Self> {
        let mut raw = std::ptr::null_mut();
        let status = unsafe {
            GetSecurityInfo(
                file.as_raw_handle(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | OWNER_SECURITY_INFORMATION | GROUP_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut raw,
            )
        };
        ensure!(
            status == 0,
            "Read source permissions: {}",
            std::io::Error::from_raw_os_error(status as i32)
        );
        ensure!(!raw.is_null(), "Missing source security descriptor");
        Ok(Self(raw))
    }
}
impl Drop for Descriptor {
    fn drop(&mut self) {
        unsafe {
            LocalFree(self.0);
        }
    }
}
pub(crate) fn identity(info: &BY_HANDLE_FILE_INFORMATION) -> (u32, u32, u32) {
    (
        info.dwVolumeSerialNumber,
        info.nFileIndexHigh,
        info.nFileIndexLow,
    )
}
pub(crate) fn move_no_replace(source: &Path, destination: &Path) -> Result<()> {
    let source = wide(source)?;
    let destination = wide(destination)?;
    retry(|| {
        // No COPY_ALLOWED or REPLACE_EXISTING: same-volume and create-only.
        if unsafe {
            MoveFileExW(
                source.as_ptr(),
                destination.as_ptr(),
                MOVEFILE_WRITE_THROUGH,
            )
        } != 0
        {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    })?;
    Ok(())
}
fn replace(note: &Path, proposed: &Path, backup: &Path) -> Result<()> {
    let note = wide(note)?;
    let proposed = wide(proposed)?;
    let backup = wide(backup)?;
    retry(|| {
        if unsafe {
            ReplaceFileW(
                note.as_ptr(),
                proposed.as_ptr(),
                backup.as_ptr(),
                0,
                std::ptr::null(),
                std::ptr::null(),
            )
        } != 0
        {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    })?;
    Ok(())
}
#[derive(Debug)]
pub enum Replacement {
    Saved { preimage: PathBuf },
    Conflict,
}

#[cfg(test)]
#[path = "windows_files/tests.rs"]
mod tests;

/// Native identity for a checked creation Undo target. Redirecting names fail closed.
pub fn checked_identity(path: &Path) -> Result<(u64, u64)> {
    let parent = Directory::open(path.parent().context("Missing parent")?)?;
    let name = path.file_name().context("Missing filename")?;
    let child = parent.child(name)?;
    let info = if std::fs::symlink_metadata(&child)?.is_dir() {
        Directory::open(&child)?.information()?
    } else {
        parent.read(name)?.2
    };
    let (volume, high, low) = identity(&info);
    Ok((u64::from(volume), u64::from(high) << 32 | u64::from(low)))
}

/// Undo only an untouched creation, through the checked DELETE handle itself.
/// No path-based unlink can delete a racing replacement. A nonempty folder is refused.
pub fn undo_created(path: &Path, expected: (u64, u64), source: Option<&[u8]>) -> Result<()> {
    let parent = Directory::open(path.parent().context("Missing parent")?)?;
    let child = parent.child(path.file_name().context("Missing filename")?)?;
    let directory = source.is_none();
    let mut guard = retry(|| {
        OpenOptions::new()
            .read(true)
            .access_mode(if directory {
                FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES | DELETE
            } else {
                GENERIC_READ | DELETE
            })
            .share_mode(FILE_SHARE_READ | FILE_SHARE_DELETE)
            .custom_flags(
                FILE_FLAG_OPEN_REPARSE_POINT
                    | if directory {
                        FILE_FLAG_BACKUP_SEMANTICS
                    } else {
                        0
                    },
            )
            .open(&child)
    })?;
    let info = information(&guard)?;
    plain(&info, directory, &guard)?;
    let (volume, high, low) = identity(&info);
    ensure!(
        (u64::from(volume), u64::from(high) << 32 | u64::from(low)) == expected,
        "This item was replaced; it was kept"
    );
    if let Some(source) = source {
        let mut bytes = Vec::new();
        guard.read_to_end(&mut bytes)?;
        ensure!(
            bytes == source,
            "This note changed after creation; it was kept"
        );
    } else {
        ensure!(
            std::fs::read_dir(&child)?.next().is_none(),
            "This folder is no longer empty; it was kept"
        );
    }
    let disposition = FILE_DISPOSITION_INFO { DeleteFile: true };
    if unsafe {
        SetFileInformationByHandle(
            guard.as_raw_handle(),
            FileDispositionInfo,
            (&disposition as *const FILE_DISPOSITION_INFO).cast(),
            std::mem::size_of::<FILE_DISPOSITION_INFO>() as u32,
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

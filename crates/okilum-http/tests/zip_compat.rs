#![cfg(feature = "updates")]

use std::{
    fs,
    io::{Cursor, Write},
};
use velopack::bundle::load_bundle_from_memory;
use zip::{write::SimpleFileOptions, CompressionMethod, ZipWriter};

fn package(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    for (name, bytes) in entries {
        zip.start_file(
            *name,
            SimpleFileOptions::default().compression_method(CompressionMethod::Stored),
        )
        .unwrap();
        zip.write_all(bytes).unwrap();
    }
    zip.finish().unwrap().into_inner()
}

fn central(bytes: &[u8]) -> usize {
    u32::from_le_bytes(bytes[bytes.len() - 6..bytes.len() - 2].try_into().unwrap()) as usize
}

fn assert_preserved_on_failure(bytes: &[u8]) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Update.exe");
    let original = b"MZ original installed helper\0\xff";
    fs::write(&path, original).unwrap();
    let bundle = load_bundle_from_memory(bytes).unwrap();
    assert!(bundle.extract_zip_idx_to_path(0, &path).is_err());
    assert!(
        fs::read(&path).unwrap() == original,
        "installed helper changed by a failed extraction"
    );
    assert_eq!(
        fs::read_dir(dir.path()).unwrap().count(),
        1,
        "failed stage must be removed"
    );
}

#[test]
fn failed_crc_after_partial_copy_keeps_original_helper_identical() {
    let mut bytes = package(&[("lib/net45/Squirrel.exe", &vec![b'x'; 150_000])]);
    // Preserve both header CRCs but corrupt data after the first 64 KiB write.
    let start = 30 + "lib/net45/Squirrel.exe".len();
    bytes[start + 90_000] ^= 1;
    assert_preserved_on_failure(&bytes);
}

#[test]
fn failed_size_check_keeps_original_helper_identical() {
    let mut bytes = package(&[("lib/net45/Squirrel.exe", b"short")]);
    let central = central(&bytes);
    bytes[22..26].copy_from_slice(&6u32.to_le_bytes());
    bytes[central + 24..central + 28].copy_from_slice(&6u32.to_le_bytes());
    assert_preserved_on_failure(&bytes);
}

#[test]
fn unsupported_first_helper_is_skipped_and_supported_helper_replaces_original() {
    let mut bytes = package(&[
        ("tools/Squirrel.exe", b"unsupported"),
        ("lib/net45/Squirrel.exe", b"MZ supported replacement"),
    ]);
    let central = central(&bytes);
    bytes[8..10].copy_from_slice(&12u16.to_le_bytes()); // Bzip2, disabled in this SDK.
    bytes[central + 10..central + 12].copy_from_slice(&12u16.to_le_bytes());
    assert_preserved_on_failure(&bytes);
    let bundle = load_bundle_from_memory(&bytes).unwrap();
    assert_eq!(
        bundle.find_zip_file(|name| name.ends_with("Squirrel.exe")),
        Some(1)
    );
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Update.exe");
    fs::write(&path, b"original").unwrap();
    assert_eq!(
        bundle
            .extract_zip_predicate_to_path(|name| name.ends_with("Squirrel.exe"), &path)
            .unwrap(),
        1
    );
    assert_eq!(fs::read(&path).unwrap(), b"MZ supported replacement");
    assert_eq!(
        fs::read_dir(dir.path())
            .unwrap()
            .filter(|e| !e
                .as_ref()
                .unwrap()
                .path()
                .to_string_lossy()
                .ends_with(".vpk-lock"))
            .count(),
        1
    );
}

#[cfg(windows)]
#[test]
fn download_propagates_invalid_bundle_and_failed_helper_extraction() {
    use sha2::{Digest, Sha256};
    use std::path::Path;
    use velopack::{
        bundle::Manifest, locator::VelopackLocatorConfig, sources::UpdateSource, UpdateInfo,
        UpdateManager, VelopackAsset, VelopackAssetFeed,
    };
    struct Source(Vec<u8>);
    impl UpdateSource for Source {
        fn get_release_feed(
            &self,
            _: &str,
            _: &Manifest,
            _: &str,
        ) -> Result<VelopackAssetFeed, velopack::Error> {
            unreachable!()
        }
        fn download_release_entry(
            &self,
            _: &VelopackAsset,
            path: &Path,
            _: Option<std::sync::mpsc::Sender<i16>>,
        ) -> Result<(), velopack::Error> {
            fs::write(path, &self.0)?;
            Ok(())
        }
    }
    let mut broken = package(&[("lib/net45/Squirrel.exe", &vec![b'x'; 150_000])]);
    broken[30 + "lib/net45/Squirrel.exe".len() + 90_000] ^= 1;
    for bytes in [b"not a ZIP".to_vec(), broken] {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let current = root.join("current");
        let packages = root.join("packages");
        fs::create_dir_all(&current).unwrap();
        fs::create_dir_all(&packages).unwrap();
        let manifest = current.join("sq.version");
        fs::write(&manifest, "<package><metadata><id>ZipCompat</id><version>1.0.0</version><title>Fixture</title><authors>QA</authors><description>Test</description><mainExe>okilum.exe</mainExe></metadata></package>").unwrap();
        let helper = root.join("Update.exe");
        fs::write(&helper, b"original helper").unwrap();
        let asset = VelopackAsset {
            PackageId: "ZipCompat".into(),
            Version: "2.0.0".into(),
            Type: "Full".into(),
            FileName: "ZipCompat-2.0.0-full.nupkg".into(),
            Size: bytes.len() as u64,
            SHA256: format!("{:X}", Sha256::digest(&bytes)),
            ..Default::default()
        };
        let manager = UpdateManager::new(
            Source(bytes),
            None,
            Some(VelopackLocatorConfig {
                RootAppDir: root.to_owned(),
                UpdateExePath: helper.clone(),
                PackagesDir: packages,
                ManifestPath: manifest,
                CurrentBinaryDir: current,
                IsPortable: true,
            }),
        )
        .unwrap();
        assert!(manager
            .download_updates(
                &UpdateInfo {
                    TargetFullRelease: asset,
                    BaseRelease: None,
                    DeltasToTarget: vec![],
                    IsDowngrade: false
                },
                None
            )
            .is_err());
        assert_eq!(fs::read(helper).unwrap(), b"original helper");
    }
}

#[test]
fn failed_stub_extraction_returns_error_and_keeps_original() {
    let name = "lib/app/Okilum_ExecutionStub.exe";
    let mut bytes = package(&[(name, &vec![b'x'; 150_000])]);
    bytes[30 + name.len() + 90_000] ^= 1;
    let bundle = load_bundle_from_memory(&bytes).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Okilum.exe");
    fs::write(&path, b"original stub").unwrap();
    assert!(bundle.extract_stubs_to_dir(dir.path()).is_err());
    assert_eq!(fs::read(&path).unwrap(), b"original stub");
}

#[cfg(windows)]
mod windows {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::{
        os::windows::{
            ffi::OsStrExt,
            fs::{MetadataExt, OpenOptionsExt},
        },
        path::{Path, PathBuf},
        sync::mpsc::Sender,
    };
    use velopack::{
        bundle::Manifest, locator::VelopackLocatorConfig, sources::UpdateSource, UpdateInfo,
        UpdateManager, VelopackAsset, VelopackAssetFeed,
    };
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::{
            Authorization::{
                ConvertSecurityDescriptorToStringSecurityDescriptorW,
                ConvertStringSecurityDescriptorToSecurityDescriptorW,
            },
            GetFileSecurityW, SetFileSecurityW, DACL_SECURITY_INFORMATION,
            GROUP_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION,
            PROTECTED_DACL_SECURITY_INFORMATION,
        },
        Storage::FileSystem::{
            SetFileAttributesW, FILE_ATTRIBUTE_ARCHIVE, FILE_ATTRIBUTE_HIDDEN,
            FILE_ATTRIBUTE_NOT_CONTENT_INDEXED, FILE_SHARE_READ,
        },
    };

    fn wide(path: &Path) -> Vec<u16> {
        path.as_os_str().encode_wide().chain(Some(0)).collect()
    }
    fn descriptor(path: &Path) -> String {
        let requested =
            DACL_SECURITY_INFORMATION | OWNER_SECURITY_INFORMATION | GROUP_SECURITY_INFORMATION;
        let mut length = 0;
        unsafe {
            GetFileSecurityW(
                wide(path).as_ptr(),
                requested,
                std::ptr::null_mut(),
                0,
                &mut length,
            );
        }
        assert!(length > 0);
        let mut data = vec![0; length as usize];
        assert_ne!(
            unsafe {
                GetFileSecurityW(
                    wide(path).as_ptr(),
                    requested,
                    data.as_mut_ptr().cast(),
                    length,
                    &mut length,
                )
            },
            0
        );
        let mut text = std::ptr::null_mut();
        let mut size = 0;
        assert_ne!(
            unsafe {
                ConvertSecurityDescriptorToStringSecurityDescriptorW(
                    data.as_mut_ptr().cast(),
                    1,
                    requested,
                    &mut text,
                    &mut size,
                )
            },
            0
        );
        let result = unsafe {
            String::from_utf16(std::slice::from_raw_parts(text, size as usize - 1)).unwrap()
        };
        unsafe {
            LocalFree(text.cast());
        }
        // Windows records DACL inheritance processing during ReplaceFileW.
        // AUTO_INHERITED changes no grant or protected/unprotected policy;
        // compare owner, group, every ACE and protection, as the core save tests do.
        let result = result.trim_end_matches('\0');
        let flags_start = result.find("D:").unwrap() + 2;
        let flags_end = flags_start + result[flags_start..].find('(').unwrap();
        format!(
            "{}{}{}",
            &result[..flags_start],
            result[flags_start..flags_end].replace("AI", ""),
            &result[flags_end..]
        )
    }

    #[test]
    fn locked_target_returns_error_preserves_original_and_retries_after_unlock() {
        let bytes = package(&[("lib/app/Squirrel.exe", b"new helper")]);
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("Update.exe");
        fs::write(&target, b"original helper").unwrap();
        let lock = fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ)
            .open(&target)
            .unwrap();
        let bundle = load_bundle_from_memory(&bytes).unwrap();
        for _ in 0..2 {
            assert!(bundle.extract_zip_idx_to_path(0, &target).is_err());
            assert_eq!(fs::read(&target).unwrap(), b"original helper");
            assert_eq!(
                fs::read_dir(dir.path())
                    .unwrap()
                    .filter(|e| !e
                        .as_ref()
                        .unwrap()
                        .path()
                        .to_string_lossy()
                        .ends_with(".vpk-lock"))
                    .count(),
                1
            );
        }
        drop(lock);
        bundle.extract_zip_idx_to_path(0, &target).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"new helper");
    }

    #[test]
    fn replacement_preserves_explicit_execute_ace_and_nondefault_attributes() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("Update.exe");
        fs::write(&target, b"original helper").unwrap();
        let sddl: Vec<u16> = "D:P(A;;FA;;;OW)(A;;GRGX;;;WD)"
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let mut security = std::ptr::null_mut();
        assert_ne!(
            unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    sddl.as_ptr(),
                    1,
                    &mut security,
                    std::ptr::null_mut(),
                )
            },
            0
        );
        let result = unsafe {
            SetFileSecurityW(
                wide(&target).as_ptr(),
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                security,
            )
        };
        unsafe {
            LocalFree(security);
        }
        assert_ne!(result, 0);
        let before = descriptor(&target);
        assert!(
            before.contains(";;;WD)"),
            "file-specific execute ACE positive control: {before}"
        );
        assert!(
            before.contains("D:P("),
            "protected DACL positive control: {before}"
        );
        let attributes =
            FILE_ATTRIBUTE_HIDDEN | FILE_ATTRIBUTE_NOT_CONTENT_INDEXED | FILE_ATTRIBUTE_ARCHIVE;
        assert_ne!(
            unsafe { SetFileAttributesW(wide(&target).as_ptr(), attributes) },
            0
        );
        let before_attributes = fs::metadata(&target).unwrap().file_attributes();
        let bytes = package(&[("lib/app/Squirrel.exe", b"replacement helper")]);
        load_bundle_from_memory(&bytes)
            .unwrap()
            .extract_zip_idx_to_path(0, &target)
            .unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"replacement helper");
        assert_eq!(
            descriptor(&target),
            before,
            "destination security descriptor must survive replacement"
        );
        assert_eq!(
            fs::metadata(&target).unwrap().file_attributes(),
            before_attributes,
            "destination attributes must survive replacement"
        );
    }

    struct Source {
        bytes: Vec<u8>,
        asset: VelopackAsset,
    }
    impl UpdateSource for Source {
        fn get_release_feed(
            &self,
            _: &str,
            _: &Manifest,
            _: &str,
        ) -> Result<VelopackAssetFeed, velopack::Error> {
            Ok(VelopackAssetFeed {
                Assets: vec![self.asset.clone()],
            })
        }
        fn download_release_entry(
            &self,
            _: &VelopackAsset,
            destination: &Path,
            _: Option<Sender<i16>>,
        ) -> Result<(), velopack::Error> {
            fs::write(destination, &self.bytes)?;
            Ok(())
        }
    }
    struct Fixture {
        _root: tempfile::TempDir,
        helper: PathBuf,
        package: PathBuf,
        bytes: Vec<u8>,
        manager: UpdateManager,
        update: UpdateInfo,
    }
    fn fixture() -> Fixture {
        let root = tempfile::tempdir().unwrap();
        let current = root.path().join("current");
        let packages = root.path().join("packages");
        fs::create_dir(&current).unwrap();
        fs::create_dir(&packages).unwrap();
        let manifest = "<package><metadata><id>ZipCompat</id><version>2.0.0</version><title>Fixture</title><authors>QA</authors><description>Test</description><mainExe>okilum.exe</mainExe></metadata></package>";
        let bytes = package(&[
            ("ZipCompat.nuspec", manifest.as_bytes()),
            ("lib/app/Squirrel.exe", b"prepared helper"),
        ]);
        let manifest_path = current.join("sq.version");
        fs::write(&manifest_path, manifest.replace("2.0.0", "1.0.0")).unwrap();
        let helper = root.path().join("Update.exe");
        fs::write(&helper, b"stale helper").unwrap();
        let asset = VelopackAsset {
            PackageId: "ZipCompat".into(),
            Version: "2.0.0".into(),
            Type: "Full".into(),
            FileName: "ZipCompat-2.0.0-full.nupkg".into(),
            Size: bytes.len() as u64,
            SHA256: format!("{:X}", Sha256::digest(&bytes)),
            ..Default::default()
        };
        let final_package = packages.join(&asset.FileName);
        let manager = UpdateManager::new(
            Source {
                bytes: bytes.clone(),
                asset: asset.clone(),
            },
            None,
            Some(VelopackLocatorConfig {
                RootAppDir: root.path().to_owned(),
                UpdateExePath: helper.clone(),
                PackagesDir: packages,
                ManifestPath: manifest_path,
                CurrentBinaryDir: current,
                IsPortable: true,
            }),
        )
        .unwrap();
        Fixture {
            _root: root,
            helper,
            package: final_package,
            bytes,
            manager,
            update: UpdateInfo {
                TargetFullRelease: asset,
                BaseRelease: None,
                DeltasToTarget: vec![],
                IsDowngrade: false,
            },
        }
    }

    #[test]
    fn helper_failure_never_publishes_pending_package_and_retry_prepares_helper() {
        let f = fixture();
        let lock = fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ)
            .open(&f.helper)
            .unwrap();
        for _ in 0..2 {
            assert!(f.manager.download_updates(&f.update, None).is_err());
            assert!(
                !f.package.exists(),
                "failed helper preparation must not publish a final package"
            );
            assert!(f.manager.get_update_pending_restart().is_none());
            assert_eq!(fs::read(&f.helper).unwrap(), b"stale helper");
        }
        drop(lock);
        f.manager.download_updates(&f.update, None).unwrap();
        assert!(f
            .manager
            .is_update_helper_prepared(&f.update.TargetFullRelease)
            .unwrap());
        assert!(f.manager.get_update_pending_restart().is_some());
        assert_eq!(fs::read(&f.helper).unwrap(), b"prepared helper");
    }

    #[test]
    fn cached_package_with_stale_or_corrupted_helper_is_not_pending_and_retry_repairs_it() {
        let f = fixture();
        // Simulate a final package left behind by the pre-fix SDK.
        fs::write(&f.package, &f.bytes).unwrap();
        assert!(!f
            .manager
            .is_update_helper_prepared(&f.update.TargetFullRelease)
            .unwrap());
        assert!(f.manager.get_update_pending_restart().is_none());
        let lock = fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ)
            .open(&f.helper)
            .unwrap();
        for _ in 0..2 {
            assert!(f.manager.download_updates(&f.update, None).is_err());
        }
        assert!(f.manager.get_update_pending_restart().is_none());
        assert_eq!(fs::read(&f.helper).unwrap(), b"stale helper");
        drop(lock);
        f.manager.download_updates(&f.update, None).unwrap();
        assert!(f.manager.get_update_pending_restart().is_some());
        fs::write(&f.helper, b"prepared helpeX").unwrap(); // same length, wrong bytes
        assert!(!f
            .manager
            .is_update_helper_prepared(&f.update.TargetFullRelease)
            .unwrap());
        assert!(f.manager.get_update_pending_restart().is_none());
    }
}

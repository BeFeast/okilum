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
    assert_eq!(fs::read(&path).unwrap(), original);
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
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
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

#![cfg(all(windows, feature = "updates"))]

use sha2::{Digest, Sha256};
use std::{fs, path::Path, sync::mpsc::Sender};
use velopack::{
    bundle::Manifest, locator::VelopackLocatorConfig, sources::UpdateSource, UpdateInfo,
    UpdateManager, VelopackAsset, VelopackAssetFeed,
};

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetConsoleWindow() -> *mut std::ffi::c_void;
    fn AllocConsole() -> i32;
    fn FreeConsole() -> i32;
}

struct Console(bool);
impl Drop for Console {
    fn drop(&mut self) {
        if self.0 {
            unsafe { FreeConsole() };
        }
    }
}

struct DeltaSource;
const PROPOSED: &[u8] = b"fixture reconstructed full package";
impl UpdateSource for DeltaSource {
    fn get_release_feed(
        &self,
        _: &str,
        _: &Manifest,
        _: &str,
    ) -> Result<VelopackAssetFeed, velopack::Error> {
        unreachable!("this control starts with an explicit delta plan")
    }

    fn download_release_entry(
        &self,
        asset: &VelopackAsset,
        destination: &Path,
        _: Option<Sender<i16>>,
    ) -> Result<(), velopack::Error> {
        assert_eq!(
            asset.Type, "Delta",
            "full fallback must not mask a broken patcher"
        );
        fs::write(destination, PROPOSED)?;
        Ok(())
    }
}

fn asset(kind: &str, bytes: &[u8]) -> VelopackAsset {
    VelopackAsset {
        PackageId: "Okilum.DeltaConsoleFixture".into(),
        Version: "2.0.0".into(),
        Type: kind.into(),
        FileName: format!("Okilum.DeltaConsoleFixture-2.0.0-{kind}.nupkg"),
        Size: bytes.len() as u64,
        SHA256: format!("{:X}", Sha256::digest(bytes)),
        ..Default::default()
    }
}

#[test]
fn windows_delta_preparation_has_no_console_and_keeps_the_delta_path() {
    // A service-hosted runner may have no console. Establish the inherited-console
    // positive control explicitly so an unflagged child cannot pass accidentally.
    let allocate = unsafe { GetConsoleWindow() }.is_null();
    if allocate {
        assert_ne!(
            unsafe { AllocConsole() },
            0,
            "console control must be available"
        );
    }
    let _console = Console(allocate);
    assert!(!unsafe { GetConsoleWindow() }.is_null());

    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().join("update with spaces שלום");
    let packages = root.join("packages");
    let current = root.join("current");
    fs::create_dir_all(&packages).unwrap();
    fs::create_dir_all(&current).unwrap();
    let manifest = current.join("sq.version");
    fs::write(&manifest, "<package><metadata><id>Okilum.DeltaConsoleFixture</id><version>1.0.0</version><title>Fixture</title><authors>QA</authors><description>Test</description><mainExe>okilum.exe</mainExe></metadata></package>").unwrap();
    let helper = std::env::var_os("OKILUM_DELTA_CONSOLE_HELPER")
        .expect("compile scripts/ci/windows-delta-console-helper.rs first");
    let update = root.join("Update.exe");
    fs::copy(helper, &update).unwrap();
    let base = asset("Full", b"fixture base");
    fs::write(packages.join(&base.FileName), b"fixture base").unwrap();
    let full = asset("Full", PROPOSED);
    // Use a distinct version/name for the existing base package.
    let old = "Okilum.DeltaConsoleFixture-1.0.0-Full.nupkg";
    fs::rename(packages.join(&base.FileName), packages.join(old)).unwrap();
    let base = VelopackAsset {
        FileName: old.into(),
        Version: "1.0.0".into(),
        ..base
    };
    let manager = UpdateManager::new(
        DeltaSource,
        None,
        Some(VelopackLocatorConfig {
            RootAppDir: root,
            UpdateExePath: update,
            PackagesDir: packages.clone(),
            ManifestPath: manifest,
            CurrentBinaryDir: current,
            IsPortable: true,
        }),
    )
    .unwrap();
    manager
        .download_updates(
            &UpdateInfo {
                TargetFullRelease: full.clone(),
                BaseRelease: Some(base),
                DeltasToTarget: vec![asset("Delta", PROPOSED)],
                IsDowngrade: false,
            },
            None,
        )
        .unwrap();
    assert_eq!(fs::read(packages.join(&full.FileName)).unwrap(), PROPOSED);
    let console =
        fs::read_to_string(packages.join(&full.FileName).with_extension("console")).unwrap();
    assert_eq!(console, "0", "delta patcher must not have a console");
}

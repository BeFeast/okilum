#![cfg(all(windows, feature = "updates"))]

use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::{mpsc::Sender, Arc, Mutex},
};
use velopack::{
    bundle::{load_bundle_from_file, Manifest},
    locator::VelopackLocatorConfig,
    sources::UpdateSource,
    UpdateInfo, UpdateManager, VelopackAsset, VelopackAssetFeed,
};

#[derive(Clone)]
struct HistoricalSource {
    directory: PathBuf,
    delta_only: bool,
    downloads: Arc<Mutex<Vec<String>>>,
}
impl UpdateSource for HistoricalSource {
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
        asset: &VelopackAsset,
        destination: &Path,
        _: Option<Sender<i16>>,
    ) -> Result<(), velopack::Error> {
        assert!(
            !self.delta_only || asset.Type == "Delta",
            "Full fallback must not conceal failed real delta application"
        );
        self.downloads.lock().unwrap().push(asset.Type.clone());
        fs::copy(self.directory.join(&asset.FileName), destination)?;
        Ok(())
    }
}

fn hash(path: &Path) -> Vec<u8> {
    let mut file = fs::File::open(path).unwrap();
    let mut sha = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        let n = file.read(&mut buffer).unwrap();
        if n == 0 {
            break;
        }
        sha.update(&buffer[..n]);
    }
    sha.finalize().to_vec()
}

fn compare_tree(expected: &Path, actual: &Path) -> usize {
    let mut files = 0;
    for entry in fs::read_dir(expected).unwrap() {
        let entry = entry.unwrap();
        let target = actual.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            files += compare_tree(&entry.path(), &target);
        } else {
            assert_eq!(
                hash(&entry.path()),
                hash(&target),
                "installed bytes differ: {}",
                target.display()
            );
            files += 1;
        }
    }
    files
}

#[test]
#[ignore = "requires checksum-verified published packages; scripts/ci/windows-historical-update.py"]
fn published_full_and_real_delta_are_applied_on_windows() {
    let directory = PathBuf::from(
        std::env::var_os("OKILUM_HISTORICAL_PACKAGES")
            .expect("run the package verification driver"),
    );
    let feed: VelopackAssetFeed = serde_json::from_slice(include_bytes!(
        "../../../scripts/ci/fixtures/windows-historical-update.json"
    ))
    .unwrap();
    let full = feed
        .Assets
        .iter()
        .find(|a| a.Type == "Full" && a.Version == "0.1.31678")
        .unwrap();
    let base = feed
        .Assets
        .iter()
        .find(|a| a.Type == "Full" && a.Version == "0.1.31653")
        .unwrap();
    let delta = feed.Assets.iter().find(|a| a.Type == "Delta").unwrap();
    let expected = tempfile::tempdir().unwrap();
    load_bundle_from_file(directory.join(&full.FileName))
        .unwrap()
        .extract_lib_contents_to_path(expected.path(), |_| {})
        .unwrap();
    for delta_only in [false, true] {
        let owned = tempfile::Builder::new()
            .prefix("okilum historical שלום ")
            .tempdir()
            .unwrap();
        let root = owned.path();
        let current = root.join("current");
        let packages = root.join("packages");
        fs::create_dir(&current).unwrap();
        fs::create_dir(&packages).unwrap();
        fs::write(root.join(".portable"), b"").unwrap();
        let old_bundle = load_bundle_from_file(directory.join(&base.FileName)).unwrap();
        old_bundle
            .extract_lib_contents_to_path(&current, |_| {})
            .unwrap();
        let helper = root.join("Update.exe");
        old_bundle
            .extract_zip_predicate_to_path(|name| name.ends_with("Squirrel.exe"), &helper)
            .unwrap();
        fs::copy(
            directory.join(&base.FileName),
            packages.join(&base.FileName),
        )
        .unwrap();
        let downloads = Arc::new(Mutex::new(Vec::new()));
        let source = HistoricalSource {
            directory: directory.clone(),
            delta_only,
            downloads: downloads.clone(),
        };
        let manager = UpdateManager::new(
            source,
            None,
            Some(VelopackLocatorConfig {
                RootAppDir: root.to_owned(),
                UpdateExePath: helper.clone(),
                PackagesDir: packages.clone(),
                ManifestPath: current.join("sq.version"),
                CurrentBinaryDir: current.clone(),
                IsPortable: true,
            }),
        )
        .unwrap();
        manager
            .download_updates(
                &UpdateInfo {
                    TargetFullRelease: full.clone(),
                    BaseRelease: delta_only.then(|| base.clone()),
                    DeltasToTarget: if delta_only {
                        vec![delta.clone()]
                    } else {
                        vec![]
                    },
                    IsDowngrade: false,
                },
                None,
            )
            .unwrap();
        assert_eq!(
            *downloads.lock().unwrap(),
            [if delta_only { "Delta" } else { "Full" }]
        );
        // Compare the reconstructed package by extracted payload, since recompressing
        // a delta can legitimately change the ZIP container bytes.
        let proposed = packages.join(&full.FileName);
        let reconstructed = tempfile::tempdir().unwrap();
        load_bundle_from_file(&proposed)
            .unwrap()
            .extract_lib_contents_to_path(reconstructed.path(), |_| {})
            .unwrap();
        let count = compare_tree(expected.path(), reconstructed.path());
        assert!(count >= 15, "real package positive control");
        // Run the real published Windows updater against an isolated portable install.
        // --norestart leaves no interactive app running; no system install is touched.
        let log = root.join("apply.log");
        let output = std::process::Command::new(&helper)
            .args(["apply", "--silent", "--norestart", "--package"])
            .arg(&proposed)
            .arg("--rootDir")
            .arg(root)
            .arg("--packageDir")
            .arg(&packages)
            .arg("--log")
            .arg(&log)
            .output()
            .unwrap();
        let log_text = fs::read_to_string(&log).unwrap_or_default();
        println!(
            "historical delta={delta_only}, status={}, stdout={}, stderr={}, log={log_text}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            output.status.success(),
            "real published Update.exe apply failed"
        );
        assert_eq!(compare_tree(expected.path(), &current), count);
        let mut installed = load_bundle_from_file(&proposed).unwrap();
        assert_eq!(
            installed.read_manifest().unwrap().version.to_string(),
            full.Version
        );
        println!("PASS real historical {} apply: {} -> {}, {count} payload files byte-identical, including 190 MB executable", if delta_only { "delta" } else { "full" }, base.Version, full.Version);
    }
}

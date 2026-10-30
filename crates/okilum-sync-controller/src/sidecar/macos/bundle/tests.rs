use super::*;
use std::{
    cell::RefCell,
    os::unix::fs::{symlink, PermissionsExt},
    rc::Rc,
};
use uuid::Uuid;

#[derive(Clone, Default)]
struct Signatures {
    calls: Rc<RefCell<Vec<(PathBuf, CodeRequirement)>>>,
    refusal: Rc<RefCell<Option<PathBuf>>>,
}
impl SignatureVerifier for Signatures {
    fn verify(&self, code: &Path, requirement: &CodeRequirement) -> Result<()> {
        self.calls
            .borrow_mut()
            .push((code.to_owned(), requirement.clone()));
        ensure!(
            self.refusal.borrow().as_deref() != Some(code),
            "injected signature refusal"
        );
        Ok(())
    }
}
struct Fixture {
    _root: tempfile::TempDir,
    app: PathBuf,
    binding: Binding,
    signatures: Signatures,
}
impl Fixture {
    fn new() -> Result<Self> {
        let root = tempfile::tempdir()?;
        let base = root.path().canonicalize()?;
        let app = base.join("Fixture.app");
        for directory in [
            app.join("Contents/MacOS"),
            app.join("Contents/Library/LaunchAgents"),
            base.join("state"),
        ] {
            fs::create_dir_all(directory)?;
        }
        for relative in ["Contents/MacOS/reader", HELPER] {
            let path = app.join(relative);
            fs::write(&path, b"synthetic code, never executed")?;
            fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
        }
        fs::write(
            app.join("Contents/Library/LaunchAgents").join(PLIST),
            bundled_plist(HELPER)?,
        )?;
        let binding = Binding {
            instance: Uuid::new_v4(),
            installation: Uuid::new_v4(),
            owner: rustix::process::geteuid().as_raw().to_string(),
            supervisor: app.join(HELPER).to_str().unwrap().to_owned(),
            state_directory: base.join("state").to_str().unwrap().to_owned(),
            device_identity: "synthetic-existing-device".to_owned(),
        };
        Ok(Self {
            _root: root,
            app,
            binding,
            signatures: Signatures::default(),
        })
    }
    fn guard(&self) -> Result<BundleGuard<Signatures>> {
        BundleGuard::new(
            &self.app,
            "reader",
            self.binding.clone(),
            app_policy(),
            helper_policy(),
            self.signatures.clone(),
        )
    }
}
fn app_policy() -> CodeRequirement {
    CodeRequirement::team_and_identifier("ABCDE12345", "org.example.fixture").unwrap()
}
fn helper_policy() -> CodeRequirement {
    CodeRequirement::team_and_identifier("ABCDE12345", "org.example.fixture.sync").unwrap()
}

#[test]
fn bundled_runtime_requires_fixed_path_outer_seal_nested_signature_and_exact_digest() -> Result<()>
{
    use sha2::{Digest, Sha256};
    let fixture = Fixture::new()?;
    let guard = fixture.guard()?;
    let policy =
        CodeRequirement::team_and_identifier("ABCDE12345", "org.example.fixture.syncthing")?;
    assert!(guard.bundled_runtime(&fixture.binding, &policy).is_err());
    let executable = fixture.app.join(SYNCTHING);
    fs::write(&executable, b"synthetic runtime, never executed")?;
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o755))?;
    let digest = format!("{:x}", Sha256::digest(fs::read(&executable)?));
    let provenance = fixture.app.join(SYNCTHING_PROVENANCE);
    fs::create_dir_all(provenance.parent().unwrap())?;
    fs::write(
        &provenance,
        serde_json::to_vec(&serde_json::json!({
            "version": "2.1.6", "signed": true, "signed_binary_sha256": digest
        }))?,
    )?;
    let (path, hash, receipt) = guard.bundled_runtime(&fixture.binding, &policy)?;
    assert_eq!(path, executable);
    assert_eq!(hash, digest);
    assert_eq!(receipt, fs::read(&provenance)?);
    assert!(fixture
        .signatures
        .calls
        .borrow()
        .contains(&(executable.clone(), policy.clone())));
    *fixture.signatures.refusal.borrow_mut() = Some(executable.clone());
    assert!(guard.bundled_runtime(&fixture.binding, &policy).is_err());
    *fixture.signatures.refusal.borrow_mut() = None;
    fs::write(&executable, b"tampered")?;
    assert!(guard.bundled_runtime(&fixture.binding, &policy).is_err());
    fs::remove_file(&executable)?;
    symlink("/usr/bin/true", &executable)?;
    assert!(guard.bundled_runtime(&fixture.binding, &policy).is_err());
    Ok(())
}

#[test]
fn valid_bundle_checks_outer_seal_and_separate_helper_policy_without_writes() -> Result<()> {
    let fixture = Fixture::new()?;
    let guard = fixture.guard()?;
    assert!(
        fixture.signatures.calls.borrow().is_empty(),
        "constructor verified or mutated code"
    );
    let plist = fixture
        .app
        .join("Contents/Library/LaunchAgents")
        .join(PLIST);
    let before = fs::read(&plist)?;
    guard.verify(&fixture.binding)?;
    assert_eq!(
        *fixture.signatures.calls.borrow(),
        vec![
            (fixture.app.clone(), app_policy()),
            (fixture.app.join(HELPER), helper_policy())
        ]
    );
    assert_eq!(fs::read(plist)?, before);
    assert!(fs::read_dir(&fixture.binding.state_directory)?
        .next()
        .is_none());
    Ok(())
}

#[test]
fn changed_binding_moved_bundle_and_changed_agent_fail_before_signature_calls() -> Result<()> {
    let fixture = Fixture::new()?;
    let guard = fixture.guard()?;
    for field in 0..6 {
        let mut binding = fixture.binding.clone();
        match field {
            0 => binding.owner.push('0'),
            1 => binding.installation = Uuid::new_v4(),
            2 => binding.instance = Uuid::new_v4(),
            3 => binding.device_identity.push('x'),
            4 => binding.state_directory.push('x'),
            _ => binding.supervisor.push('x'),
        }
        assert!(guard.verify(&binding).is_err());
    }
    let plist = fixture
        .app
        .join("Contents/Library/LaunchAgents")
        .join(PLIST);
    fs::write(&plist, b"<plist>changed agent</plist>")?;
    assert!(guard.verify(&fixture.binding).is_err());
    fs::write(&plist, bundled_plist(HELPER)?)?;
    let moved = fixture.app.with_file_name("Moved.app");
    fs::rename(&fixture.app, &moved)?;
    assert!(guard.verify(&fixture.binding).is_err());
    assert!(fixture.signatures.calls.borrow().is_empty());
    Ok(())
}

#[test]
fn linked_writable_or_nonexecutable_helper_and_in_bundle_state_are_refused() -> Result<()> {
    let fixture = Fixture::new()?;
    let guard = fixture.guard()?;
    let helper = fixture.app.join(HELPER);
    for mode in [0o777, 0o644] {
        fs::set_permissions(&helper, fs::Permissions::from_mode(mode))?;
        assert!(guard.verify(&fixture.binding).is_err());
    }
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o755))?;
    let other = helper.with_file_name("other");
    fs::rename(&helper, &other)?;
    symlink(&other, &helper)?;
    assert!(guard.verify(&fixture.binding).is_err());
    fs::remove_file(&helper)?;
    fs::rename(&other, &helper)?;
    let mut binding = fixture.binding.clone();
    binding.state_directory = fixture.app.to_str().unwrap().to_owned();
    let inside = BundleGuard::new(
        &fixture.app,
        "reader",
        binding.clone(),
        app_policy(),
        helper_policy(),
        fixture.signatures.clone(),
    )?;
    assert!(inside.verify(&binding).is_err());
    assert!(fixture.signatures.calls.borrow().is_empty());
    guard.verify(&fixture.binding)?; // Positive control after restoring the layout.
    Ok(())
}

#[test]
fn signature_refusal_is_not_repaired_or_replaced_by_helper_only_checks() -> Result<()> {
    let fixture = Fixture::new()?;
    let guard = fixture.guard()?;
    *fixture.signatures.refusal.borrow_mut() = Some(fixture.app.clone());
    assert!(guard.verify(&fixture.binding).is_err());
    assert_eq!(fixture.signatures.calls.borrow().len(), 1);
    fixture.signatures.calls.borrow_mut().clear();
    *fixture.signatures.refusal.borrow_mut() = Some(fixture.app.join(HELPER));
    assert!(guard.verify(&fixture.binding).is_err());
    assert_eq!(fixture.signatures.calls.borrow().len(), 2);
    *fixture.signatures.refusal.borrow_mut() = None;
    guard.verify(&fixture.binding)?;
    Ok(())
}

#[test]
fn a_fresh_guard_cannot_adopt_a_foreign_owner_or_different_bundle_path() -> Result<()> {
    let fixture = Fixture::new()?;
    for field in 0..2 {
        let mut binding = fixture.binding.clone();
        if field == 0 {
            binding.owner.push('0');
        } else {
            binding.supervisor = fixture
                .app
                .with_file_name("Copy.app")
                .join(HELPER)
                .to_str()
                .unwrap()
                .to_owned();
        }
        let guard = BundleGuard::new(
            &fixture.app,
            "reader",
            binding.clone(),
            app_policy(),
            helper_policy(),
            fixture.signatures.clone(),
        )?;
        assert!(guard.verify(&binding).is_err());
    }
    assert!(fixture.signatures.calls.borrow().is_empty());
    fixture.guard()?.verify(&fixture.binding)?;
    Ok(())
}

#[test]
fn read_only_foreign_ownership_is_not_a_trusted_installation() {
    // Both supported install modes pass, while another account cannot acquire
    // trust by removing its own write bit. No privileged chown is needed in CI.
    let current = 501;
    assert!(trusted_owner(current, current));
    assert!(trusted_owner(0, current));
    for foreign in [1, 502, u32::MAX] {
        assert!(!trusted_owner(foreign, current));
    }
    assert!(trusted_owner(0, 0));
    assert!(!trusted_owner(501, 0));
}

use super::*;
use crate::sidecar::supervisor::ipc::{
    windows_endpoint::{PrivateClient, PrivatePipe},
    Scope,
};
use uuid::Uuid;
use windows::Win32::System::Threading::GetCurrentProcess;

struct Policy(bool);
impl ImagePolicy for Policy {
    fn verify_image(&self, _: &Path) -> Result<()> {
        ensure!(self.0, "untrusted signer");
        Ok(())
    }
}
struct Case {
    supervisor: String,
    owner: String,
    started: u64,
    policy: bool,
}
fn genuine() -> Case {
    let me = unsafe { GetCurrentProcess() };
    Case {
        supervisor: image_path(me).unwrap(),
        owner: current_sid().unwrap(),
        started: start_time(me).unwrap(),
        policy: true,
    }
}
/// A fresh private pipe served by this very process (so its image, owner and start
/// time are known), then a discovering connect with the case's claims.
fn connect(case: &Case) -> Result<()> {
    let scope = Scope {
        installation: Uuid::from_u128(1),
        instance: Uuid::from_u128(2),
        generation: Uuid::new_v4(),
    };
    let _pipe = PrivatePipe::create(&scope)?;
    let binding = Binding {
        instance: scope.instance,
        installation: scope.installation,
        owner: case.owner.clone(),
        supervisor: case.supervisor.clone(),
        state_directory: r"C:\unused".into(),
        device_identity: "existing-device".into(),
    };
    let hint = Hint::new(scope.generation, case.started)?;
    let client = PrivateClient::connect_discovering(&scope, |pipe| {
        identify_server(pipe, &binding, &hint, &Policy(case.policy))
    })?;
    // Descriptor, direction and the retained peer on the live pipe are rechecked.
    client.verify()?;
    Ok(())
}

#[test]
fn the_genuine_server_is_identified_and_each_wrong_claim_is_refused() {
    connect(&genuine()).expect("positive control: the genuine server is accepted");
    let refused = |name: &str, mutate: &dyn Fn(&mut Case), expect: &str| {
        let mut case = genuine();
        mutate(&mut case);
        let error = connect(&case).expect_err(name);
        assert!(format!("{error:#}").contains(expect), "{name}: {error:#}");
        // The same endpoint construction still works for the genuine claims.
        connect(&genuine()).unwrap();
    };
    refused(
        "different executable",
        &|c| c.supervisor = r"C:\Windows\System32\cmd.exe".into(),
        "different executable",
    );
    refused(
        "stale or reused-PID start time",
        &|c| c.started += 1,
        "start time differs",
    );
    refused(
        "binding owned by another account",
        &|c| c.owner = "S-1-5-18".into(),
        "another user",
    );
    refused("unsigned image", &|c| c.policy = false, "signature policy");
}

#[test]
fn image_paths_compare_case_insensitively_and_nothing_else() {
    assert!(same_path(r"C:\Prog\Sup.exe", r"c:\prog\sup.EXE"));
    assert!(same_path("C:/Prog/Sup.exe", r"C:\Prog\Sup.exe"));
    assert!(!same_path(r"C:\Prog\Sup.exe", r"C:\Prog\Sup2.exe"));
    assert!(!same_path(r"C:\Prog\Sup.exe", r"C:\Other\Sup.exe"));
    assert!(!same_path(r"C:\Prog\Sup.exe", r"C:\Prog\Sup.exe.bak"));
}

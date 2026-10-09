//! Standalone observer: deliberately never calls Runner::open.
use std::path::PathBuf;
fn run() -> anyhow::Result<serde_json::Value> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    anyhow::ensure!(
        args.len() == 4 && args[0] == "--operational" && args[2] == "--candidate",
        "invalid arguments"
    );
    let candidate = serde_json::from_slice(&std::fs::read(PathBuf::from(&args[3]))?)?;
    okilum_brain::t3_recovery::preflight(&PathBuf::from(&args[1]), &candidate)
}
fn main() {
    match run() {
        Ok(report) => {
            println!("{}", serde_json::to_string_pretty(&report).unwrap());
            if report["supported_recovery_candidate"] != true {
                std::process::exit(2);
            }
        }
        Err(_) => {
            // File errors can include private paths and serde errors can include
            // content. The machine-readable refusal must never echo them.
            println!("{{\"schema\":\"tessera-t3-recovery/v1\",\"read_only\":true,\"supported_recovery_candidate\":false,\"blockers\":[\"invalid_or_unreadable_input\"],\"live_apply_authorized\":false}}");
            std::process::exit(2);
        }
    }
}

//! Read-only all-goal admission proof; never opens a live Runner.
fn main() {
    let result = (|| -> anyhow::Result<serde_json::Value> {
        let args = std::env::args_os().skip(1).collect::<Vec<_>>();
        anyhow::ensure!(
            (args.len() == 4 || (args.len() == 6 && args[4] == "--proof-manifest"))
                && args[0] == "--operational"
                && args[2] == "--brain",
            "invalid arguments"
        );
        let manifest = if args.len() == 6 {
            Some(
                serde_json::from_slice::<tessera_brain::t3_compat::Manifest>(&std::fs::read(
                    &args[5],
                )?)?,
            )
        } else {
            None
        };
        tessera_brain::t3_routes::inspect_with_manifest(
            std::path::Path::new(&args[1]),
            std::path::Path::new(&args[3]),
            manifest.as_ref(),
        )
    })();
    match result {
        Ok(report) => {
            println!("{}", serde_json::to_string_pretty(&report).unwrap());
            if report["eligible"] != true {
                std::process::exit(2);
            }
        }
        Err(_) => {
            println!("{{\"schema\":\"tessera-t3-inventory/v1\",\"read_only\":true,\"eligible\":false,\"error\":\"invalid_or_changed_inventory\"}}");
            std::process::exit(2);
        }
    }
}

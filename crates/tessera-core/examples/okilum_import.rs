//! Native check for the Tessera → Okilum state import (#967). Uses the real
//! environment (HOME, XDG_*, LOCALAPPDATA, APPDATA) of the machine it runs on.
//!
//! okilum_import plan              list legacy and new roots, change nothing
//! okilum_import run               import (what an Okilum build does on first launch)
//! okilum_import run --fail-after N  stop after N copied files, as a crash would
use tessera_core::app_migration::{self, Environment, Options};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let env = Environment::current();
    let roots = app_migration::roots(&env);
    let Some(lock) = app_migration::lock_path(&roots) else {
        eprintln!("no app-state roots for this environment");
        std::process::exit(2);
    };
    match args.first().map(String::as_str) {
        Some("plan") => {
            for root in &roots {
                let state = if root.new.join(app_migration::MARKER).is_file() {
                    "imported"
                } else if !root.old.is_dir() {
                    "no legacy data"
                } else if root.new.exists() {
                    "will merge"
                } else {
                    "will copy"
                };
                println!(
                    "{:<7} {} -> {} ({state})",
                    root.kind,
                    root.old.display(),
                    root.new.display()
                );
            }
            println!("lock    {}", lock.display());
        }
        Some("run") => {
            let fail_after = args
                .iter()
                .position(|a| a == "--fail-after")
                .and_then(|i| args.get(i + 1))
                .and_then(|n| n.parse().ok());
            match app_migration::import(&roots, &lock, &Options { fail_after }) {
                Ok(report) => {
                    for (root, outcome) in report.roots {
                        println!("{:<7} {:?}", root.kind, outcome);
                    }
                }
                Err(error) => {
                    eprintln!("import failed: {error}");
                    std::process::exit(1);
                }
            }
        }
        _ => {
            eprintln!("usage: okilum_import plan | run [--fail-after N]");
            std::process::exit(2);
        }
    }
}

//! Writes the static LaunchAgent plist for the bundled sync helper, so the release script
//! and the code that verifies it share one definition:
//! `bundled_plist <Contents/MacOS/helper> <output directory>`. Writing registers nothing.
use okilum_sync_controller::sidecar::macos::{bundled_plist, PLIST};

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let (Some(program), Some(directory), None) = (args.next(), args.next(), args.next()) else {
        anyhow::bail!("usage: bundled_plist <Contents/MacOS/helper> <output directory>");
    };
    std::fs::write(
        std::path::Path::new(&directory).join(PLIST),
        bundled_plist(&program)?,
    )?;
    Ok(())
}

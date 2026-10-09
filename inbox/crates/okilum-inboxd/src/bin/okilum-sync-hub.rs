use clap::Parser;
#[derive(Parser)]
struct Args {
    #[arg(long)]
    config: std::path::PathBuf,
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(unix)]
    {
        let args = Args::parse();
        okilum_inboxd::sync_hub::private_file(&args.config)?;
        let config = serde_json::from_slice(&std::fs::read(&args.config)?)?;
        okilum_inboxd::sync_hub::serve(config)?;
    }
    #[cfg(not(unix))]
    {
        return Err("host adapter requires Unix".into());
    }
    Ok(())
}

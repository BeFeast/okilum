use clap::{Parser, ValueEnum};
use std::{
    fs,
    net::SocketAddr,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};
use tessera_inboxd::{auth::Auth, store::Store};

#[derive(Clone, ValueEnum)]
enum Command {
    Bootstrap,
    Serve,
}
#[derive(Parser)]
#[command(about = "Optional one-owner Inbox backend (LAN QA only)")]
struct Options {
    #[arg(value_enum)]
    command: Command,
    #[arg(long)]
    data_dir: PathBuf,
    #[arg(long)]
    origin: String,
    #[arg(long, default_value = "127.0.0.1:24171")]
    listen: SocketAddr,
    #[arg(long, requires_all = ["ai_model", "ai_credential_file"])]
    ai_endpoint: Option<String>,
    #[arg(long, requires = "ai_endpoint")]
    ai_model: Option<String>,
    #[arg(long, requires = "ai_endpoint")]
    ai_credential_file: Option<PathBuf>,
    #[arg(long, requires = "vault_folder")]
    fixture_vault: Option<PathBuf>,
    /// Private opt-in owner/vault mapping; does not contain the hub REST key.
    #[arg(long)]
    sync_config: Option<PathBuf>,
    /// Private operator-provisioned T3 bridge credential and scope (disabled by default).
    #[arg(long)]
    bridge_credential_file: Option<PathBuf>,
    /// Read-only derived projection; the collector retains the separate source token.
    #[arg(long)]
    forgejo_cache_file: Option<PathBuf>,
    #[arg(long, requires = "fixture_vault")]
    vault_folder: Vec<String>,
}
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let options = Options::parse();
    if !options.listen.ip().is_loopback() {
        return Err("Inbox listener must be loopback-only".into());
    }
    private_directory(&options.data_dir)?;
    let database = options.data_dir.join("inbox.db");
    if fs::symlink_metadata(&database).is_ok_and(|m| !m.is_file()) {
        return Err("Inbox database must be a regular file".into());
    }
    let mut auth = Auth::new(Store::open(&database)?, &options.origin)?;
    match options.command {
        Command::Bootstrap => {
            let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() as i64;
            let token = auth.bootstrap(now)?;
            // This explicit admin command is the only output of the secret.
            // Fragment avoids HTTP access logs and Referer; web clears it on load.
            println!("{}/#enroll={token}", auth.origin);
        }
        Command::Serve => {
            let lock = fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .write(true)
                .open(options.data_dir.join("server.lock"))?;
            lock.try_lock()
                .map_err(|_| "another Inbox server owns this data directory")?;
            auth.store.recover_discussions()?;
            let provider = match (
                options.ai_endpoint,
                options.ai_model,
                options.ai_credential_file,
            ) {
                (Some(endpoint), Some(model), Some(path)) => Some(std::sync::Arc::new(
                    tessera_inboxd::provider::Provider::from_credential(&endpoint, &model, &path)?,
                )),
                (None, None, None) => None,
                _ => return Err("incomplete AI configuration".into()),
            };
            let vault = options
                .fixture_vault
                .map(|path| tessera_inboxd::vault::Vault::open(&path, options.vault_folder))
                .transpose()?
                .map(std::sync::Arc::new);
            let bridge = options
                .bridge_credential_file
                .map(|path| tessera_inboxd::bridge::Bridge::from_credential(&path, auth.owner))
                .transpose()?;
            let sync = options.sync_config.and_then(|path| {
                let result=(|| -> Result<_,Box<dyn std::error::Error>> {
                    let config=tessera_inboxd::sync::Config::load(&path,auth.owner.0)?;
                    config.bind(&mut auth.store)?;
                    Ok(std::sync::Arc::new(config))
                })();
                match result {
                    Ok(config)=>Some(config),
                    Err(_)=>{eprintln!("sync_configuration_rejected: Sync disabled; Inbox login/capture remain available");None}
                }
            });
            let listener = tokio::net::TcpListener::bind(options.listen).await?;
            axum::serve(
                listener,
                tessera_inboxd::http::router_with_sync(
                    auth,
                    provider,
                    vault,
                    bridge,
                    options
                        .forgejo_cache_file
                        .map(tessera_inboxd::forgejo::Cache),
                    sync,
                ),
            )
            .with_graceful_shutdown(async {
                let _ = tokio::signal::ctrl_c().await;
            })
            .await?;
            drop(lock);
        }
    }
    Ok(())
}
fn private_directory(path: &std::path::Path) -> std::io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    if let Err(error) = builder.create(path) {
        if error.kind() != std::io::ErrorKind::AlreadyExists {
            return Err(error);
        }
    }
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(std::io::Error::other(
            "data directory must be a real directory",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(std::io::Error::other(
                "data directory must be private (mode 0700)",
            ));
        }
    }
    Ok(())
}

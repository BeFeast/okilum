//! One coherent service-file location per owning context (#2772).
//! Resolving never creates, converts, merges or deletes a file.
use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    fs::File,
    path::{Path, PathBuf},
};

pub mod migration;

pub const FILES: &[&str] = &[
    "project.toml",
    "project.yaml",
    "board.toml",
    "board.yaml",
    "board-local.toml",
    "board-local.yaml",
    "board-context.toml",
    "board-delivery.toml",
    "thread-ids.md",
];
pub const MIGRATION: &str = "service-migration.json";
const GENERATION: &str = ".service-layout-generation";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    Hidden,
    Legacy,
}
impl Kind {
    pub fn directory(self) -> &'static str {
        match self {
            Self::Hidden => ".okilum",
            Self::Legacy => "ops",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceLayout {
    pub context: PathBuf,
    pub kind: Kind,
    token: String,
    context_identity: String,
    directory_identity: String,
}

fn regular_directory(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(meta) => {
            ensure!(
                meta.is_dir(),
                "{} must be a directory, not a symlink",
                path.display()
            );
            Ok(true)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e).with_context(|| format!("Cannot inspect {}", path.display())),
    }
}

pub(crate) fn regular_bytes(path: &Path) -> Result<Vec<u8>> {
    use std::io::Read;
    ensure!(
        fs::symlink_metadata(path)?.is_file(),
        "{} must be a regular file, not a symlink",
        path.display()
    );
    let mut bytes = Vec::new();
    File::open(path)?
        .take(512 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= 512 * 1024,
        "Service file exceeds 512 KiB: {}",
        path.display()
    );
    Ok(bytes)
}

fn identity(path: &Path) -> Result<String> {
    let meta = fs::symlink_metadata(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(format!("{}:{}", meta.dev(), meta.ino()))
    }
    #[cfg(windows)]
    {
        let _ = meta;
        Ok(serde_json::to_string(
            &okilum_write_authority::filesystem_identity::read(path)?,
        )?)
    }
    #[cfg(not(any(unix, windows)))]
    {
        Ok(format!("{:?}", meta.created()?))
    }
}

/// Enumerate the *whole* service-file set, including unusable entries. A
/// preferred symlink/unreadable file must not disappear into legacy fallback.
pub(crate) fn inventory(context: &Path, kind: Kind) -> Result<Vec<String>> {
    let dir = context.join(kind.directory());
    if !regular_directory(&dir)? {
        return Ok(Vec::new());
    }
    let mut found = Vec::new();
    for entry in fs::read_dir(&dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if FILES.contains(&name.as_str()) {
            regular_bytes(&entry.path())?;
            found.push(name);
        } else if name.contains(".sync-conflict-")
            && ["project", "board", "thread-ids"]
                .iter()
                .any(|prefix| name.starts_with(prefix))
        {
            bail!(
                "Conflicting service file {}; reconcile it before using this layout",
                entry.path().display()
            );
        }
    }
    found.sort();
    Ok(found)
}

impl ServiceLayout {
    pub fn resolve(context: &Path) -> Result<Self> {
        Self::resolve_inner(context, false)
    }

    pub(crate) fn resolve_inner(context: &Path, recovering: bool) -> Result<Self> {
        ensure!(regular_directory(context)?, "Owning context is missing");
        if !recovering && fs::symlink_metadata(context.join(".okilum").join(MIGRATION)).is_ok() {
            bail!(
                "Service migration needs explicit recovery in {}; open Service storage",
                context.display()
            );
        }
        let hidden = inventory(context, Kind::Hidden)?;
        let legacy = inventory(context, Kind::Legacy)?;
        ensure!(hidden.is_empty() || legacy.is_empty(),
            "Conflicting service layouts: {} and {}; reconcile the complete set, not individual files",
            context.join(".okilum").display(), context.join("ops").display());
        let (kind, files) = if !legacy.is_empty() {
            (Kind::Legacy, &legacy)
        } else {
            (Kind::Hidden, &hidden)
        };
        ensure!(
            files.is_empty()
                || files
                    .iter()
                    .any(|n| n == "project.toml" || n == "project.yaml"),
            "Orphan service files in {}; restore the project manifest before creating or writing",
            context.join(kind.directory()).display()
        );
        if let Some(name) = ["project.toml", "project.yaml"]
            .into_iter()
            .find(|n| files.iter().any(|f| f == n))
        {
            let bytes = regular_bytes(&context.join(kind.directory()).join(name))?;
            crate::area_files::parse(
                std::str::from_utf8(&bytes)?,
                if name.ends_with(".toml") {
                    crate::area_files::Format::Toml
                } else {
                    crate::area_files::Format::Yaml
                },
            )?;
        }
        let directory = context.join(kind.directory());
        let generation = directory.join(GENERATION);
        let generation = match fs::symlink_metadata(&generation) {
            Ok(_) => String::from_utf8(regular_bytes(&generation)?)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(e.into()),
        };
        let context_identity = identity(context)?;
        let directory_identity = if regular_directory(&directory)? {
            identity(&directory)?
        } else {
            "absent".into()
        };
        let token = format!(
            "{context_identity}:{}:{directory_identity}:{generation}",
            kind.directory()
        );
        Ok(Self {
            context: context.to_owned(),
            kind,
            token,
            context_identity,
            directory_identity,
        })
    }

    pub fn directory(&self) -> PathBuf {
        self.context.join(self.kind.directory())
    }

    pub fn path(&self, name: &str) -> Result<PathBuf> {
        ensure!(FILES.contains(&name), "Unknown service file {name}");
        Ok(self.directory().join(name))
    }

    pub fn relative(&self, name: &str) -> Result<String> {
        self.path(name)?;
        Ok(format!("{}/{name}", self.kind.directory()))
    }

    /// Format precedence is scoped to the already-selected whole layout.
    pub fn preferred(&self, name: &str) -> Result<Option<PathBuf>> {
        ensure!(
            matches!(name, "project" | "board" | "board-local"),
            "Unknown Area manifest"
        );
        for extension in ["toml", "yaml"] {
            let path = self.path(&format!("{name}.{extension}"))?;
            match fs::symlink_metadata(&path) {
                Ok(_) => {
                    regular_bytes(&path)?;
                    return Ok(Some(path));
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(None)
    }

    pub fn check_current(&self) -> Result<()> {
        ensure!(
            Self::resolve(&self.context)? == *self,
            "Service location changed; reload before writing"
        );
        Ok(())
    }

    /// Only explicit creation may create a new service directory.
    pub fn create_directory(&self) -> Result<()> {
        self.check_current()?;
        if !regular_directory(&self.directory())? {
            fs::create_dir(self.directory())?;
        }
        Ok(())
    }

    /// Cross-process cooperative writer/migration lock, outside the Sync vault.
    pub fn lock(context: &Path) -> Result<Lock> {
        let root = state_directory(context)?;
        fs::create_dir_all(&root)?;
        ensure!(
            fs::symlink_metadata(&root)?.is_dir(),
            "Service state must not be a symlink"
        );
        let path = root.join("writer.lock");
        if let Ok(meta) = fs::symlink_metadata(&path) {
            ensure!(meta.is_file(), "Service lock is not a regular file");
        }
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        file.try_lock()
            .context("Service metadata is being edited or migrated; retry after it finishes")?;
        Ok(Lock { _file: file })
    }
}

pub(crate) fn state_directory(context: &Path) -> Result<PathBuf> {
    let canonical = context.canonicalize()?;
    let key = format!(
        "{:x}",
        Sha256::digest(canonical.to_string_lossy().as_bytes())
    );
    Ok(okilum_write_authority::account_directory()?
        .parent()
        .context("Missing account directory")?
        .join("service-layouts")
        .join(key))
}

pub struct Lock {
    _file: File,
}

/// Diagnostic/source-link display only. Failures stay visible, never a guessed
/// fallback path that could direct an agent to recreate legacy metadata.
pub fn display_path(context: &Path, name: &str) -> String {
    match ServiceLayout::resolve(context).and_then(|layout| layout.path(name)) {
        Ok(path) => path.display().to_string(),
        Err(error) => format!("Service metadata unavailable: {error:#}"),
    }
}

#[cfg(test)]
mod tests;

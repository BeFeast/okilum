//! Read-only project records (#1892). Canonical Markdown remains the authority.
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, io::Read, path::Path};

const MAX_FILE: usize = 256 * 1024;
const MAX_TOTAL: usize = 4 * 1024 * 1024;
const MAX_ENTRIES: usize = 512;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Decisions,
    Reports,
    Handovers,
}
impl Kind {
    pub const ALL: [Self; 3] = [Self::Decisions, Self::Reports, Self::Handovers];
    pub fn label(self) -> &'static str {
        match self {
            Self::Decisions => "Decisions",
            Self::Reports => "Reports",
            Self::Handovers => "Handovers",
        }
    }
    pub fn directories(self) -> &'static [&'static str] {
        match self {
            Self::Decisions => &["ops/decisions", "decisions"],
            Self::Reports => &["ops/daily", "ops/reports", "reports"],
            Self::Handovers => &["handovers", "ops/handovers"],
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    pub path: String,
    pub title: String,
    pub date: Option<String>,
    /// Only an explicit authored status; never inferred from prose or a merge.
    pub status: Option<String>,
    pub body: String,
    pub revision: String,
}
impl Record {
    pub fn draft(&self, kind: Kind) -> String {
        format!(
            "Discuss {}: {}\nProject source: {}\nRead revision (SHA-256): {}\nRecorded date: {}\n\nPlease review this record and discuss the proposed change. The record has not been edited.\n\nQuestion or proposed change: ",
            kind.label(), self.title, self.path, self.revision, self.date.as_deref().unwrap_or("not specified")
        )
    }
}

#[derive(Default, Debug)]
pub struct Snapshot {
    pub records: Vec<Record>,
    pub problems: Vec<String>,
}

/// Bounded, deterministic discovery of documented directories, with no symlink
/// traversal and no vault-wide scan. All failures remain visible beside records.
pub fn read(project: &Path, kind: Kind) -> Snapshot {
    let mut snapshot = Snapshot::default();
    let mut budget = Budget {
        entries: 0,
        bytes: 0,
    };
    for directory in kind.directories() {
        visit(project, Path::new(directory), 0, &mut budget, &mut snapshot);
    }
    snapshot
        .records
        .sort_by(|a, b| b.date.cmp(&a.date).then_with(|| a.path.cmp(&b.path)));
    snapshot
}

struct Budget {
    entries: usize,
    bytes: usize,
}

fn visit(project: &Path, relative: &Path, depth: usize, budget: &mut Budget, out: &mut Snapshot) {
    let result = (|| -> anyhow::Result<()> {
        // Check every ancestor as well as the final entry. In particular an ops
        // symlink must not let conventional directories escape this project.
        let mut checked = project.to_path_buf();
        for part in relative.components() {
            checked.push(part);
            match std::fs::symlink_metadata(&checked) {
                Ok(meta) => anyhow::ensure!(!meta.is_symlink(), "symlinks are not followed"),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
                Err(e) => return Err(e.into()),
            }
        }
        anyhow::ensure!(depth <= 4, "directory depth exceeds 4");
        let mut entries = Vec::new();
        for entry in std::fs::read_dir(project.join(relative))? {
            anyhow::ensure!(
                budget.entries < MAX_ENTRIES,
                "record discovery exceeds 512 entries; refine the source folders"
            );
            budget.entries += 1;
            entries.push(entry?);
        }
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let path = relative.join(entry.file_name());
            let ty = entry.file_type()?;
            if ty.is_symlink() {
                out.problems
                    .push(format!("{}: symlinks are not followed", path.display()));
            } else if ty.is_dir() {
                visit(project, &path, depth + 1, budget, out);
            } else if path
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
            {
                match load(project, &path, budget) {
                    Ok(record) => out.records.push(record),
                    Err(error) => out.problems.push(format!("{}: {error:#}", path.display())),
                }
            }
        }
        Ok(())
    })();
    if let Err(error) = result {
        out.problems
            .push(format!("{}: {error:#}", relative.display()));
    }
}

fn load(project: &Path, path: &Path, budget: &mut Budget) -> anyhow::Result<Record> {
    let full = project.join(path);
    anyhow::ensure!(
        std::fs::symlink_metadata(&full)?.is_file(),
        "record must be a regular file, not a symlink"
    );
    anyhow::ensure!(
        full.canonicalize()?.starts_with(project.canonicalize()?),
        "record escapes project"
    );
    let mut bytes = Vec::new();
    std::fs::File::open(&full)?
        .take((MAX_FILE + 1) as u64)
        .read_to_end(&mut bytes)?;
    anyhow::ensure!(
        bytes.len() <= MAX_FILE,
        "record exceeds 256 KiB; not truncated"
    );
    budget.bytes += bytes.len();
    anyhow::ensure!(
        budget.bytes <= MAX_TOTAL,
        "records exceed 4 MiB; not truncated"
    );
    let revision = format!("{:x}", Sha256::digest(&bytes));
    let source = String::from_utf8(bytes)?;
    let normalized = source.replace("\r\n", "\n");
    let (metadata, body) = if let Some(rest) = normalized.strip_prefix("---\n") {
        let (yaml, body) = rest
            .split_once("\n---\n")
            .ok_or_else(|| anyhow::anyhow!("unclosed YAML frontmatter"))?;
        (
            serde_yaml::from_str::<BTreeMap<String, serde_yaml::Value>>(yaml)?,
            body,
        )
    } else {
        (BTreeMap::new(), normalized.as_str())
    };
    let field = |key: &str| {
        metadata
            .get(key)
            .and_then(serde_yaml::Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    };
    let title = field("title")
        .or_else(|| {
            body.lines().find_map(|line| {
                line.strip_prefix("# ")
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
            })
        })
        .unwrap_or_else(|| {
            path.file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .replace(['-', '_'], " ")
        });
    let filename = path.file_name().unwrap_or_default().to_string_lossy();
    let date = field("date").or_else(|| {
        filename
            .get(..10)
            .filter(|date| chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").is_ok())
            .map(str::to_owned)
    });
    Ok(Record {
        path: path.to_string_lossy().replace('\\', "/"),
        title,
        date,
        status: field("status"),
        body: body.to_owned(),
        revision,
    })
}

#[cfg(test)]
mod tests;

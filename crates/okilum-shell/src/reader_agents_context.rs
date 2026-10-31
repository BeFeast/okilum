//! Local-only relevance and explicit Board setup for the Agents offer (#2093).
use anyhow::Result;
use std::path::Path;

pub fn project_folder(root: &Path) -> bool {
    if [
        ".git",
        "ops/project.toml",
        "ops/project.yaml",
        "Cargo.toml",
        "package.json",
        "pyproject.toml",
        "go.mod",
        "CMakeLists.txt",
        "Makefile",
    ]
    .iter()
    .any(|name| root.join(name).exists())
    {
        return true;
    }
    // Only this folder, with a bounded scan; never traverse a notes vault.
    std::fs::read_dir(root).ok().is_some_and(|entries| {
        entries.take(128).flatten().any(|entry| {
            entry.file_type().is_ok_and(|kind| kind.is_file())
                && entry
                    .path()
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .is_some_and(|ext| {
                        matches!(
                            ext.to_ascii_lowercase().as_str(),
                            "rs" | "py"
                                | "js"
                                | "jsx"
                                | "ts"
                                | "tsx"
                                | "go"
                                | "c"
                                | "cpp"
                                | "h"
                                | "cs"
                                | "swift"
                                | "java"
                                | "kt"
                                | "sln"
                                | "csproj"
                        )
                    })
        })
    })
}

fn t3_name(name: &std::ffi::OsStr) -> bool {
    matches!(
        name.to_string_lossy()
            .to_ascii_lowercase()
            .trim_end_matches(".exe"),
        "t3 code" | "t3-code" | "t3code" | "t3-code-server"
    )
}

pub fn local_t3_running() -> bool {
    use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::All,
        ProcessRefreshKind::new().with_exe(UpdateKind::OnlyIfNotSet),
    );
    system.processes().values().any(|process| {
        t3_name(process.name()) || process.exe().and_then(Path::file_name).is_some_and(t3_name)
    })
}

/// Explicit Enable may create the local project and Board; it never enrolls T3.
/// Existing manifests, Boards and notes remain authoritative.
pub fn open_local_board(root: &Path) -> Result<okilum_t3::projects::Project> {
    use okilum_t3::{
        board::manage::{Change, Session},
        projects::{create, Project},
    };
    let project = if create::is_project(root) {
        Project::read(root)?
    } else {
        create::create(root, root, &create::Draft::for_folder(root))?.project
    };
    let session = Session::load(root)?;
    if session.document.is_none() {
        session.save(&Change::Details(session.details()))?;
    }
    Ok(project)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn notes_are_not_a_project_but_checkout_file_code_and_manifest_are() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("note.md"), "# Notes").unwrap();
        assert!(!project_folder(temp.path()));
        for marker in [".git", "main.rs", "ops/project.yaml"] {
            let path = temp.path().join(marker);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, "local marker").unwrap();
            assert!(project_folder(temp.path()), "positive control: {marker}");
            std::fs::remove_file(path).unwrap();
        }
        assert!(!project_folder(temp.path()));
    }
    #[test]
    fn only_t3_executables_count_not_a_generic_node_or_editor_process() {
        for name in ["T3 Code", "t3-code", "T3-Code.exe", "t3-code-server"] {
            assert!(t3_name(name.as_ref()));
        }
        for name in ["node", "code", "my-t3-code-helper", "t3-code-notes.md"] {
            assert!(!t3_name(name.as_ref()));
        }
    }
    #[test]
    fn explicit_enable_creates_an_empty_board_and_preserves_existing_note_bytes() {
        let temp = tempfile::tempdir().unwrap();
        let note = temp.path().join("note.md");
        let original = b"# Exact\r\nsource bytes\r\n";
        std::fs::write(&note, original).unwrap();
        let project = open_local_board(temp.path()).unwrap();
        assert_eq!(project.folder, temp.path());
        let session = okilum_t3::board::manage::Session::load(temp.path()).unwrap();
        assert!(session.document.is_some());
        assert!(session.cards().is_empty());
        assert!(session.details().repository.is_empty());
        assert!(session.details().stage_note.is_empty());
        let manifest =
            std::fs::read(temp.path().join(okilum_t3::projects::create::MANIFEST)).unwrap();
        let board = std::fs::read(temp.path().join("ops/board.toml")).unwrap();
        open_local_board(temp.path()).unwrap();
        assert_eq!(std::fs::read(&note).unwrap(), original);
        assert_eq!(
            std::fs::read(temp.path().join(okilum_t3::projects::create::MANIFEST)).unwrap(),
            manifest
        );
        assert_eq!(
            std::fs::read(temp.path().join("ops/board.toml")).unwrap(),
            board
        );
    }
}

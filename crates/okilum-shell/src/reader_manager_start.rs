//! Explicit, retained manager launch. A lost launch response is never retried.
use super::*;
use anyhow::{ensure, Context as _, Result};
use okilum_t3::{
    projects::{Manager, Project},
    ManagerGateway,
};
use std::io::Write;

pub(super) fn start(
    area: &Path,
    context: &str,
    state: &Path,
    gateway: &dyn ManagerGateway,
) -> Result<String> {
    let project = Project::read(area)?;
    let project_id = project
        .manager_project
        .as_deref()
        .context("This project has no T3 manager project configured")?;
    ensure!(
        gateway.list_projects()?.iter().any(|p| p.id == project_id),
        "The configured T3 project is unavailable"
    );
    if let Manager::Thread(thread) = &project.manager {
        let threads = gateway.list_threads(project_id)?;
        ensure!(
            threads
                .iter()
                .find(|t| &t.thread_id == thread)
                .is_none_or(|t| t.settled),
            "Manager is still available; refresh Projects instead of starting another"
        );
    }
    start_inner(
        area,
        context,
        state,
        &project,
        |project_id, title, message| {
            gateway
                .start_manager(project_id, title, message)
                .map_err(anyhow::Error::from)
        },
    )
}

fn start_inner(
    area: &Path,
    context: &str,
    state: &Path,
    project: &Project,
    launch: impl FnOnce(&str, &str, &str) -> Result<String>,
) -> Result<String> {
    let project_id = project
        .manager_project
        .as_deref()
        .context("T3 project is not configured")?;
    let current = Project::read(area)?;
    ensure!(
        current.manager == project.manager
            && current.manager_project == project.manager_project
            && current.environment == project.environment,
        "Project binding changed before launch"
    );
    let registry = area.join("ops/thread-ids.md");
    if !registry.exists() {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&registry)?;
        file.write_all(b"# Thread registry\n\n| Role | Thread |\n|---|---|\n| Manager | ")?;
        file.write_all("— (not started) |\n".as_bytes())?;
        file.sync_all()?;
    }
    ensure!(
        !okilum_core::file_editor::FileEditor::has_unsaved_draft(
            &registry,
            &state.join("manager-registry-drafts")
        )?,
        "An unsaved registry draft needs recovery before starting a manager"
    );
    // Reuse the lossless revision-aware editor; no rendered-note read or ad-hoc rename.
    let mut editor = okilum_core::file_editor::FileEditor::open(
        &registry,
        &state.join("manager-registry-drafts"),
    )?;
    let source = editor.text().to_owned();
    ensure!(
        okilum_t3::projects::manager_role(&source)? == project.manager,
        "Manager role changed"
    );
    let directory = state.join("manager-starts");
    std::fs::create_dir_all(&directory)?;
    let key = format!(
        "{:x}",
        sha2::Sha256::digest(
            format!(
                "{}\0{:?}\0{}\0{:?}",
                area.display(),
                project.manager,
                project_id,
                project.environment
            )
            .as_bytes()
        )
    );
    let intent = directory.join(format!("{key}.json"));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&intent)
        .context(
            "A retained manager launch already exists; reconcile it before starting another",
        )?;
    let mut record = serde_json::json!({ "area": area, "projectId": project_id, "outcome": "unconfirmed", "threadId": null, "expectedManager": format!("{:?}", project.manager), "environment": project.environment });
    file.write_all(serde_json::to_string(&record)?.as_bytes())?;
    file.sync_all()?;
    #[cfg(unix)]
    std::fs::File::open(&directory)?.sync_all()?;
    let workflow = match project.kind {
        okilum_t3::projects::Kind::Knowledge => "Knowledge project: work in the vault root using the knowledge workflow. No repo, executor, CI or release is required.",
        okilum_t3::projects::Kind::Code => "Code project: use the area's linked repository and existing code workflow.",
        okilum_t3::projects::Kind::Hybrid => "Hybrid project: preserve the vault context and use its linked repository for code work.",
    };
    let context = context.trim_matches('/');
    ensure!(
        !context.contains(['`', '\n', '\r']),
        "Project context path cannot be represented unambiguously in a manager brief"
    );
    let prefix = if context.is_empty() {
        String::new()
    } else {
        format!("{context}/")
    };
    let message = format!(
        "прочитай `{prefix}ops/manager-state.md` и `{prefix}AGENTS.md`, ты manager.\n{workflow}"
    );
    let thread = launch(project_id, &format!("[{}] Manager", project.name), &message)?;
    record["threadId"] = thread.clone().into();
    record["outcome"] = "created_registry_pending".into();
    // Keep a separate immutable receipt: a crash writing it still leaves the
    // original unconfirmed intent, which blocks another non-idempotent launch.
    let receipt = directory.join(format!("{key}-created.json"));
    let mut receipt_file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&receipt)?;
    receipt_file.write_all(serde_json::to_string(&record)?.as_bytes())?;
    receipt_file.sync_all()?;
    #[cfg(unix)]
    std::fs::File::open(&directory)?.sync_all()?;
    let save = (|| -> Result<()> {
        let current = Project::read(area)?;
        ensure!(
            current.manager_project == project.manager_project
                && current.environment == project.environment,
            "Project binding changed during launch"
        );
        editor.set_text(okilum_t3::projects::register_manager(
            &source,
            &project.manager,
            &thread,
        )?)?;
        ensure!(
            editor.save()? == okilum_core::file_editor::Save::Saved,
            "Manager registry changed concurrently"
        );
        Ok(())
    })();
    save.with_context(|| {
        format!(
            "Manager {thread} was created, but its registry update failed; do not start another"
        )
    })?;
    Ok(thread)
}

use sha2::Digest;

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf, Project) {
        let temp = tempfile::tempdir().unwrap();
        let area = temp.path().join("vault/Music/Areas/Studio");
        let state = temp.path().join("app-state");
        std::fs::create_dir_all(area.join("ops")).unwrap();
        std::fs::write(area.join("ops/project.yaml"), "name: Studio\nkind: knowledge\nt3:\n  schema_version: 1\n  environment: test\n  manager: {project_id: brain, thread_id: null, workspace_mode: local}\n").unwrap();
        std::fs::write(area.join("ops/thread-ids.md"), "# Registry\r\n\r\n| Role | Thread | Since |\r\n|---|---|---|\r\n| Manager | — (not started) | today |\r\n\r\nUnrelated text.\r\n").unwrap();
        let project = Project::read(&area).unwrap();
        (temp, area, state, project)
    }

    #[test]
    fn explicit_launch_keeps_unrelated_registry_bytes_and_knowledge_context() {
        let (_temp, area, state, project) = fixture();
        let before = std::fs::read_to_string(area.join("ops/thread-ids.md")).unwrap();
        let id = start_inner(
            &area,
            "Music/Areas/Studio",
            &state,
            &project,
            |id, title, message| {
                assert_eq!(id, "brain");
                assert_eq!(title, "[Studio] Manager");
                assert!(message.contains("`Music/Areas/Studio/ops/manager-state.md`"));
                assert!(message.contains("Knowledge project: work in the vault root"));
                Ok("mcp:created".into())
            },
        )
        .unwrap();
        assert_eq!(id, "mcp:created");
        let after = std::fs::read_to_string(area.join("ops/thread-ids.md")).unwrap();
        assert_eq!(after, before.replace("— (not started)", "`mcp:created`"));
        assert!(
            std::fs::read_dir(state.join("manager-starts"))
                .unwrap()
                .count()
                >= 2
        );
    }

    #[test]
    fn lost_response_blocks_a_second_launch_across_reopening() {
        let (_temp, area, state, project) = fixture();
        let before = std::fs::read(area.join("ops/thread-ids.md")).unwrap();
        assert!(start_inner(
            &area,
            "Music/Areas/Studio",
            &state,
            &project,
            |_, _, _| anyhow::bail!("lost response")
        )
        .is_err());
        let second = start_inner(&area, "Music/Areas/Studio", &state, &project, |_, _, _| {
            panic!("must not send a second launch")
        });
        assert!(format!("{:#}", second.unwrap_err()).contains("retained manager launch"));
        assert_eq!(
            std::fs::read(area.join("ops/thread-ids.md")).unwrap(),
            before
        );
    }

    #[test]
    fn registry_edit_during_launch_is_retained_and_the_created_id_remains_visible() {
        let (_temp, area, state, project) = fixture();
        let registry = area.join("ops/thread-ids.md");
        let mut edited = std::fs::read_to_string(&registry).unwrap();
        edited.push_str("An external edit.\r\n");
        let result = start_inner(&area, "Music/Areas/Studio", &state, &project, |_, _, _| {
            std::fs::write(&registry, &edited)?;
            Ok("mcp:created".into())
        });
        let error = format!("{:#}", result.unwrap_err());
        assert!(error.contains("mcp:created"));
        assert_eq!(std::fs::read_to_string(&registry).unwrap(), edited);
        assert!(std::fs::read_dir(state.join("manager-starts"))
            .unwrap()
            .any(|entry| std::fs::read_to_string(entry.unwrap().path())
                .unwrap()
                .contains("mcp:created")));
    }
}

//! User-selected templates, loaded only after explicit Settings/create actions.
use super::*;
use anyhow::{Context as _, Result};
use gpui_component::menu::{PopupMenu, PopupMenuItem};
#[cfg(test)]
use std::io::Read as _;

pub const DEFAULT_FOLDER: &str = "_Assets/Templates";
#[cfg(test)]
const MAX_TEMPLATE_BYTES: u64 = 1024 * 1024;

fn preferences_path(root: &Path, state: &Path) -> Result<PathBuf> {
    let root = root.canonicalize()?;
    let keyed = reader_sidebar::State::path(state, &root);
    let path = state
        .join("templates")
        .join(keyed.file_name().context("Missing preference key")?);
    reader_layout::settings_path_at(&root, path)
        .context("Template preferences require storage outside the vault")
}
fn valid_folder(folder: &str) -> bool {
    !folder.is_empty()
        && Path::new(folder).components().all(|p| {
            matches!(
                p,
                std::path::Component::Normal(_) | std::path::Component::CurDir
            )
        })
}
pub fn load(root: &Path, state: &Path) -> Result<String> {
    Ok(configured_folder(root, state)?.unwrap_or_else(|| DEFAULT_FOLDER.into()))
}
pub(super) fn configured_folder(root: &Path, state: &Path) -> Result<Option<String>> {
    let path = preferences_path(root, state)?;
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let value: serde_json::Value = serde_json::from_slice(&bytes)?;
    let folder = value["folder"]
        .as_str()
        .context("Invalid template folder preference")?;
    anyhow::ensure!(
        valid_folder(folder),
        "Choose a templates folder inside the vault"
    );
    Ok(Some(folder.into()))
}
pub fn save(root: &Path, state: &Path, chosen: &Path) -> Result<String> {
    let root = root.canonicalize()?;
    let chosen = chosen.canonicalize()?;
    anyhow::ensure!(chosen.is_dir(), "Choose a folder");
    let rel = chosen
        .strip_prefix(&root)
        .context("Choose a templates folder inside this vault")?;
    let folder = if rel.as_os_str().is_empty() {
        ".".into()
    } else {
        rel.to_str().context("Use a UTF-8 folder name")?.to_owned()
    };
    let path = preferences_path(&root, state)?;
    let parent = path.parent().context("Missing preference directory")?;
    std::fs::create_dir_all(parent)?;
    let temp = parent.join(format!(".templates-{}.json", uuid::Uuid::new_v4()));
    std::fs::write(
        &temp,
        serde_json::to_vec(&serde_json::json!({"folder":folder}))?,
    )?;
    std::fs::rename(&temp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&temp);
    })?;
    Ok(folder)
}
fn list(root: &Path, folder: &str) -> Result<Vec<PathBuf>> {
    anyhow::ensure!(valid_folder(folder), "Invalid template folder");
    let root = root.canonicalize()?;
    let folder = root
        .join(folder)
        .canonicalize()
        .context("Templates folder is missing. Choose it in Settings → Files")?;
    anyhow::ensure!(
        folder.starts_with(&root),
        "Templates folder is outside the vault"
    );
    let mut templates = Vec::new();
    for entry in std::fs::read_dir(folder)? {
        let entry = entry?;
        if entry.file_type()?.is_file()
            && entry
                .path()
                .extension()
                .is_some_and(|s| s.eq_ignore_ascii_case("md"))
        {
            templates.push(entry.path());
        }
    }
    templates.sort();
    anyhow::ensure!(!templates.is_empty(), "No Markdown templates in this folder. Add a .md template or choose another folder in Settings → Files");
    Ok(templates)
}
#[cfg(test)]
fn read_template(root: &Path, path: &Path) -> Result<String> {
    let root = root.canonicalize()?;
    let path = path.canonicalize()?;
    anyhow::ensure!(
        path.starts_with(root) && path.is_file(),
        "Template must be a file inside the vault"
    );
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(MAX_TEMPLATE_BYTES + 1)
        .read_to_end(&mut bytes)?;
    anyhow::ensure!(
        bytes.len() as u64 <= MAX_TEMPLATE_BYTES,
        "Template exceeds 1 MiB"
    );
    String::from_utf8(bytes).context("Template must be UTF-8 Markdown")
}
#[cfg(test)]
fn expand(template: &str, title: &str, date: &str) -> String {
    regex::Regex::new(r"\{\{(title|date)\}\}")
        .unwrap()
        .replace_all(template, |c: &regex::Captures| {
            if &c[1] == "title" {
                title.to_owned()
            } else {
                date.to_owned()
            }
        })
        .into_owned()
}

impl Reader {
    pub(super) fn new_from_template(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(state) = self.session_directory.clone() else {
            return;
        };
        let root = self.vault_root.clone();
        cx.spawn_in(window, async move |this, cx| {
            let scan_root = root.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    let folder = load(&scan_root, &state)?;
                    let templates = list(&scan_root, &folder)?;
                    Ok::<_, anyhow::Error>((folder, templates))
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.vault_root != root {
                    return;
                }
                match result {
                    Ok((_folder, templates)) => {
                        let reader = cx.entity().downgrade();
                        let popup = PopupMenu::build(window, cx, move |mut menu, _, _| {
                            for path in &templates {
                                let path = path.clone();
                                let reader = reader.clone();
                                let label = path
                                    .file_stem()
                                    .unwrap_or_default()
                                    .to_string_lossy()
                                    .into_owned();
                                menu = menu.item(PopupMenuItem::new(label).on_click(
                                    move |_, window, cx| {
                                        let _ = reader.update(cx, |reader, cx| {
                                            reader.create_from_template(path.clone(), window, cx)
                                        });
                                    },
                                ));
                            }
                            menu
                        });
                        cx.subscribe(&popup, |this, _, _: &DismissEvent, cx| {
                            this.file_menu = None;
                            cx.notify();
                        })
                        .detach();
                        popup.focus_handle(cx).focus(window, cx);
                        this.file_menu = Some((popup, window.mouse_position()));
                        cx.notify();
                    }
                    Err(error) => {
                        this.link_notice = Some(format!("Cannot use template: {error:#}"));
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }
    fn create_from_template(
        &mut self,
        template: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name = template
            .file_name()
            .and_then(|n| n.to_str())
            .map(str::to_owned);
        self.new_note(None, window, cx);
        if let Some(name) = name {
            self.choose_creation_template(Some(name), window, cx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[test]
    fn folder_preferences_are_per_root_and_do_not_write_into_vaults() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        let other = temp.path().join("other");
        let state = temp.path().join("state");
        std::fs::create_dir_all(root.join("My Templates")).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        assert_eq!(load(&root, &state).unwrap(), DEFAULT_FOLDER);
        assert_eq!(
            save(&root, &state, &root.join("My Templates")).unwrap(),
            "My Templates"
        );
        assert_eq!(load(&root, &state).unwrap(), "My Templates");
        assert_eq!(load(&other, &state).unwrap(), DEFAULT_FOLDER);
        assert!(save(&root, &state, &other).is_err());
        assert!(save(&root, &root.join("state"), &root.join("My Templates")).is_err());
        assert!(!root.join("state").exists());
    }
    #[test]
    fn template_read_and_expansion_preserve_source_and_reject_escape() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        std::fs::create_dir_all(root.join(DEFAULT_FOLDER)).unwrap();
        let template = root.join(DEFAULT_FOLDER).join("Note.md");
        let source = "---\r\ncreated: {{date}}\r\n---\r\n# {{title}}\r\n[[reference]]";
        std::fs::write(&template, source).unwrap();
        std::fs::write(root.join(DEFAULT_FOLDER).join("ignore.txt"), "ignore").unwrap();
        assert_eq!(list(&root, DEFAULT_FOLDER).unwrap().len(), 1);
        let read = read_template(&root, &template).unwrap();
        assert_eq!(
            expand(&read, "Literal {{date}}", "2026-10-05"),
            source
                .replace("{{date}}", "2026-10-05")
                .replace("{{title}}", "Literal {{date}}")
        );
        assert_eq!(std::fs::read_to_string(template).unwrap(), source);
        std::fs::write(temp.path().join("outside.md"), "outside").unwrap();
        std::os::unix::fs::symlink(temp.path().join("outside.md"), root.join("escape.md")).unwrap();
        assert!(read_template(&root, &root.join("escape.md")).is_err());
        assert!(list(&root, "../").is_err());
        assert!(list(&root, "absent").is_err());
    }
}

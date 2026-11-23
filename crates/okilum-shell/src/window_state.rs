//! Window geometry lives in `okilum_shell_kit::window_state` (#3233); the
//! Reader window key derives from the app's command-line options.
pub(crate) use okilum_shell_kit::window_state::*;

pub(crate) fn reader_key(opts: &super::Opts) -> String {
    let path = opts.vault.clone().or_else(|| {
        opts.open_path.as_ref().map(|path| {
            let path = path.canonicalize().unwrap_or_else(|_| path.clone());
            if path.is_dir() {
                path
            } else if let Some(root) = opts
                .reusable_roots
                .iter()
                .find(|root| path.starts_with(root))
            {
                root.clone()
            } else {
                let parent = path.parent().unwrap_or(&path);
                parent
                    .ancestors()
                    .find(|root| root.join(".obsidian").is_dir())
                    .unwrap_or(parent)
                    .to_path_buf()
            }
        })
    });
    path.map(|p| {
        let p = p.canonicalize().unwrap_or(p);
        format!("reader:{}", p.to_string_lossy())
    })
    .unwrap_or_else(|| "reader".into())
}

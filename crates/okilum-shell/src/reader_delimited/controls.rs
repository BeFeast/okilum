use super::*;
use std::process::{Command, Stdio};

impl TablePreview {
    pub(super) fn open_duckdb(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.external_pending {
            return;
        }
        self.external_error = None;
        self.external_pending = true;
        let path = self.path.clone();
        #[cfg(test)]
        let program_override = self.duckdb_override.clone();
        let (send, receive) = std::sync::mpsc::channel();
        // The external UI can outlive this view; reap it without occupying a
        // GPUI executor. Keep stdin open so the CLI cannot exit at pipe EOF.
        let started = std::thread::Builder::new().name("duckdb-ui".into()).spawn(move || {
            let result = (|| -> anyhow::Result<()> {
                #[cfg(not(test))]
                let program = duckdb_program();
                #[cfg(test)]
                let program = program_override.unwrap_or_else(duckdb_program);
                let program = program.ok_or_else(|| anyhow::anyhow!("Install the DuckDB CLI with UI support from duckdb.org/docs/installation, then retry."))?;
                let args = duckdb_args(&path)?;
                let mut child = Command::new(program).args(args).stdin(Stdio::piped())
                    .stdout(Stdio::null()).stderr(Stdio::null()).spawn()?;
                let _input = child.stdin.take();
                anyhow::ensure!(child.wait()?.success(), "DuckDB could not open this file. Check that your DuckDB CLI supports -ui and this file format.");
                Ok(())
            })();
            let _ = send.send(result.map_err(|e| format!("Cannot open in DuckDB: {e:#}")));
        });
        if let Err(error) = started {
            self.external_pending = false;
            self.external_error = Some(format!("Cannot start DuckDB: {error}"));
            reader_toast::duckdb_error(self.external_error.clone().unwrap(), window, cx);
        } else {
            cx.spawn_in(window, async move |this, cx| loop {
                if this.upgrade().is_none() {
                    break;
                }
                let result = match receive.try_recv() {
                    Ok(result) => result,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
                    Err(std::sync::mpsc::TryRecvError::Empty) => {
                        cx.background_executor()
                            .timer(Duration::from_millis(100))
                            .await;
                        continue;
                    }
                };
                let _ = this.update_in(cx, |this, window, cx| {
                    this.external_pending = false;
                    this.external_error = result.err();
                    if let Some(error) = &this.external_error {
                        reader_toast::duckdb_error(error.clone(), window, cx);
                    }
                    cx.notify();
                });
                break;
            })
            .detach();
        }
        cx.notify();
    }
}

fn duckdb_program() -> Option<PathBuf> {
    let executable = if cfg!(windows) {
        "duckdb.exe"
    } else {
        "duckdb"
    };
    let mut directories: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    directories.extend([
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
    ]);
    if let Some(home) = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }) {
        directories.push(PathBuf::from(home).join(".duckdb/cli/latest"));
    }
    directories
        .into_iter()
        .map(|dir| dir.join(executable))
        .find(|path| path.is_file())
}

fn duckdb_args(path: &Path) -> anyhow::Result<Vec<String>> {
    let path = std::fs::canonicalize(path)?;
    let path = okilum_core::vault::plain_path(&path);
    let path = path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("DuckDB requires a UTF-8 file path"))?;
    let pattern: String = path
        .chars()
        .map(|ch| match ch {
            '[' => "[[]".to_owned(),
            ']' => "[]]".to_owned(),
            '*' => "[*]".to_owned(),
            '?' => "[?]".to_owned(),
            '\\' if cfg!(windows) => "/".to_owned(),
            ch => ch.to_string(),
        })
        .collect();
    let function = if path.to_ascii_lowercase().ends_with(".json") {
        "read_json_auto"
    } else {
        "read_csv_auto"
    };
    // A SQL string literal, never a shell command. Disable glob expansion so
    // filenames containing [] or * cannot select neighbouring files.
    let sql = format!(
        "CREATE VIEW okilum_data AS SELECT * FROM {function}('{}');",
        pattern.replace('\'', "''")
    );
    Ok(vec!["-cmd".into(), sql, "-ui".into()])
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    #[test]
    pub(super) fn duckdb_uses_one_escaped_file_and_no_shell() {
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path().join("O'Brien [1]; $HOME.json");
        std::fs::write(&path, "[]").unwrap();
        let args = duckdb_args(&path).unwrap();
        assert_eq!(args.last().unwrap(), "-ui");
        assert!(args[1].contains("read_json_auto('"));
        assert!(args[1].contains("O''Brien [[]1[]]; $HOME.json"));
    }
}

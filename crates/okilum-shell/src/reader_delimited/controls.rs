use super::*;
use std::process::{Command, Stdio};

impl TablePreview {
    #[inline(never)]
    pub(super) fn toolbar(&self, cx: &mut Context<Self>) -> AnyElement {
        let table = self.table.as_ref().unwrap();
        let loaded = table.rows.len().saturating_sub(usize::from(self.header));
        let scope = if table.limited || (!self.all && loaded > INITIAL_ROWS) {
            " · partial file"
        } else {
            ""
        };
        let label = if self.updating {
            "Updating rows…".into()
        } else {
            format!(
                "{} / {} loaded rows · {} columns{scope}",
                self.order.len(),
                loaded.min(if self.all { usize::MAX } else { INITIAL_ROWS }),
                table.columns
            )
        };
        let filter = Input::new(self.filter.as_ref().unwrap())
            .small()
            .cleanable(true)
            .into_any_element();
        let external = Button::new("table-duckdb")
            .debug_selector(|| "table-duckdb".into())
            .small()
            .ghost()
            .label(if self.external_pending {
                "DuckDB is open"
            } else {
                "Open in DuckDB"
            })
            .disabled(self.external_pending)
            .on_click(cx.listener(|this, _, _, cx| this.open_duckdb(cx)))
            .into_any_element();
        let reset = Button::new("table-reset-sort")
            .small()
            .ghost()
            .label("Source order")
            .disabled(self.sort.is_none())
            .on_click(cx.listener(|this, _, _, cx| {
                this.sort = None;
                this.refilter(cx);
            }))
            .into_any_element();
        let controls = h_flex()
            .px_4()
            .py_2()
            .gap_2()
            .flex_wrap()
            .child(div().w(px(240.)).child(filter))
            .child(reset)
            .child(external)
            .into_any_element();
        v_flex()
            .flex_none()
            .child(controls)
            .child(
                div()
                    .px_4()
                    .pb_2()
                    .text_color(brand::palette(cx).text_muted)
                    .child(label),
            )
            .children(self.external_error.as_ref().map(|error| {
                let install = Button::new("table-get-duckdb")
                    .small()
                    .ghost()
                    .label("Get DuckDB")
                    .on_click(|_, _, cx| cx.open_url("https://duckdb.org/docs/installation/"))
                    .into_any_element();
                v_flex().px_4().pb_2().child(error.clone()).child(install)
            }))
            .into_any_element()
    }

    fn open_duckdb(&mut self, cx: &mut Context<Self>) {
        if self.external_pending {
            return;
        }
        self.external_error = None;
        self.external_pending = true;
        let path = self.path.clone();
        let (send, receive) = async_channel::bounded(1);
        // The external UI can outlive this view; reap it without occupying a
        // GPUI executor. Keep stdin open so the CLI cannot exit at pipe EOF.
        let started = std::thread::Builder::new().name("duckdb-ui".into()).spawn(move || {
            let result = (|| -> anyhow::Result<()> {
                let program = duckdb_program().ok_or_else(|| anyhow::anyhow!("Install the DuckDB CLI with UI support from duckdb.org/docs/installation, then retry."))?;
                let args = duckdb_args(&path)?;
                let mut child = Command::new(program).args(args).stdin(Stdio::piped())
                    .stdout(Stdio::null()).stderr(Stdio::null()).spawn()?;
                let _input = child.stdin.take();
                anyhow::ensure!(child.wait()?.success(), "DuckDB could not open this file. Check that your DuckDB CLI supports -ui and this file format.");
                Ok(())
            })();
            let _ = send.send_blocking(result.map_err(|e| format!("Cannot open in DuckDB: {e:#}")));
        });
        if let Err(error) = started {
            self.external_pending = false;
            self.external_error = Some(format!("Cannot start DuckDB: {error}"));
        } else {
            cx.spawn(async move |this, cx| {
                if let Ok(result) = receive.recv().await {
                    let _ = this.update(cx, |this, cx| {
                        this.external_pending = false;
                        this.external_error = result.err();
                        cx.notify();
                    });
                }
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
    fn duckdb_uses_one_escaped_file_and_no_shell() {
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path().join("O'Brien [1]; $HOME.json");
        std::fs::write(&path, "[]").unwrap();
        let args = duckdb_args(&path).unwrap();
        assert_eq!(args.last().unwrap(), "-ui");
        assert!(args[1].contains("read_json_auto('"));
        assert!(args[1].contains("O''Brien [[]1[]]; $HOME.json"));
    }
}

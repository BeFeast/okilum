//! Explicit whole-file copy, bounded and read off the UI thread. Display-only
//! formatting never becomes a source of clipboard text.
use super::*;
use std::io::Read as _;

const LIMIT: u64 = 64 * 1024 * 1024;

fn read_text(root: &Path, rel: &str) -> anyhow::Result<String> {
    let path = reader_files::checked_path(root, rel)?;
    let file = std::fs::File::open(path)?;
    anyhow::ensure!(
        file.metadata()?.len() <= LIMIT,
        "File content exceeds the 64 MB copy limit"
    );
    let mut bytes = Vec::new();
    file.take(LIMIT + 1).read_to_end(&mut bytes)?;
    anyhow::ensure!(
        bytes.len() as u64 <= LIMIT,
        "File content exceeds the 64 MB copy limit"
    );
    String::from_utf8(bytes).map_err(|_| anyhow::anyhow!("File content is not UTF-8 text"))
}

fn copy(text: String, window: &mut Window, cx: &mut App) {
    let size = size_label(text.len());
    cx.write_to_clipboard(ClipboardItem::new_string(text));
    reader_toast::transient(format!("File content copied · {size}"), window, cx);
}

fn size_label(bytes: usize) -> String {
    if bytes >= 1_000_000 {
        format!("{:.1} MB", bytes as f64 / 1_000_000.)
    } else if bytes >= 1_000 {
        format!("{:.1} KB", bytes as f64 / 1_000.)
    } else {
        format!("{bytes} bytes")
    }
}

impl Reader {
    pub(crate) fn copy_file_content(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !reader_viewer_chrome::copy_content_available(self) {
            return;
        }
        if let Some(editing) = self.editing.as_ref() {
            if editing.input().read(cx).value().len() as u64 > LIMIT {
                reader_toast::error("File content exceeds the 64 MB copy limit", window, cx);
                return;
            }
            if let Some(text) = self.source_text(cx) {
                copy(text, window, cx);
            }
            return;
        }
        let root = self.vault_root.clone();
        let rel = self.selected_file().to_owned();
        let read = cx
            .background_executor()
            .spawn(async move { read_text(&root, &rel) });
        window
            .spawn(cx, async move |cx| {
                let result = read.await;
                let _ = cx.update(|window, cx| match result {
                    Ok(text) => copy(text, window, cx),
                    Err(error) => reader_toast::error(
                        format!("Cannot copy file content: {error}"),
                        window,
                        cx,
                    ),
                });
            })
            .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[test]
    fn copy_reads_exact_disk_text_and_refuses_binary_and_oversized_files() {
        let temp = tempfile::tempdir().unwrap();
        let text = "\u{feff}{\"value\":1e2,\"value\":\"שלום\"}\r\n";
        std::fs::write(temp.path().join("file.json"), text).unwrap();
        assert_eq!(read_text(temp.path(), "file.json").unwrap(), text);
        std::fs::write(temp.path().join("binary.txt"), [0xff]).unwrap();
        assert!(read_text(temp.path(), "binary.txt")
            .unwrap_err()
            .to_string()
            .contains("UTF-8"));
        let file = std::fs::File::create(temp.path().join("large.txt")).unwrap();
        file.set_len(LIMIT + 1).unwrap();
        assert!(read_text(temp.path(), "large.txt")
            .unwrap_err()
            .to_string()
            .contains("copy limit"));
        assert_eq!(
            std::fs::read_to_string(temp.path().join("file.json")).unwrap(),
            text
        );
    }
}

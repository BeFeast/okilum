//! Completion-time selection check. Never replaces the authored selection with
//! the file currently occupying its path; only explicit attachment can do that.
use super::model;

pub(super) fn recovery_error(selected: Option<&model::Video>) -> Option<String> {
    let selected = selected?;
    match model::video_file(&selected.path) {
        Ok(current) if current == *selected => None,
        _ => Some(
            "The selected video changed, was replaced or is unavailable. Your draft and original selection are kept. Select the video again or remove it before retrying."
                .into(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_completion_retains_selection_and_refuses_changed_asset_until_reselected() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("recording.webm");
        std::fs::write(&path, b"original").unwrap();
        let selected = model::video_file(&path).unwrap();
        assert!(
            recovery_error(Some(&selected)).is_none(),
            "unchanged selection positive control"
        );
        let timestamp = std::fs::metadata(&path).unwrap().modified().unwrap();
        // Simulate an edit while the provider is running, even at the same
        // length and restored timestamp: the original bytes remain selected.
        std::fs::write(&path, b"replaced").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(timestamp))
            .unwrap();
        assert!(recovery_error(Some(&selected))
            .unwrap()
            .contains("before retrying"));
        assert_ne!(model::video_file(&path).unwrap(), selected);
        let explicitly_reselected = model::video_file(&path).unwrap();
        assert!(recovery_error(Some(&explicitly_reselected)).is_none());
        std::fs::remove_file(&path).unwrap();
        assert!(recovery_error(Some(&selected)).is_some());
        assert!(recovery_error(None).is_none(), "explicit remove recovers");
    }
}

//! Publication notices shared by Reader save/Repair/Restore error paths.
//! Consult before errno-based access notices: the cause can also contain EIO.
use okilum_core::file_editor::{PublishedSaveNeedsAttention, UnconfirmedSave};

pub(crate) fn publication_notice(error: &anyhow::Error) -> Option<&'static str> {
    if let Some(outcome) = error.downcast_ref::<UnconfirmedSave>() {
        return Some(match outcome {
            UnconfirmedSave::UnknownPublication => "The drive did not confirm the save. Your draft and recovery copy are kept. Inspect the file before retrying.",
            UnconfirmedSave::Published => "The file was replaced, but save completion was not confirmed. Recovery is kept. Inspect the file before retrying.",
        });
    }
    error.is::<PublishedSaveNeedsAttention>().then_some(
        "The save was published, but recovery changed during verification. Your draft is preserved. Inspect the file before saving again.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_outcome_wins_over_io_cause_and_survives_context() {
        for outcome in [
            UnconfirmedSave::UnknownPublication,
            UnconfirmedSave::Published,
        ] {
            let error = anyhow::Error::from(std::io::Error::other("lost reply"))
                .context(outcome)
                .context("Saving note");
            let notice = publication_notice(&error).unwrap();
            assert!(notice.contains("Inspect the file before retrying"));
            assert!(!notice.contains("couldn’t access"));
        }
        assert!(publication_notice(&anyhow::anyhow!("ordinary access failure")).is_none());
        assert!(publication_notice(&anyhow::Error::new(PublishedSaveNeedsAttention)).is_some());
    }
}

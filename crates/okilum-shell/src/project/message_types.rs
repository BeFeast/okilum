//! Pure composer state for #1478. Parent owns GPUI wiring and draft persistence.
//! Persist this state alongside the existing text/attachments draft; never add
//! these local fields to a T3 send request. Sending uses `source_text()`.
use okilum_t3::message_types::{initial_token, Author, MessageType, Metadata, Provenance};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TypedDraft {
    /// Original text always survives token conversion and Undo byte for byte.
    pub text: String,
    pub selection: Option<Metadata>,
    pub conversion_suppressed: bool,
    token_bytes: Option<usize>,
}
impl TypedDraft {
    pub fn from_text(text: String) -> Self {
        Self {
            text,
            ..Self::default()
        }
    }
    /// Restore local draft metadata, validating that a retained pill still has
    /// an exact supported token. Legacy drafts pass None/false/false.
    pub fn restore(
        text: String,
        selection: Option<Metadata>,
        suppressed: bool,
        pill: bool,
    ) -> Self {
        let token_bytes = if pill && !suppressed && selection.is_some() {
            initial_token(&text).map(|(_, bytes)| bytes)
        } else {
            None
        };
        Self {
            text,
            selection,
            conversion_suppressed: suppressed,
            token_bytes,
        }
    }
    pub fn has_token_pill(&self) -> bool {
        self.token_bytes.is_some()
    }
    pub fn source_text(&self) -> &str {
        &self.text
    }
    pub fn visible_text(&self) -> &str {
        self.token_bytes
            .and_then(|bytes| self.text.get(bytes..))
            .unwrap_or(&self.text)
    }
    /// Call on composer changes before rendering the pill. Buttons already set
    /// an explicit type, so automatic conversion cannot override that choice.
    pub fn detect_token(&mut self) -> bool {
        if self.conversion_suppressed || self.selection.is_some() {
            return false;
        }
        let Some((kind, bytes)) = initial_token(&self.text) else {
            return false;
        };
        self.selection = Some(Metadata {
            kind,
            provenance: Provenance::ExplicitToken,
            author: Author::User,
        });
        self.token_bytes = Some(bytes);
        true
    }
    /// Update the visible editor body without losing the original token spelling.
    pub fn edit_body(&mut self, body: &str) {
        if let Some(bytes) = self.token_bytes {
            self.text.truncate(bytes);
            self.text.push_str(body);
        } else {
            self.text = body.into();
        }
    }
    pub fn select(&mut self, kind: MessageType) {
        self.selection = Some(Metadata {
            kind,
            provenance: Provenance::ExplicitSelection,
            author: Author::User,
        });
        // Keep an existing token hidden until Undo, preserving its source bytes.
    }
    /// Esc or Backspace immediately after pill conversion. Suppression lasts
    /// for this draft, including subsequent body edits and draft restoration.
    pub fn undo_token(&mut self) -> bool {
        if self.token_bytes.take().is_none() {
            return false;
        }
        self.selection = None;
        self.conversion_suppressed = true;
        true
    }
    /// Suggestions remain suggestions until explicitly accepted by the user.
    pub fn accept_suggestion(&mut self, kind: MessageType) {
        if self.selection.is_none() {
            self.selection = Some(Metadata {
                kind,
                provenance: Provenance::ManagerClassification,
                author: Author::User,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn token_edit_undo_and_restore_preserve_source() {
        let mut draft = TypedDraft::from_text("FR: original".into());
        assert!(draft.detect_token());
        assert_eq!(draft.visible_text(), " original");
        draft.edit_body(" revised");
        assert_eq!(draft.source_text(), "FR: revised");
        assert!(draft.undo_token());
        assert_eq!(draft.visible_text(), "FR: revised");
        let mut restored = TypedDraft::restore(
            draft.text.clone(),
            draft.selection,
            draft.conversion_suppressed,
            draft.has_token_pill(),
        );
        restored.edit_body("BUG broken");
        assert!(!restored.detect_token());
        assert_eq!(restored.selection, None);
    }
    #[test]
    fn explicit_selection_wins_and_new_drafts_are_independent() {
        let mut draft = TypedDraft::from_text("BUG broken".into());
        draft.select(MessageType::Status);
        assert!(!draft.detect_token());
        draft.accept_suggestion(MessageType::FeatureRequest);
        assert_eq!(draft.selection.unwrap().kind, MessageType::Status);
        assert_eq!(draft.source_text(), "BUG broken");
        assert!(!draft.undo_token());
        let mut other = TypedDraft::from_text("BUG broken".into());
        assert!(other.detect_token());
        assert_eq!(other.selection.unwrap().kind, MessageType::Bug);
    }
}

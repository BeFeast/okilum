//! Pure composer state for #1478. Parent owns GPUI wiring and draft persistence.
//! Persist this state alongside the existing text/attachments draft; never add
//! these local fields to a T3 send request. Sending uses `source_text()`.
use okilum_t3::message_types::{initial_token, Author, MessageType, Metadata, Provenance};

#[path = "message_types/integration.rs"]
mod integration;
pub(super) use integration::State;

/// Exact chat source, separate from normalized prompt recall. Aliases are
/// existing transport identities used only for local authored-type lookup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Source {
    pub id: String,
    pub text: String,
    pub author: Author,
    pub aliases: Vec<String>,
}

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
    #[cfg_attr(not(test), allow(dead_code, reason = "exercised only by tests"))]
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

/// Versioned local draft payload. Store alongside the existing attachments with
/// the same conversation identity and revision check, outside the derived index.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DraftMetadata {
    version: u32,
    selection: Option<Metadata>,
    conversion_suppressed: bool,
    token_pill: bool,
}
impl TypedDraft {
    pub fn retained_metadata(&self) -> DraftMetadata {
        DraftMetadata {
            version: 1,
            selection: self.selection,
            conversion_suppressed: self.conversion_suppressed,
            token_pill: self.has_token_pill(),
        }
    }
    /// Unsupported or inconsistent state is surfaced, never silently discarded.
    pub fn restore_metadata(text: String, retained: DraftMetadata) -> Result<Self, &'static str> {
        if retained.version != 1 {
            return Err("Unsupported typed draft version");
        }
        if retained.token_pill
            && (retained.conversion_suppressed
                || retained.selection.is_none()
                || initial_token(&text).is_none())
        {
            return Err("Retained typed draft token does not match its source");
        }
        Ok(Self::restore(
            text,
            retained.selection,
            retained.conversion_suppressed,
            retained.token_pill,
        ))
    }
}

/// Filtering changes only the panel projection. Legacy untyped messages remain
/// Chat and can be found through the explicit Chat filter without classification.
pub fn filtered_messages<'a>(
    messages: &'a [okilum_t3::message_types::Message],
    environment: &str,
    project: &str,
    filter: Option<MessageType>,
) -> Vec<&'a okilum_t3::message_types::Message> {
    messages
        .iter()
        .filter(|message| {
            let kind = message
                .metadata
                .map_or(MessageType::Chat, |metadata| metadata.kind);
            message.key.environment == environment
                && message.key.project == project
                && filter.map_or(kind != MessageType::Chat, |filter| filter == kind)
        })
        .collect()
}

/// Compare the full original snapshot before preparing a correction. Persistence
/// and its revision check belong to the parent; the UI must not publish this
/// result until that write succeeds. Duplicate identities are ambiguous.
#[cfg_attr(not(test), allow(dead_code, reason = "exercised only by tests"))]
pub fn corrected_message(
    messages: &[okilum_t3::message_types::Message],
    expected: &okilum_t3::message_types::Message,
    kind: MessageType,
) -> Result<okilum_t3::message_types::Message, &'static str> {
    corrected_message_with_author(messages, expected, kind, None)
}

pub(super) fn corrected_message_with_author(
    messages: &[okilum_t3::message_types::Message],
    expected: &okilum_t3::message_types::Message,
    kind: MessageType,
    source_author: Option<Author>,
) -> Result<okilum_t3::message_types::Message, &'static str> {
    let mut matches = messages
        .iter()
        .filter(|message| message.key == expected.key);
    let source = matches.next().ok_or("Message is no longer available")?;
    if matches.next().is_some() {
        return Err("Message identity is ambiguous");
    }
    if source != expected {
        return Err("Message changed; review it before correcting its type");
    }
    let author = source
        .metadata
        .map(|metadata| metadata.author)
        .or(source_author)
        .ok_or("Message author metadata is unavailable")?;
    let mut corrected = source.clone();
    corrected.metadata = Some(Metadata {
        kind,
        provenance: Provenance::UserCorrection,
        author,
    });
    Ok(corrected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    #[test]
    fn filters_keep_legacy_chat_and_acknowledged_corrections_discoverable() {
        use okilum_t3::message_types::{Message, MessageKey};
        let source = Message {
            key: MessageKey {
                environment: "e".into(),
                project: "p".into(),
                thread: "t".into(),
                message: "m".into(),
            },
            text: "Body".into(),
            metadata: Some(Metadata {
                kind: MessageType::Bug,
                provenance: Provenance::ExplicitSelection,
                author: Author::User,
            }),
        };
        let legacy = Message {
            key: MessageKey {
                message: "legacy".into(),
                ..source.key.clone()
            },
            metadata: None,
            ..source.clone()
        };
        let messages = vec![source.clone(), legacy];
        assert_eq!(
            filtered_messages(&messages, "e", "p", None),
            vec![&messages[0]]
        );
        assert_eq!(
            filtered_messages(&messages, "e", "p", Some(MessageType::Chat)),
            vec![&messages[1]]
        );
        assert!(filtered_messages(&messages, "foreign", "p", None).is_empty());
        assert!(filtered_messages(&messages, "e", "foreign", None).is_empty());
        let acknowledged = corrected_message(&messages, &source, MessageType::Chat).unwrap();
        assert_eq!(
            messages[0], source,
            "preparing correction cannot publish it"
        );
        let messages = vec![acknowledged];
        assert!(filtered_messages(&messages, "e", "p", None).is_empty());
        assert_eq!(
            filtered_messages(&messages, "e", "p", Some(MessageType::Chat)),
            vec![&messages[0]]
        );
    }
    #[test]
    fn retained_metadata_round_trip_and_invalid_state_refusal() {
        let mut draft = TypedDraft::from_text("DEC: approved".into());
        assert!(draft.detect_token());
        let bytes = serde_json::to_vec(&draft.retained_metadata()).unwrap();
        let retained: DraftMetadata = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            TypedDraft::restore_metadata(draft.text.clone(), retained.clone()).unwrap(),
            draft
        );
        assert!(TypedDraft::restore_metadata("different text".into(), retained.clone()).is_err());
        assert!(TypedDraft::restore_metadata(
            draft.text.clone(),
            DraftMetadata {
                version: 2,
                ..retained
            }
        )
        .is_err());
        assert!(draft.undo_token());
        let restored =
            TypedDraft::restore_metadata(draft.text.clone(), draft.retained_metadata()).unwrap();
        assert_eq!(restored, draft);
        assert!(!draft.detect_token());
    }
    #[test]
    fn correction_checks_exact_snapshot_and_preserves_author_and_text() {
        use okilum_t3::message_types::{Message, MessageKey};
        let source = Message {
            key: MessageKey {
                environment: "e".into(),
                project: "p".into(),
                thread: "t".into(),
                message: "m".into(),
            },
            text: "Original manager question".into(),
            metadata: Some(Metadata {
                kind: MessageType::Status,
                provenance: Provenance::ManagerClassification,
                author: Author::Manager,
            }),
        };
        let corrected = corrected_message(
            std::slice::from_ref(&source),
            &source,
            MessageType::Decision,
        )
        .unwrap();
        assert_eq!(corrected.key, source.key);
        assert_eq!(corrected.text, source.text);
        assert_eq!(
            corrected.metadata.unwrap(),
            Metadata {
                kind: MessageType::Decision,
                provenance: Provenance::UserCorrection,
                author: Author::Manager
            }
        );
        assert!(corrected_message(&[corrected], &source, MessageType::Bug).is_err());
        assert!(
            corrected_message(&[source.clone(), source.clone()], &source, MessageType::Bug)
                .is_err()
        );
        assert!(corrected_message(&[], &source, MessageType::Bug).is_err());
        let legacy = Message {
            metadata: None,
            ..source
        };
        assert!(
            corrected_message(std::slice::from_ref(&legacy), &legacy, MessageType::Bug).is_err()
        );
    }
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

//! Local typed-message projection (#1478). These values are not T3 wire fields.
//! Callers retain metadata with its conversation identity outside the index;
//! absent metadata means ordinary chat, never inferred classification.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum MessageType {
    #[default]
    Chat,
    FeatureRequest,
    Bug,
    Decision,
    Status,
}
impl MessageType {
    pub const ALL: [Self; 5] = [
        Self::Chat,
        Self::FeatureRequest,
        Self::Bug,
        Self::Decision,
        Self::Status,
    ];

    /// Stable semantic icon key shared by composer, chips and panel tabs.
    pub fn icon(self) -> &'static str {
        match self {
            Self::Chat => "message-circle",
            Self::FeatureRequest => "lightbulb",
            Self::Bug => "bug",
            Self::Decision => "git-branch",
            Self::Status => "activity",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Chat => "Chat",
            Self::FeatureRequest => "FR",
            Self::Bug => "Bug",
            Self::Decision => "Decision",
            Self::Status => "Status",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Provenance {
    ExplicitToken,
    ExplicitSelection,
    ManagerClassification,
    UserCorrection,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Author {
    User,
    Manager,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Metadata {
    pub kind: MessageType,
    pub provenance: Provenance,
    pub author: Author,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum DecisionMeaning {
    RecordedDecision,
    AwaitingAnswer,
}
impl Metadata {
    pub fn decision_meaning(self) -> Option<DecisionMeaning> {
        (self.kind == MessageType::Decision).then_some(match self.author {
            Author::User => DecisionMeaning::RecordedDecision,
            Author::Manager => DecisionMeaning::AwaitingAnswer,
        })
    }
}

/// Exact source identity. A panel card links back to this same chat message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MessageKey {
    pub environment: String,
    pub project: String,
    pub thread: String,
    pub message: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    pub key: MessageKey,
    /// Full original text, including explicit prefixes. Never rewritten by projection.
    pub text: String,
    pub metadata: Option<Metadata>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PanelCard<'a> {
    pub source: &'a Message,
    pub metadata: Metadata,
}
impl PanelCard<'_> {
    pub fn chip(&self) -> String {
        format!(
            "{} → {}",
            self.metadata.kind.label(),
            self.source.key.message
        )
    }
}

/// Input order is retained. Chat remains available in the original feed; this
/// projection neither creates issues nor fabricates lifecycle/build evidence.
pub fn panel<'a>(messages: &'a [Message], environment: &str, project: &str) -> Vec<PanelCard<'a>> {
    messages
        .iter()
        .filter_map(|source| {
            let metadata = source.metadata?;
            (source.key.environment == environment
                && source.key.project == project
                && metadata.kind != MessageType::Chat)
                .then_some(PanelCard { source, metadata })
        })
        .collect()
}

/// Only an exact initial uppercase token followed by ASCII space or colon.
/// Returned byte length includes the delimiter, enabling lossless pill Undo.
pub fn initial_token(text: &str) -> Option<(MessageType, usize)> {
    [
        ("FR", MessageType::FeatureRequest),
        ("BUG", MessageType::Bug),
        ("DEC", MessageType::Decision),
    ]
    .into_iter()
    .find_map(|(token, kind)| {
        let tail = text.strip_prefix(token)?;
        matches!(tail.as_bytes().first(), Some(b' ' | b':')).then_some((kind, token.len() + 1))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn approved_token_boundaries() {
        for text in [
            "Friday",
            "FRIDAY",
            "Fr ",
            "fr:",
            "BUGfix",
            "BUG",
            "hello FR ",
            "баг ",
            "эф эр ",
            " FR ",
            "FR\n",
            "FR\t",
        ] {
            assert_eq!(initial_token(text), None, "{text:?}");
        }
        for (text, kind, bytes) in [
            ("FR ", MessageType::FeatureRequest, 3),
            ("FR:idea", MessageType::FeatureRequest, 3),
            ("BUG broken", MessageType::Bug, 4),
            ("DEC:yes", MessageType::Decision, 4),
        ] {
            assert_eq!(initial_token(text), Some((kind, bytes)));
        }
    }
    #[test]
    fn panel_retains_source_and_scopes_without_classifying_legacy_chat() {
        let message = |id: &str, environment: &str, metadata| Message {
            key: MessageKey {
                environment: environment.into(),
                project: "p".into(),
                thread: "t".into(),
                message: id.into(),
            },
            text: "FR: original body".into(),
            metadata,
        };
        let metadata = Metadata {
            kind: MessageType::Decision,
            provenance: Provenance::ExplicitSelection,
            author: Author::User,
        };
        let messages = vec![
            message("legacy", "e", None),
            message("decision", "e", Some(metadata)),
            message("foreign", "other", Some(metadata)),
        ];
        let before = messages.clone();
        let cards = panel(&messages, "e", "p");
        assert_eq!(cards.len(), 1);
        assert!(std::ptr::eq(cards[0].source, &messages[1]));
        assert_eq!(
            cards[0].metadata.decision_meaning(),
            Some(DecisionMeaning::RecordedDecision)
        );
        assert_eq!(
            Metadata {
                author: Author::Manager,
                ..metadata
            }
            .decision_meaning(),
            Some(DecisionMeaning::AwaitingAnswer)
        );
        assert_eq!(messages, before);
        assert!(panel(&messages, "e", "another-project").is_empty());
    }
}

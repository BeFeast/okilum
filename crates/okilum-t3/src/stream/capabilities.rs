//! Explicit native environment capabilities. Missing or malformed advertisements
//! remain unknown; they never inherit permission from ordinary message uploads.
use super::{AttachmentKind, NativeClient, StreamError, StreamResult, MAX_FILE_BYTES};
use crate::actions::{validate_question_attachments, QuestionAttachments};
use crate::ServerIdentity;
use serde_json::{json, Value};

#[derive(Clone, Debug, PartialEq)]
pub struct NativeEnvironmentProfile {
    pub identity: ServerIdentity,
    pub capabilities: NativeEnvironmentCapabilities,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NativeEnvironmentCapabilities {
    /// Only a JSON boolean is a known capability; absent or malformed is None.
    pub question_attachments: Option<bool>,
    pub attachment_uploads: Option<bool>,
    /// Positive integer advertised by fileAttachments.maxUploadBytes. This is
    /// the server value; use effective_file_max_upload_bytes before uploading.
    pub file_attachment_max_upload_bytes: Option<u64>,
}

impl NativeEnvironmentCapabilities {
    /// Environment support only. Request mode, question rules, client authority
    /// and uploaded-reference validation remain independent caller obligations.
    pub fn question_attachments_available(&self) -> bool {
        self.question_attachments == Some(true) && self.attachment_uploads == Some(true)
    }

    /// File uploads need both explicit upload support and a positive server
    /// limit. Never exceed the existing native attachment contract's bound.
    pub fn effective_file_max_upload_bytes(&self) -> Option<u64> {
        if self.attachment_uploads != Some(true) {
            return None;
        }
        self.file_attachment_max_upload_bytes
            .filter(|bytes| *bytes > 0)
            .map(|bytes| bytes.min(MAX_FILE_BYTES))
    }

    /// The caller supplies an explicitly read profile. This performs no network
    /// access and never infers question support from ordinary upload support.
    pub fn validate_question_attachments(
        &self,
        attachments: &QuestionAttachments,
    ) -> StreamResult<()> {
        validate_question_attachments(attachments)?;
        if attachments.is_empty() {
            return Ok(());
        }
        if !self.question_attachments_available() {
            return Err(StreamError::Protocol(
                "native environment does not advertise question attachments",
            ));
        }
        for attachment in attachments.values().flatten() {
            if attachment.metadata.kind == AttachmentKind::File
                && !self
                    .effective_file_max_upload_bytes()
                    .is_some_and(|limit| attachment.metadata.size_bytes <= limit)
            {
                return Err(StreamError::Protocol(
                    "question file exceeds the advertised upload limit",
                ));
            }
        }
        Ok(())
    }
}

impl NativeClient {
    /// One explicit native server.getConfig read over the existing origin-bound
    /// transport. Does not cache capabilities, upload assets, or change scopes.
    pub fn environment_profile(&self) -> StreamResult<NativeEnvironmentProfile> {
        profile_from_config(&self.manager_rpc("server.getConfig", json!({}), false)?)
    }
}

fn profile_from_config(config: &Value) -> StreamResult<NativeEnvironmentProfile> {
    let environment = &config["environment"];
    let identity: ServerIdentity = serde_json::from_value(environment.clone())
        .map_err(|_| StreamError::Protocol("missing native environment identity"))?;
    if identity.environment_id.trim().is_empty() || identity.label.trim().is_empty() {
        return Err(StreamError::Protocol("invalid native environment identity"));
    }
    let advertised = &environment["capabilities"];
    Ok(NativeEnvironmentProfile {
        identity,
        capabilities: NativeEnvironmentCapabilities {
            question_attachments: advertised["questionAttachments"].as_bool(),
            attachment_uploads: advertised["attachmentUploads"].as_bool(),
            file_attachment_max_upload_bytes: advertised["fileAttachments"]["maxUploadBytes"]
                .as_u64()
                .filter(|bytes| *bytes > 0),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stream::{AttachmentMetadata, ChatAttachment};

    fn config(capabilities: Value) -> Value {
        json!({"environment": {
            "environmentId":"actual-server", "label":"Remote server", "capabilities":capabilities
        }})
    }

    fn parse(capabilities: Value) -> NativeEnvironmentCapabilities {
        profile_from_config(&config(capabilities))
            .unwrap()
            .capabilities
    }

    #[test]
    fn explicit_true_flags_authorize_environment_support_with_actual_identity() {
        let profile = profile_from_config(&config(json!({
            "questionAttachments":true, "attachmentUploads":true,
            "fileAttachments":{"maxUploadBytes":MAX_FILE_BYTES}
        })))
        .unwrap();
        assert_eq!(profile.identity.environment_id, "actual-server");
        assert_eq!(profile.identity.label, "Remote server");
        assert_eq!(profile.capabilities.question_attachments, Some(true));
        assert_eq!(profile.capabilities.attachment_uploads, Some(true));
        assert!(profile.capabilities.question_attachments_available());
        assert_eq!(
            profile.capabilities.effective_file_max_upload_bytes(),
            Some(MAX_FILE_BYTES)
        );
    }

    #[test]
    fn each_explicit_false_independently_blocks_question_attachment_support() {
        for (question, uploads) in [(false, true), (true, false), (false, false)] {
            let capabilities =
                parse(json!({"questionAttachments":question,"attachmentUploads":uploads}));
            assert_eq!(capabilities.question_attachments, Some(question));
            assert_eq!(capabilities.attachment_uploads, Some(uploads));
            assert!(!capabilities.question_attachments_available());
        }
    }

    #[test]
    fn absent_or_malformed_flags_remain_unknown_even_when_other_flag_is_true() {
        for malformed in [Value::Null, json!(1), json!("true"), json!([]), json!({})] {
            for name in ["questionAttachments", "attachmentUploads"] {
                let mut advertised = json!({"questionAttachments":true,"attachmentUploads":true});
                advertised[name] = malformed.clone();
                let capabilities = parse(advertised);
                assert!(!capabilities.question_attachments_available());
                assert_eq!(
                    if name == "questionAttachments" {
                        capabilities.question_attachments
                    } else {
                        capabilities.attachment_uploads
                    },
                    None
                );
            }
        }
        for advertised in [
            json!({}),
            json!({"questionAttachments":true}),
            json!({"attachmentUploads":true}),
        ] {
            assert!(!parse(advertised).question_attachments_available());
        }
        let mut missing = config(Value::Null);
        missing["environment"]
            .as_object_mut()
            .unwrap()
            .remove("capabilities");
        assert_eq!(
            profile_from_config(&missing).unwrap().capabilities,
            NativeEnvironmentCapabilities::default()
        );
    }

    #[test]
    fn malformed_capability_containers_never_authorize_uploads() {
        for advertised in [
            Value::Null,
            json!(true),
            json!(7),
            json!("supported"),
            json!([]),
        ] {
            let capabilities = parse(advertised);
            assert_eq!(capabilities, NativeEnvironmentCapabilities::default());
            assert!(!capabilities.question_attachments_available());
            assert_eq!(capabilities.effective_file_max_upload_bytes(), None);
        }
    }

    #[test]
    fn identity_preserves_existing_server_identity_parsing_without_alias_fallback() {
        let positive = config(json!({"questionAttachments":true,"attachmentUploads":true}));
        assert!(profile_from_config(&positive)
            .unwrap()
            .capabilities
            .question_attachments_available());
        for field in ["environmentId", "label"] {
            for invalid in [Value::Null, json!(1), json!(""), json!(" \n")] {
                let mut value = positive.clone();
                value["environment"][field] = invalid;
                assert!(matches!(
                    profile_from_config(&value),
                    Err(StreamError::Protocol(_))
                ));
            }
            let mut missing = positive.clone();
            missing["environment"]
                .as_object_mut()
                .unwrap()
                .remove(field);
            assert!(matches!(
                profile_from_config(&missing),
                Err(StreamError::Protocol(_))
            ));
            for accepted in [" padded", "padded ", "embedded\ncontrol"] {
                let mut value = positive.clone();
                value["environment"][field] = json!(accepted);
                let identity = profile_from_config(&value).unwrap().identity;
                assert_eq!(
                    if field == "label" {
                        identity.label
                    } else {
                        identity.environment_id
                    },
                    accepted,
                    "profile parsing must preserve server_identity semantics verbatim"
                );
            }
        }
        for invalid in [
            json!({}),
            json!({"environment":null}),
            json!({"environment":[]}),
        ] {
            assert!(matches!(
                profile_from_config(&invalid),
                Err(StreamError::Protocol(_))
            ));
        }
    }

    #[test]
    fn file_limit_requires_positive_integer_and_never_exceeds_native_contract() {
        for (advertised, effective) in [
            (1, 1),
            (MAX_FILE_BYTES / 2, MAX_FILE_BYTES / 2),
            (MAX_FILE_BYTES, MAX_FILE_BYTES),
            (MAX_FILE_BYTES + 1, MAX_FILE_BYTES),
            (u64::MAX, MAX_FILE_BYTES),
        ] {
            let capabilities = parse(
                json!({"attachmentUploads":true,"fileAttachments":{"maxUploadBytes":advertised}}),
            );
            assert_eq!(
                capabilities.file_attachment_max_upload_bytes,
                Some(advertised)
            );
            assert_eq!(
                capabilities.effective_file_max_upload_bytes(),
                Some(effective)
            );
            assert!(
                !capabilities.question_attachments_available(),
                "ordinary file support cannot imply question support"
            );
        }
        for invalid in [
            Value::Null,
            json!(0),
            json!(-1),
            json!(0.5),
            json!(1024.0),
            json!("1024"),
            json!(true),
            serde_json::from_str("18446744073709551616").unwrap(),
        ] {
            let capabilities = parse(
                json!({"attachmentUploads":true,"fileAttachments":{"maxUploadBytes":invalid}}),
            );
            assert_eq!(capabilities.file_attachment_max_upload_bytes, None);
            assert_eq!(capabilities.effective_file_max_upload_bytes(), None);
        }
    }

    #[test]
    fn missing_file_limit_or_upload_permission_does_not_authorize_file_uploads() {
        for advertised in [
            json!({"questionAttachments":true,"attachmentUploads":true}),
            json!({"attachmentUploads":true,"fileAttachments":null}),
            json!({"attachmentUploads":true,"fileAttachments":true}),
            json!({"attachmentUploads":true,"fileAttachments":{}}),
        ] {
            assert_eq!(parse(advertised).effective_file_max_upload_bytes(), None);
        }
        for upload in [Value::Null, json!(false), json!("true")] {
            let capabilities = parse(
                json!({"attachmentUploads":upload,"fileAttachments":{"maxUploadBytes":MAX_FILE_BYTES}}),
            );
            assert_eq!(
                capabilities.file_attachment_max_upload_bytes,
                Some(MAX_FILE_BYTES)
            );
            assert_eq!(capabilities.effective_file_max_upload_bytes(), None);
        }
        let unknown_upload = parse(json!({"fileAttachments":{"maxUploadBytes":MAX_FILE_BYTES}}));
        assert_eq!(unknown_upload.effective_file_max_upload_bytes(), None);
    }

    #[test]
    fn question_references_require_both_true_flags_and_the_explicit_file_limit() {
        let file = ChatAttachment {
            id: "issued-file".into(),
            metadata: AttachmentMetadata {
                kind: AttachmentKind::File,
                name: "report.txt".into(),
                mime_type: "text/plain".into(),
                size_bytes: 19,
            },
            source: None,
        };
        let references = QuestionAttachments::from([("q".into(), vec![file.clone()])]);
        let positive = parse(json!({"questionAttachments":true,"attachmentUploads":true,
            "fileAttachments":{"maxUploadBytes":19}}));
        assert!(positive.validate_question_attachments(&references).is_ok());
        for unavailable in [
            json!({}),
            json!({"questionAttachments":false,"attachmentUploads":true,"fileAttachments":{"maxUploadBytes":19}}),
            json!({"questionAttachments":true,"attachmentUploads":false,"fileAttachments":{"maxUploadBytes":19}}),
            json!({"questionAttachments":true,"attachmentUploads":true}),
            json!({"questionAttachments":true,"attachmentUploads":true,"fileAttachments":{"maxUploadBytes":18}}),
        ] {
            assert!(parse(unavailable)
                .validate_question_attachments(&references)
                .is_err());
        }
        let mut image = file;
        image.metadata.kind = AttachmentKind::Image;
        image.metadata.mime_type = "image/png".into();
        let images = QuestionAttachments::from([("q".into(), vec![image])]);
        assert!(
            parse(json!({"questionAttachments":true,"attachmentUploads":true}))
                .validate_question_attachments(&images)
                .is_ok(),
            "image bounds come from the existing image contract"
        );
    }
}

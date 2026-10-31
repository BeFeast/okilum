//! Native T3 attachments: RPC mints a short-lived same-origin URL; HTTP carries
//! the bytes. Only confirmed asset references belong in a durable message.
use super::*;
use serde::{Deserialize, Serialize};
use std::io::Read;

// Deployed T3 chatAttachment contract (0.1.7-nightly.20261031.3991).
pub const MAX_IMAGE_BYTES: u64 = 10 * 1024 * 1024;
pub const MAX_TOTAL_IMAGE_BYTES: u64 = 80 * 1024 * 1024;
pub const MAX_FILE_BYTES: u64 = 50 * 1024 * 1024;
const TRANSFER_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AttachmentKind {
    Image,
    File,
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;

    #[test]
    #[ignore = "explicit isolated real-T3 asset transfer; no thread commands"]
    fn live_isolated_attachment_transfer_without_thread_commands() {
        let origin = std::env::var("OKILUM_T3_QA_ORIGIN").expect("authorized server origin");
        let credential =
            std::env::var("OKILUM_T3_QA_CREDENTIAL_FILE").expect("private native credential file");
        let environment = Environment::new("qa2122-assets", &origin).unwrap();
        let token =
            NativeToken::from_secret(&environment, &std::fs::read(credential).unwrap()).unwrap();
        let client = NativeClient::new(environment, token, Duration::from_secs(10)).unwrap();
        let image=base64::engine::general_purpose::STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=").unwrap();
        for (kind, name, mime_type, bytes) in [
            (
                AttachmentKind::Image,
                "okilum2122-qa.png",
                "image/png",
                image,
            ),
            (
                AttachmentKind::File,
                "okilum2122-qa.txt",
                "text/plain",
                b"Okilum #2122 isolated asset transport verification.\n".to_vec(),
            ),
        ] {
            let metadata = AttachmentMetadata {
                kind,
                name: name.into(),
                mime_type: mime_type.into(),
                size_bytes: bytes.len() as u64,
            };
            let attachment = client.upload_attachment(&metadata, bytes.clone()).unwrap();
            let downloaded = client.download_attachment(&attachment);
            let cleanup = client.delete_attachment(&attachment.id);
            assert!(cleanup.is_ok(), "temporary QA asset cleanup must succeed");
            assert_eq!(downloaded.unwrap(), bytes);
            println!("ASSETS2122 kind={kind:?} uploaded=PASS downloaded=PASS bytes={} cleanup=PASS thread-commands=0",metadata.size_bytes);
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentMetadata {
    #[serde(rename = "type")]
    pub kind: AttachmentKind,
    pub name: String,
    pub mime_type: String,
    pub size_bytes: u64,
}

/// Wire-compatible ChatImageAttachment / ChatFileAttachment. No signed URL,
/// token or local path is part of this durable reference.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChatAttachment {
    pub id: String,
    #[serde(flatten)]
    pub metadata: AttachmentMetadata,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<Value>,
}

impl AttachmentMetadata {
    pub fn validate(&self) -> StreamResult<()> {
        if self.name.trim().is_empty() || self.name.chars().count() > 255 {
            return Err(StreamError::Protocol(
                "attachment name is missing or too long",
            ));
        }
        if self.mime_type.trim().is_empty() || self.mime_type.chars().count() > 100 {
            return Err(StreamError::Protocol(
                "attachment MIME type is missing or too long",
            ));
        }
        let limit = match self.kind {
            AttachmentKind::Image => {
                if !self
                    .mime_type
                    .get(..6)
                    .is_some_and(|prefix| prefix.eq_ignore_ascii_case("image/"))
                {
                    return Err(StreamError::Protocol("invalid image MIME type"));
                }
                MAX_IMAGE_BYTES
            }
            AttachmentKind::File => MAX_FILE_BYTES,
        };
        if self.size_bytes == 0 || self.size_bytes > limit {
            return Err(StreamError::Protocol(
                "attachment is empty or exceeds the T3 byte limit",
            ));
        }
        Ok(())
    }
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

impl ChatAttachment {
    pub fn validate(&self) -> StreamResult<()> {
        self.metadata.validate()?;
        if !valid_id(&self.id) {
            return Err(StreamError::Protocol("invalid T3 attachment identity"));
        }
        Ok(())
    }
}

pub fn validate_attachments(attachments: &[ChatAttachment]) -> StreamResult<()> {
    let mut image_bytes = 0_u64;
    for attachment in attachments {
        attachment.validate()?;
        if attachment.metadata.kind == AttachmentKind::Image {
            image_bytes = image_bytes
                .checked_add(attachment.metadata.size_bytes)
                .ok_or(StreamError::Protocol("attachment byte total overflow"))?;
        }
    }
    if image_bytes > MAX_TOTAL_IMAGE_BYTES {
        return Err(StreamError::Protocol(
            "images exceed the T3 total byte limit",
        ));
    }
    Ok(())
}

impl NativeClient {
    fn attachment_url(&self, value: &Value) -> StreamResult<url::Url> {
        let relative = value["relativeUrl"]
            .as_str()
            .filter(|s| s.starts_with('/') && !s.starts_with("//") && s.len() <= 4096)
            .ok_or(StreamError::Protocol("invalid attachment transfer URL"))?;
        let origin =
            url::Url::parse(&self.environment.origin()).map_err(|_| StreamError::WrongOrigin)?;
        let url = origin
            .join(relative)
            .map_err(|_| StreamError::Protocol("invalid attachment transfer URL"))?;
        if url.origin() != origin.origin()
            || url.fragment().is_some()
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err(StreamError::WrongOrigin);
        }
        if !value["expiresAt"]
            .as_f64()
            .is_some_and(|expires| expires > crate::unix_now() as f64 * 1000.)
        {
            return Err(StreamError::Protocol("attachment transfer URL expired"));
        }
        Ok(url)
    }

    /// One upload attempt, off the UI thread. No message is dispatched here.
    /// The caller retains its draft bytes until a confirmed reference returns.
    pub fn upload_attachment(
        &self,
        metadata: &AttachmentMetadata,
        bytes: Vec<u8>,
    ) -> StreamResult<ChatAttachment> {
        if !self.access.allowed() {
            return Err(StreamError::Closed);
        }
        self.token.validate(&self.environment, crate::unix_now())?;
        if !self.can_operate() {
            return Err(StreamError::Rejected);
        }
        metadata.validate()?;
        if metadata.kind == AttachmentKind::Image
            && !matches!(
                metadata.mime_type.as_str(),
                "image/png" | "image/jpeg" | "image/gif" | "image/webp"
            )
        {
            return Err(StreamError::Protocol(
                "T3 does not support this image upload type",
            ));
        }
        if bytes.len() as u64 != metadata.size_bytes {
            return Err(StreamError::Protocol(
                "attachment bytes changed before upload",
            ));
        }
        let issued = self.manager_rpc(
            "attachments.createUploadUrl",
            serde_json::to_value(metadata)
                .map_err(|_| StreamError::Protocol("invalid attachment metadata"))?,
            false,
        )?;
        let id = issued["attachmentId"]
            .as_str()
            .filter(|id| valid_id(id))
            .ok_or(StreamError::Protocol("invalid issued attachment identity"))?;
        let url = self.attachment_url(&issued)?;
        if !self.access.allowed() {
            return Err(StreamError::Closed);
        }
        let response = self
            .http
            .post(url)
            .header("Content-Type", &metadata.mime_type)
            .timeout(self.timeout.max(TRANSFER_TIMEOUT))
            .body(bytes)
            .send()
            .map_err(|_| {
                // No caller can have journaled this reference yet. A lost
                // HTTP ACK must not leave an uploaded, unreachable draft blob.
                let _ = self.delete_attachment(id);
                StreamError::Unreachable
            })?;
        let status = response.status().as_u16();
        if status != 204 {
            let _ = self.delete_attachment(id);
            return Err(StreamError::Http(status));
        }
        if !self.access.allowed() {
            return Err(StreamError::Closed);
        }
        Ok(ChatAttachment {
            id: id.to_owned(),
            metadata: metadata.clone(),
            source: None,
        })
    }

    /// Download bytes through a fresh same-origin signed URL. The bearer is
    /// used only on the RPC socket, never sent to the signed HTTP URL.
    pub fn download_attachment(&self, attachment: &ChatAttachment) -> StreamResult<Vec<u8>> {
        if !self.access.allowed() {
            return Err(StreamError::Closed);
        }
        attachment.validate()?;
        let issued = self.manager_rpc(
            "assets.createUrl",
            json!({"resource":{
                "_tag":"attachment", "attachmentId":attachment.id,
                "fileName":attachment.metadata.name, "mimeType":attachment.metadata.mime_type
            }}),
            false,
        )?;
        let url = self.attachment_url(&issued)?;
        if !self.access.allowed() {
            return Err(StreamError::Closed);
        }
        let response = self
            .http
            .get(url)
            .timeout(self.timeout.max(TRANSFER_TIMEOUT))
            .send()
            .map_err(|_| StreamError::Unreachable)?;
        let status = response.status().as_u16();
        if status != 200 {
            return Err(StreamError::Http(status));
        }
        if response
            .content_length()
            .is_some_and(|size| size > attachment.metadata.size_bytes)
        {
            return Err(StreamError::Protocol(
                "attachment download exceeds its declared size",
            ));
        }
        let mut bytes = Vec::new();
        response
            .take(attachment.metadata.size_bytes + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| StreamError::Unreachable)?;
        if bytes.len() as u64 != attachment.metadata.size_bytes {
            return Err(StreamError::Protocol(
                "attachment download size does not match",
            ));
        }
        if !self.access.allowed() {
            return Err(StreamError::Closed);
        }
        Ok(bytes)
    }

    /// Explicit cleanup for an unsent draft asset. Never delete references
    /// already handed to the durable command journal, even after a lost ACK.
    pub fn delete_attachment(&self, id: &str) -> StreamResult<()> {
        if !self.access.allowed() {
            return Err(StreamError::Closed);
        }
        if !self.can_operate() {
            return Err(StreamError::Rejected);
        }
        if !valid_id(id) {
            return Err(StreamError::Protocol("invalid T3 attachment identity"));
        }
        self.manager_rpc("attachments.delete", json!({"attachmentId":id}), false)?;
        Ok(())
    }
}

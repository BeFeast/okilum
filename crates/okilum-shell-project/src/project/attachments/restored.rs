//! Restore composer previews from durable server references, without uploading
//! again or changing send readiness. Local, unfinished sources still reselect.
use super::*;
use std::sync::Weak;

type Images = (Arc<gpui::Image>, Arc<gpui::Image>);

#[cfg(test)]
type FetchFixture = Arc<dyn Fn(&ChatAttachment) -> anyhow::Result<Vec<u8>> + Send + Sync>;

#[derive(Default)]
pub(super) struct Previews {
    rows: HashMap<String, Entry>,
    inflight: Option<uuid::Uuid>,
    #[cfg(test)]
    fetch: Option<FetchFixture>,
}

struct Entry {
    row: Attachment,
    scope: Scope,
    client: Weak<NativeClient>,
    state: State,
}

enum State {
    Loading(uuid::Uuid),
    Ready(Images),
    Failed,
}

impl Previews {
    pub(super) fn remove(&mut self, id: &str) {
        self.rows.remove(id);
    }
}

impl ProjectView {
    fn restored_preview(&self, row: &Attachment) -> Option<&State> {
        let scope = self.attachment_scope()?;
        let client = self.native.model_client()?;
        let AttachmentState::Uploaded { environment_id, .. } = &row.state else {
            return None;
        };
        if !client.access_allowed() || self.attachment_identity()?.environment_id != *environment_id
        {
            return None;
        }
        let entry = self.attachments.restored.rows.get(&row.id)?;
        (entry.row == *row
            && entry.scope == scope
            && entry
                .client
                .upgrade()
                .is_some_and(|known| Arc::ptr_eq(&known, &client)))
        .then_some(&entry.state)
    }

    pub(super) fn attachment_preview(&self, row: &Attachment) -> Option<Images> {
        if self.has_attachment_source(&row.id) {
            return Some((
                self.attachments.previews.get(&row.id)?.clone(),
                self.attachments.expanded_previews.get(&row.id)?.clone(),
            ));
        }
        match self.restored_preview(row)? {
            State::Ready(images) => Some(images.clone()),
            _ => None,
        }
    }

    pub(super) fn render_restored_attachment_status(
        &self,
        row: &Attachment,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match self.restored_preview(row) {
            Some(State::Loading(_)) => div().text_sm().child("Loading preview…").into_any_element(),
            Some(State::Failed) => {
                let id = row.id.clone();
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_sm()
                            .whitespace_normal()
                            .child("Preview unavailable. The attachment is still attached."),
                    )
                    .child(
                        Button::new(SharedString::from(format!("attachment-preview-retry-{id}")))
                            .debug_selector({
                                let id = id.clone();
                                move || format!("attachment-preview-retry-{id}")
                            })
                            .ghost()
                            .small()
                            .label("Retry preview")
                            .disabled(self.busy)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.attachments.restored.remove(&id);
                                this.restore_attachment_previews(window, cx);
                                cx.notify();
                            })),
                    )
                    .into_any_element()
            }
            _ => div().into_any_element(),
        }
    }

    pub(super) fn restore_attachment_previews(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.active
            || !self.draft_ready()
            || !reader_features::enabled(reader_features::Feature::Agents, cx)
        {
            return;
        }
        let Some(scope) = self.attachment_scope() else {
            return;
        };
        let Some(client) = self
            .native
            .model_client()
            .filter(|client| client.access_allowed())
        else {
            return;
        };
        let Some(identity) = self.attachment_identity() else {
            return;
        };
        let environment_id = identity.environment_id.clone();
        let rows = self.draft_attachments().to_vec();
        self.attachments.restored.rows.retain(|id, entry| {
            rows.iter().any(|row| row.id == *id && *row == entry.row)
                && entry.scope == scope
                && entry
                    .client
                    .upgrade()
                    .is_some_and(|known| Arc::ptr_eq(&known, &client))
        });
        if self.attachments.restored.inflight.is_some() {
            return;
        }
        let next = rows.into_iter().find_map(|row| {
            let AttachmentState::Uploaded {
                attachment_id,
                environment_id: uploaded,
            } = &row.state
            else {
                return None;
            };
            if row.kind != DraftKind::Image
                || uploaded != &environment_id
                || self.has_attachment_source(&row.id)
                || self.restored_preview(&row).is_some()
            {
                return None;
            }
            let reference = ChatAttachment {
                id: attachment_id.clone(),
                metadata: AttachmentMetadata {
                    kind: AttachmentKind::Image,
                    name: row.name.clone(),
                    mime_type: row.mime_type.clone(),
                    size_bytes: row.size_bytes,
                },
                source: row.source.clone(),
            };
            reference.validate().ok()?;
            Some((row, reference))
        });
        let Some((row, reference)) = next else {
            return;
        };
        let job = uuid::Uuid::new_v4();
        self.attachments.restored.inflight = Some(job);
        self.attachments.restored.rows.insert(
            row.id.clone(),
            Entry {
                row: row.clone(),
                scope: scope.clone(),
                client: Arc::downgrade(&client),
                state: State::Loading(job),
            },
        );
        #[cfg(test)]
        let fetch = self.attachments.restored.fetch.clone();
        cx.spawn_in(window, async move |this, cx| {
            let reader = client.clone();
            let result = cx.background_executor().spawn(async move {
                #[cfg(test)]
                let bytes = match fetch { Some(fetch) => fetch(&reference)?, None => reader.download_attachment(&reference)? };
                #[cfg(not(test))]
                let bytes = reader.download_attachment(&reference)?;
                image_previews(&bytes).context("Cannot decode the attachment preview")
            }).await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.attachments.restored.inflight != Some(job) { return; }
                this.attachments.restored.inflight = None;
                let current = this.active
                    && this.draft_ready()
                    && this.attachment_scope().as_ref() == Some(&scope)
                    && this.native.model_client().is_some_and(|current| Arc::ptr_eq(&current, &client))
                    && client.access_allowed()
                    && this.draft_attachments().iter().any(|current| current == &row)
                    && matches!(this.attachments.restored.rows.get(&row.id).map(|entry| &entry.state), Some(State::Loading(current)) if *current == job);
                if current {
                    let entry = this.attachments.restored.rows.get_mut(&row.id).unwrap();
                    entry.state = match result { Ok(images) => State::Ready(images), Err(_) => State::Failed };
                } else if matches!(this.attachments.restored.rows.get(&row.id).map(|entry| &entry.state), Some(State::Loading(current)) if *current == job) {
                    this.attachments.restored.remove(&row.id);
                }
                this.restore_attachment_previews(window, cx);
                cx.notify();
            });
        }).detach();
    }
}

#[cfg(test)]
mod tests;

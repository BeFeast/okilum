//! Parent integration for the delivered typed composer. Local annotations use
//! the existing identity/lock/revision store; no metadata enters a T3 request.
use super::*;
use crate::project::*;
use composer::{RetypeRequested, TypedScope};
use okilum_t3::{
    chat_drafts::{Draft, Store},
    message_types::{Message, MessageKey},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Annotation {
    message: String,
    source: String,
    metadata: Metadata,
}
type Annotations = Store<Annotation>;

fn fingerprint(text: &str, author: Author) -> String {
    format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&(text, author)).expect("source tuple serializes"))
    )
}
fn read(store: &Annotations) -> Result<(u64, Vec<Annotation>), String> {
    let saved = store.load().map_err(|error| error.to_string())?;
    let mut ids = HashSet::new();
    if !saved.draft.text.is_empty()
        || saved.draft.attachments.iter().any(|entry| {
            entry.message.is_empty() || entry.source.len() != 64 || !ids.insert(&entry.message)
        })
    {
        return Err("Retained message types are malformed; nothing was overwritten".into());
    }
    Ok((saved.revision, saved.draft.attachments))
}
fn write(
    store: &Annotations,
    revision: u64,
    expected: Option<&Annotation>,
    replacement: Annotation,
) -> Result<(u64, Vec<Annotation>), String> {
    let (actual, mut entries) = read(store)?;
    if actual != revision
        || entries
            .iter()
            .find(|entry| entry.message == replacement.message)
            != expected
    {
        return Err("Message types changed in another writer; review before retrying".into());
    }
    entries.retain(|entry| entry.message != replacement.message);
    entries.push(replacement);
    let saved = store
        .save(
            revision,
            Draft {
                text: String::new(),
                attachments: entries,
            },
        )
        .map_err(|error| error.to_string())?;
    Ok((saved.revision, saved.draft.attachments))
}

#[derive(Clone)]
pub(in crate::project) struct SendType {
    store: Annotations,
    text: String,
    metadata: Metadata,
}
impl SendType {
    pub(in crate::project) fn prepare_mcp(
        &self,
        journal: &okilum_t3::chat::MessageJournal,
        message: okilum_t3::OutgoingMessage,
    ) -> anyhow::Result<okilum_t3::chat::MessageRecord> {
        anyhow::ensure!(
            message.text == self.text,
            "Typed source changed; nothing was sent"
        );
        self.retain(&message.client_request_id)
            .map_err(anyhow::Error::msg)?;
        journal.prepare_message(message)
    }
    /// Called before dispatch using an existing immutable outgoing message ID.
    /// Idempotent retries cannot replace a different source or selected type.
    pub(in crate::project) fn retain(&self, message: &str) -> Result<(), String> {
        let replacement = Annotation {
            message: message.into(),
            source: fingerprint(&self.text, self.metadata.author),
            metadata: self.metadata,
        };
        let (revision, entries) = read(&self.store)?;
        if let Some(old) = entries.iter().find(|entry| entry.message == message) {
            return if old == &replacement {
                Ok(())
            } else {
                Err("Retained message type has a different source; nothing was sent".into())
            };
        }
        write(&self.store, revision, None, replacement).map(|_| ())
    }
}

#[derive(Default)]
pub(in crate::project) struct State {
    owner: Option<TypedScope>,
    connection_epoch: u64,
    epoch: u64,
    source_revision: Option<u64>,
    sources: Vec<Source>,
    messages: Vec<Message>,
    revision: u64,
    annotations: Vec<Annotation>,
    store: Option<Annotations>,
    loaded: bool,
    writing: bool,
    clear_input: bool,
    load: Option<Task<()>>,
}

impl ProjectView {
    pub(in crate::project) fn typed_annotation_writing(&self) -> bool {
        self.typed.writing
    }
    pub(in crate::project) fn typed_reload_annotations(&mut self, cx: &mut Context<Self>) {
        self.typed.owner = None;
        self.typed_sync(cx);
    }
    pub(in crate::project) fn typed_scope(&self) -> Option<TypedScope> {
        if !self.active
            || self.typed.connection_epoch != self.native.connection_epoch
            || self
                .native
                .model_client()
                .is_some_and(|client| !client.access_allowed())
        {
            return None;
        }
        let scope = self.typed.owner.as_ref()?;
        if !self.draft_typed_owner_valid(scope) {
            return None;
        }
        let binding = self.binding.as_ref()?;
        (self.environment.as_ref()?.origin() == scope.environment
            && binding.manager.project_id.as_deref() == Some(scope.project.as_str())
            && binding.manager.thread_id.as_deref() == Some(scope.thread.as_str())
            && self.chat.as_ref()?.thread_id == scope.thread)
            .then(|| scope.clone())
    }

    pub(in crate::project) fn typed_invalidate(
        &mut self,
        clear_input: bool,
        cx: &mut Context<Self>,
    ) {
        self.typed.clear_input |= clear_input && self.composer.read(cx).typed_scope().is_some();
        self.typed.epoch = self.typed.epoch.wrapping_add(1);
        self.typed.owner = None;
        self.typed.source_revision = None;
        self.typed.store = None;
        self.typed.load = None;
        self.typed.loaded = false;
        self.typed.writing = false;
        self.typed.sources.clear();
        self.typed.messages.clear();
        self.typed.annotations.clear();
        self.composer
            .update(cx, |composer, cx| composer.clear_typed_context(cx));
    }
    pub(in crate::project) fn typed_clear_pending(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if std::mem::take(&mut self.typed.clear_input) {
            self.compose
                .update(cx, |input, cx| input.set_value("", window, cx));
        }
    }

    pub(in crate::project) fn typed_send_type(
        &self,
        text: &str,
        cx: &App,
    ) -> Result<Option<SendType>, String> {
        let Some(metadata) = self.composer.read(cx).typed_metadata() else {
            return Ok(None);
        };
        let scope = self
            .typed_scope()
            .ok_or("Message type owner changed; nothing was sent")?;
        let environment = self
            .environment
            .as_ref()
            .ok_or("No message type environment")?;
        let state = self
            .state
            .as_ref()
            .ok_or("Local message type storage is unavailable; nothing was sent")?;
        let store = Store::new(
            &state.join("t3-message-types"),
            environment,
            &scope.project,
            &scope.thread,
        );
        Ok(Some(SendType {
            store,
            text: text.into(),
            metadata,
        }))
    }

    pub(in crate::project) fn typed_sync(&mut self, cx: &mut Context<Self>) {
        if self.typed.owner.is_some() && self.typed.connection_epoch != self.native.connection_epoch
        {
            self.draft_suspend_typed(cx);
            self.typed_invalidate(false, cx);
            return;
        }
        let scope = self
            .composer
            .read(cx)
            .typed_scope()
            .cloned()
            .filter(|scope| {
                self.active
                    && reader_features::enabled(reader_features::Feature::Agents, cx)
                    && self
                        .environment
                        .as_ref()
                        .is_some_and(|environment| environment.origin() == scope.environment)
                    && self
                        .chat
                        .as_ref()
                        .is_some_and(|chat| chat.thread_id == scope.thread)
                    && self.binding.as_ref().is_some_and(|binding| {
                        binding.manager.project_id.as_deref() == Some(&scope.project)
                            && binding.manager.thread_id.as_deref() == Some(&scope.thread)
                    })
                    && self
                        .native
                        .model_client()
                        .is_none_or(|client| client.access_allowed())
            });
        let Some(scope) = scope else {
            if self.typed.owner.is_some() {
                self.draft_suspend_typed(cx);
                self.typed_invalidate(false, cx);
            }
            return;
        };
        if self.typed.owner.as_ref() != Some(&scope) {
            self.typed.epoch = self.typed.epoch.wrapping_add(1);
            self.typed.owner = Some(scope.clone());
            self.typed.connection_epoch = self.native.connection_epoch;
            self.typed.loaded = false;
            self.typed.writing = false;
            self.typed.source_revision = None;
            self.typed.annotations.clear();
            let Some((state, environment)) = self.state.as_ref().zip(self.environment.as_ref())
            else {
                self.composer.update(cx, |composer, cx| {
                    composer.typed_write_failed(
                        &scope,
                        "Message type storage is unavailable".into(),
                        cx,
                    )
                });
                return;
            };
            let store = Store::new(
                &state.join("t3-message-types"),
                environment,
                &scope.project,
                &scope.thread,
            );
            self.typed.store = Some(store.clone());
            let epoch = self.typed.epoch;
            let scope = scope.clone();
            self.typed.load = Some(cx.spawn(async move |this, cx| {
                let result = cx
                    .background_executor()
                    .spawn(async move { read(&store) })
                    .await;
                let _ = this.update(cx, |this, cx| {
                    if this.typed.epoch != epoch || this.typed_scope().as_ref() != Some(&scope) {
                        return;
                    }
                    match result {
                        Ok((revision, annotations)) => {
                            this.typed.revision = revision;
                            this.typed.annotations = annotations;
                            this.typed.loaded = true;
                            this.typed.source_revision = None;
                            this.typed_sync(cx);
                        }
                        Err(error) => this.composer.update(cx, |composer, cx| {
                            composer.typed_write_failed(&scope, error, cx)
                        }),
                    }
                    cx.notify();
                });
            }));
        }
        let revision = self.feed.read(cx).typed_revision();
        if self.typed.source_revision == Some(revision) {
            return;
        }
        let sources = self.feed.update(cx, |feed, _| feed.typed_snapshot(&scope));
        self.typed.source_revision = Some(revision);
        self.typed.sources = sources.as_ref().clone();
        let mut authors = HashMap::new();
        let messages = sources
            .iter()
            .map(|source| {
                let key = MessageKey {
                    environment: scope.environment.clone(),
                    project: scope.project.clone(),
                    thread: scope.thread.clone(),
                    message: source.id.clone(),
                };
                authors.insert(key.clone(), source.author);
                let annotation = self
                    .typed
                    .annotations
                    .iter()
                    .find(|annotation| annotation.message == source.id)
                    .or_else(|| {
                        self.typed
                            .annotations
                            .iter()
                            .find(|annotation| source.aliases.contains(&annotation.message))
                    });
                let metadata = annotation
                    .filter(|annotation| {
                        annotation.source == fingerprint(&source.text, source.author)
                            && annotation.metadata.author == source.author
                    })
                    .map(|annotation| annotation.metadata);
                Message {
                    key,
                    text: source.text.clone(),
                    metadata,
                }
            })
            .collect::<Vec<_>>();
        self.typed.messages = messages.clone();
        let ready = self.typed.loaded && !self.typed.writing;
        self.composer.update(cx, |composer, cx| {
            composer.set_typed_sources(&scope, messages, authors, ready, cx)
        });
        self.feed
            .update(cx, |feed, cx| feed.typed_labels_changed(cx));
    }

    pub(in crate::project) fn typed_retype(
        &mut self,
        request: &RetypeRequested,
        cx: &mut Context<Self>,
    ) {
        self.typed_sync(cx);
        if self.typed_scope().as_ref() != Some(&request.scope)
            || !self.typed.loaded
            || self.typed.writing
        {
            return;
        }
        let Some(source) = self
            .typed
            .sources
            .iter()
            .find(|source| source.id == request.expected.key.message)
        else {
            return;
        };
        let result = super::corrected_message_with_author(
            &self.typed.messages,
            &request.expected,
            request
                .replacement
                .metadata
                .map_or(MessageType::Chat, |metadata| metadata.kind),
            Some(source.author),
        );
        if result.as_ref().ok() != Some(&request.replacement) {
            return;
        }
        let Some(store) = self.typed.store.clone() else {
            return;
        };
        let replacement = Annotation {
            message: source.id.clone(),
            source: fingerprint(&source.text, source.author),
            metadata: request
                .replacement
                .metadata
                .expect("correction has metadata"),
        };
        let expected = self
            .typed
            .annotations
            .iter()
            .find(|annotation| annotation.message == source.id)
            .cloned();
        let revision = self.typed.revision;
        let epoch = self.typed.epoch;
        let scope = request.scope.clone();
        self.typed.writing = true;
        self.typed.source_revision = None;
        self.typed_sync(cx);
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { write(&store, revision, expected.as_ref(), replacement) })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.typed.epoch != epoch || this.typed_scope().as_ref() != Some(&scope) {
                    return;
                }
                this.typed.writing = false;
                this.typed.source_revision = None;
                match result {
                    Ok((revision, annotations)) => {
                        this.typed.revision = revision;
                        this.typed.annotations = annotations;
                        this.typed.source_revision = None;
                        this.typed_sync(cx);
                        this.composer.update(cx, |composer, cx| {
                            composer.typed_write_succeeded(&scope, cx)
                        });
                    }
                    Err(error) => {
                        this.typed_sync(cx);
                        this.composer.update(cx, |composer, cx| {
                            composer.typed_write_failed(&scope, error, cx)
                        });
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(in crate::project) fn typed_open_message(
        &mut self,
        painted_scope: &TypedScope,
        expected: &Message,
        cx: &mut Context<Self>,
    ) {
        self.typed_sync(cx);
        let key = &expected.key;
        let Some(scope) = self.typed_scope().filter(|scope| {
            key.environment == scope.environment
                && key.project == scope.project
                && key.thread == scope.thread
        }) else {
            return;
        };
        if &scope != painted_scope
            || !self
                .typed
                .messages
                .iter()
                .any(|message| message == expected)
        {
            return;
        }
        let Some(source) = self
            .typed
            .sources
            .iter()
            .find(|source| source.id == key.message)
            .cloned()
        else {
            return;
        };
        let feed = self.feed.clone();
        // Defer until the parent entity is no longer borrowed; Feed independently
        // checks the live owner and full source again before scrolling.
        App::defer(cx, move |cx| {
            feed.update(cx, |feed, cx| {
                feed.reveal_typed_source(&scope, &source, cx);
            });
        });
    }
}

#[cfg(test)]
#[path = "integration_tests.rs"]
mod tests;

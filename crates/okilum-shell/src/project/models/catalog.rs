//! A credential/environment-scoped catalog shared by Manager views. Opening a
//! picker only reads this entity. One configuration stream owns each warmup.
use super::*;
use okilum_t3::stream::{model_catalog, CancelSubscription, NativeClient, StreamError};
use std::collections::HashMap;

#[derive(Default)]
struct Catalogs(HashMap<[u8; 32], WeakEntity<Catalog>>);
impl Global for Catalogs {}

pub(in crate::project) struct Catalog {
    pub data: Option<model_catalog::Snapshot>,
    pub loading: bool,
    pub error: Option<String>,
    client: Arc<NativeClient>,
    epoch: u64,
    cancel: Option<CancelSubscription>,
    worker: Option<Task<()>>,
}
impl Drop for Catalog {
    fn drop(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            cancel.cancel();
        }
    }
}
impl Catalog {
    pub(in crate::project) fn acquire(client: Arc<NativeClient>, cx: &mut App) -> Entity<Self> {
        let scope = client.catalog_scope();
        if let Some(existing) = cx
            .default_global::<Catalogs>()
            .0
            .get(&scope)
            .and_then(WeakEntity::upgrade)
        {
            existing.update(cx, |catalog, cx| {
                if !catalog.client.access_allowed() {
                    catalog.client = client;
                    catalog.restart(cx);
                }
            });
            return existing;
        }
        let catalog = cx.new(|cx| {
            let mut catalog = Self {
                data: None,
                loading: false,
                error: None,
                client,
                epoch: 0,
                cancel: None,
                worker: None,
            };
            catalog.restart(cx);
            catalog
        });
        let registry = cx.default_global::<Catalogs>();
        registry.0.retain(|_, value| value.upgrade().is_some());
        registry.0.insert(scope, catalog.downgrade());
        catalog
    }

    pub(in crate::project) fn refresh(&mut self, cx: &mut Context<Self>) {
        // Coalesce refresh/reconnect signals while the initial snapshot is in flight.
        if !self.loading && self.client.access_allowed() {
            self.restart(cx);
        }
    }

    fn restart(&mut self, cx: &mut Context<Self>) {
        if let Some(cancel) = self.cancel.take() {
            cancel.cancel();
        }
        self.worker.take();
        self.epoch = self.epoch.wrapping_add(1);
        let epoch = self.epoch;
        self.loading = true;
        self.error = None;
        let client = self.client.clone();
        self.worker = Some(cx.spawn(async move |this: WeakEntity<Self>, cx| {
            let mut failures = 0;
            'connect: loop {
                if !client.access_allowed() {
                    return;
                }
                let connect = client.clone();
                let opened = super::super::native::recovery::stream_io(
                    cx.background_executor(),
                    move || connect.subscribe_model_catalog(),
                )
                .await;
                let mut stream = match opened {
                    Ok(stream) => stream,
                    Err(error) => {
                        if !Self::failed(&this, epoch, &error, cx) {
                            return;
                        }
                        failures += 1;
                        cx.background_executor()
                            .timer(super::super::native::delay(failures))
                            .await;
                        continue;
                    }
                };
                let cancel = stream.cancellation().ok();
                if !this
                    .update(cx, |catalog, _| {
                        if catalog.epoch != epoch {
                            return false;
                        }
                        catalog.cancel = cancel;
                        true
                    })
                    .unwrap_or(false)
                {
                    return;
                }
                loop {
                    let (returned, updates) = super::super::native::recovery::stream_io(
                        cx.background_executor(),
                        move || {
                            let mut updates = Vec::new();
                            let result = stream.receive(|batch| {
                                updates.extend_from_slice(batch);
                                Ok(())
                            });
                            (stream, result.map(|_| updates))
                        },
                    )
                    .await;
                    stream = returned;
                    let updates = match updates {
                        Ok(updates) => updates,
                        Err(error) => {
                            if !Self::failed(&this, epoch, &error, cx) {
                                return;
                            }
                            failures += 1;
                            cx.background_executor()
                                .timer(super::super::native::delay(failures))
                                .await;
                            continue 'connect;
                        }
                    };
                    failures = 0;
                    let refresh = this
                        .update(cx, |catalog, cx| catalog.apply_batch(epoch, updates, cx))
                        .ok()
                        .flatten();
                    match refresh {
                        None => return,
                        Some(true) => {
                            // Re-snapshot the supported config stream once per
                            // batch of settings changes, never one RPC per view.
                            drop(stream);
                            continue 'connect;
                        }
                        Some(false) => {}
                    }
                }
            }
        }));
        cx.notify();
    }

    fn apply_batch(
        &mut self,
        epoch: u64,
        updates: Vec<model_catalog::Update>,
        cx: &mut Context<Self>,
    ) -> Option<bool> {
        if self.epoch != epoch || !self.client.access_allowed() {
            return None;
        }
        let mut refresh = false;
        for update in updates {
            match update {
                model_catalog::Update::Snapshot(snapshot) => {
                    self.data = Some(snapshot);
                    self.loading = false;
                    self.error = None;
                }
                model_catalog::Update::Providers(providers) => {
                    if let Some(data) = &mut self.data {
                        data.providers = providers;
                    }
                }
                model_catalog::Update::Refresh => refresh = true,
                model_catalog::Update::Unchanged => {}
            }
        }
        if refresh {
            self.loading = true;
        }
        cx.notify();
        Some(refresh)
    }

    fn failed(this: &WeakEntity<Self>, epoch: u64, error: &StreamError, cx: &mut AsyncApp) -> bool {
        this.update(cx, |catalog, cx| {
            if catalog.epoch != epoch || !catalog.client.access_allowed() {
                return false;
            }
            catalog.loading = false;
            catalog.error = Some(format!("Models are unavailable. {error}"));
            cx.notify();
            matches!(
                error,
                StreamError::Closed
                    | StreamError::Unreachable
                    | StreamError::Http(408 | 429 | 500..=599)
            )
        })
        .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests;

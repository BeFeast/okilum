//! Model catalogs arrive independently of opening a model picker. The initial
//! snapshot and provider changes use T3's existing server configuration stream.
use super::*;
use crate::{ModelInfo, ProviderCapability, ServerIdentity};
use std::collections::BTreeSet;

#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    pub environment: ServerIdentity,
    pub providers: Vec<ProviderCapability>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Update {
    Snapshot(Snapshot),
    Providers(Vec<ProviderCapability>),
    /// Settings can change provider instances. The consumer coalesces this
    /// signal into one bounded refresh, independently of the picker lifecycle.
    Refresh,
    Unchanged,
}

fn invalid() -> StreamError {
    StreamError::Protocol("invalid native model catalog")
}

fn nonempty(value: &Value, name: &str) -> StreamResult<String> {
    value[name]
        .as_str()
        .filter(|text| !text.trim().is_empty())
        .map(str::to_owned)
        .ok_or_else(invalid)
}

pub(super) fn providers(value: &Value) -> StreamResult<Vec<ProviderCapability>> {
    let rows = value.as_array().ok_or_else(invalid)?;
    let mut instances = BTreeSet::new();
    let mut result = Vec::new();
    for row in rows {
        match row["enabled"].as_bool() {
            Some(false) => continue,
            Some(true) => {}
            None => return Err(invalid()),
        }
        let instance = nonempty(row, "instanceId")?;
        if !instances.insert(instance.clone()) {
            return Err(invalid());
        }
        let mut model_ids = BTreeSet::new();
        let models = row["models"]
            .as_array()
            .ok_or_else(invalid)?
            .iter()
            .map(|model| {
                let id = nonempty(model, "slug")?;
                if !model_ids.insert(id.clone()) {
                    return Err(invalid());
                }
                Ok(ModelInfo {
                    id,
                    label: model["name"].as_str().map(str::to_owned),
                    short_name: model["shortName"].as_str().map(str::to_owned),
                    sub_provider: model["subProvider"].as_str().map(str::to_owned),
                    capabilities: match model.get("capabilities") {
                        None | Some(Value::Null) => None,
                        Some(value) => {
                            Some(serde_json::from_value(value.clone()).map_err(|_| invalid())?)
                        }
                    },
                })
            })
            .collect::<StreamResult<Vec<_>>>()?;
        result.push(ProviderCapability {
            provider_instance_id: instance,
            driver_kind: nonempty(row, "driver")?,
            display_name: row["displayName"].as_str().map(str::to_owned),
            models,
            runtime_modes: match row.get("supportedRuntimeModes") {
                None => Vec::new(),
                Some(value) => value
                    .as_array()
                    .ok_or_else(invalid)?
                    .iter()
                    .map(|mode| {
                        mode.as_str()
                            .filter(|mode| !mode.trim().is_empty())
                            .map(str::to_owned)
                            .ok_or_else(invalid)
                    })
                    .collect::<StreamResult<Vec<_>>>()?,
            },
        });
    }
    Ok(result)
}

fn snapshot(value: &Value) -> StreamResult<Snapshot> {
    let environment: ServerIdentity =
        serde_json::from_value(value["environment"].clone()).map_err(|_| invalid())?;
    if environment.environment_id.trim().is_empty() || environment.label.trim().is_empty() {
        return Err(invalid());
    }
    Ok(Snapshot {
        environment,
        providers: providers(&value["providers"])?,
    })
}

fn decode(value: &Value, initialized: bool) -> StreamResult<Vec<Update>> {
    let values = value["values"]
        .as_array()
        .filter(|values| !values.is_empty())
        .ok_or_else(invalid)?;
    let mut initialized = initialized;
    values
        .iter()
        .map(|value| {
            if value["version"] != 1 {
                return Err(StreamError::IncompatibleProtocol);
            }
            if value["type"] == "snapshot" {
                let result = snapshot(&value["config"])?;
                initialized = true;
                return Ok(Update::Snapshot(result));
            }
            if !initialized {
                return Err(invalid());
            }
            match value["type"].as_str() {
                Some("providerStatuses") => Ok(Update::Providers(providers(
                    &value["payload"]["providers"],
                )?)),
                Some("settingsUpdated") if value["payload"]["settings"].is_object() => {
                    Ok(Update::Refresh)
                }
                Some("settingsUpdated") => Err(invalid()),
                Some(
                    "keybindingsUpdated" | "environmentThemesUpdated" | "usageLimitSourcesUpdated",
                ) => Ok(Update::Unchanged),
                _ => Err(StreamError::IncompatibleProtocol),
            }
        })
        .collect()
}

pub struct Subscription {
    inner: ThreadSubscription,
    initialized: bool,
}

impl NativeClient {
    /// One subscription per authenticated environment; never call from render
    /// or a picker-open callback. No provider refresh/mutation is requested.
    pub fn subscribe_model_catalog(&self) -> StreamResult<Subscription> {
        if !self.access.allowed() {
            return Err(StreamError::Closed);
        }
        self.token.validate(&self.environment, crate::unix_now())?;
        let (socket, cancel, phases) = self.open_socket()?;
        let mut inner = ThreadSubscription {
            access: self.access.clone(),
            socket,
            cancel,
            phases,
            origin: self.environment.origin(),
            thread_id: String::new(),
            cursor: None,
            closed: false,
        };
        inner.send(json!({"_tag":"Request","id":"1","tag":"subscribeServerConfig","payload":{},"headers":[]}))?;
        Ok(Subscription {
            inner,
            initialized: false,
        })
    }

    /// Existing read-only RPC, used only for coalesced configuration refreshes.
    pub fn model_catalog_snapshot(&self) -> StreamResult<Snapshot> {
        if !self.access.allowed() {
            return Err(StreamError::Closed);
        }
        snapshot(&self.manager_rpc("server.getConfig", json!({}), false)?)
    }
}

impl Subscription {
    pub fn cancellation(&self) -> StreamResult<CancelSubscription> {
        self.inner.cancellation()
    }

    pub fn receive(
        &mut self,
        apply: impl FnOnce(&[Update]) -> StreamResult<()>,
    ) -> StreamResult<()> {
        let result = (|| {
            if self.inner.closed || !self.inner.access.allowed() {
                return Err(StreamError::Closed);
            }
            let value = self.inner.read_chunk()?;
            let updates = decode(&value, self.initialized)?;
            apply(&updates)?;
            self.initialized |= updates
                .iter()
                .any(|update| matches!(update, Update::Snapshot(_)));
            self.inner.send(json!({"_tag":"Ack","requestId":"1"}))
        })();
        if result.is_err() {
            self.inner.closed = true;
            let _ = self.inner.cancel.shutdown(Shutdown::Both);
        }
        result
    }
}

#[cfg(test)]
mod tests;

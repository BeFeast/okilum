//! Detached Inbox backend.
pub mod auth;
pub mod execution;
pub mod http;
pub mod store;
mod web;

pub mod discussion;
pub mod provider;

pub mod publication;
#[cfg(target_os = "linux")]
pub mod vault;
#[cfg(not(target_os = "linux"))]
pub mod vault {
    use crate::{
        publication::{Publication, Publish},
        store::{Error, Store},
    };
    pub struct Vault;
    impl Vault {
        pub fn open(_: &std::path::Path, _: Vec<String>) -> Result<Self, Error> {
            Err(Error::VaultUnavailable)
        }
        pub fn folders(&self) -> &[String] {
            &[]
        }
        pub fn inspect(&self, _: &mut Publication) {}
        pub fn publish(
            &self,
            _: &mut Store,
            _: tessera_inbox_domain::OwnerId,
            _: uuid::Uuid,
            _: &Publish,
        ) -> Result<Publication, Error> {
            Err(Error::VaultUnavailable)
        }
    }
}

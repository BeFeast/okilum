//! Explicit reconnect reuses an origin-bound keychain credential. No refresh
//! grant, automatic browser sign-in, or retry of a failed network connection.
use crate::{Environment, GatewayError, McpGateway, StoredToken, TokenFreshness};

pub struct Connected {
    pub gateway: McpGateway,
    /// Persist newly authorized credentials before polling; a later transient
    /// server failure must not throw away a successful browser authorization.
    pub issued_token: Option<StoredToken>,
}

/// `saved` includes the key it was read under. Never send a credential from
/// another origin while changing the address in the connection field.
pub fn connect(
    environment: &Environment,
    saved: Option<(String, StoredToken)>,
    sign_in_again: bool,
    authorize: impl FnOnce() -> anyhow::Result<StoredToken>,
) -> anyhow::Result<Connected> {
    if !sign_in_again {
        if let Some((_, token)) = saved.filter(|(key, token)| {
            key == &environment.credential_url()
                && token.freshness(crate::unix_now()) != TokenFreshness::Expired
        }) {
            let gateway = McpGateway::new(environment.clone(), Some(token))?;
            match gateway.connect() {
                Ok(()) => {
                    return Ok(Connected {
                        gateway,
                        issued_token: None,
                    })
                }
                Err(GatewayError::InvalidCredential) => {}
                Err(error) => return Err(error.into()),
            }
        }
    }
    let token = authorize()?;
    let gateway = McpGateway::new(environment.clone(), Some(token.clone()))?;
    Ok(Connected {
        gateway,
        issued_token: Some(token),
    })
}

#[cfg(test)]
mod tests;

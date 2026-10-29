//! Explicit reconnect reuses an origin-bound keychain credential. No refresh
//! grant or automatic browser sign-in. Local-network permission recovery uses
//! one explicit connection-attempt retry budget, never a feature-operation retry.
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
    connect_with_retry(
        environment,
        saved,
        sign_in_again,
        |_| authorize(),
        &mut crate::connection::Retry::default(),
        || {},
    )
}

/// Share one local-network retry budget across credential verification and OAuth
/// preparation. The caller can use the remaining budget for the final check.
pub fn connect_with_retry(
    environment: &Environment,
    saved: Option<(String, StoredToken)>,
    sign_in_again: bool,
    authorize: impl FnOnce(&mut crate::connection::Retry) -> anyhow::Result<StoredToken>,
    retry: &mut crate::connection::Retry,
    notice: impl FnOnce(),
) -> anyhow::Result<Connected> {
    if !sign_in_again {
        if let Some((_, token)) = saved.filter(|(key, token)| {
            key == &environment.credential_url()
                && token.freshness(crate::unix_now()) != TokenFreshness::Expired
        }) {
            let gateway = McpGateway::new(environment.clone(), Some(token))?;
            match retry.run(
                environment,
                || gateway.connect().map_err(Into::into),
                notice,
            ) {
                Ok(()) => {
                    return Ok(Connected {
                        gateway,
                        issued_token: None,
                    })
                }
                Err(error)
                    if error.downcast_ref::<GatewayError>()
                        == Some(&GatewayError::InvalidCredential) => {}
                Err(error) => return Err(error),
            }
        }
    }
    let token = authorize(retry)?;
    let gateway = McpGateway::new(environment.clone(), Some(token.clone()))?;
    Ok(Connected {
        gateway,
        issued_token: Some(token),
    })
}

#[cfg(test)]
mod tests;

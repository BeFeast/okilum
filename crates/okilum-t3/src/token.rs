use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// A token closer than this to its expiry is reported as "expires soon".
/// T3 issues MCP tokens for 30 days and has no refresh grant, so the user
/// has to reconnect; three days gives them warning.
pub const EXPIRES_SOON_SECS: u64 = 3 * 24 * 60 * 60;

/// An OAuth access token for one environment, as kept in the OS keychain.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredToken {
    pub access_token: String,
    /// Unix seconds.
    pub expires_at: u64,
    pub scope: String,
    /// The client id T3 issued at registration; reconnecting reuses it.
    pub client_id: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenFreshness {
    Valid,
    ExpiresSoon,
    Expired,
}

impl StoredToken {
    pub fn freshness(&self, now: u64) -> TokenFreshness {
        if now >= self.expires_at {
            TokenFreshness::Expired
        } else if self.expires_at - now <= EXPIRES_SOON_SECS {
            TokenFreshness::ExpiresSoon
        } else {
            TokenFreshness::Valid
        }
    }

    /// The keychain payload.
    pub fn to_secret(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("a token serialises")
    }

    pub fn from_secret(secret: &[u8]) -> Result<Self> {
        serde_json::from_slice(secret).context("the stored T3 token is not readable")
    }
}

impl std::fmt::Debug for StoredToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoredToken")
            .field("access_token", &"<redacted>")
            .field("expires_at", &self.expires_at)
            .field("scope", &self.scope)
            .field("client_id", &self.client_id)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token(expires_at: u64) -> StoredToken {
        StoredToken {
            access_token: "secret-value".into(),
            expires_at,
            scope: "orchestration:read orchestration:operate".into(),
            client_id: "client".into(),
        }
    }

    #[test]
    fn freshness_crosses_both_thresholds() {
        let now = 1_000_000;
        assert_eq!(
            token(now + EXPIRES_SOON_SECS + 1).freshness(now),
            TokenFreshness::Valid
        );
        assert_eq!(
            token(now + EXPIRES_SOON_SECS).freshness(now),
            TokenFreshness::ExpiresSoon
        );
        assert_eq!(token(now + 1).freshness(now), TokenFreshness::ExpiresSoon);
        assert_eq!(token(now).freshness(now), TokenFreshness::Expired);
    }

    #[test]
    fn debug_never_prints_the_token() {
        let printed = format!("{:?}", token(5));
        assert!(!printed.contains("secret-value"), "{printed}");
        assert!(printed.contains("<redacted>"));
    }

    #[test]
    fn secret_round_trips() {
        let original = token(42);
        assert_eq!(
            StoredToken::from_secret(&original.to_secret()).unwrap(),
            original
        );
        assert!(StoredToken::from_secret(b"not json").is_err());
    }
}

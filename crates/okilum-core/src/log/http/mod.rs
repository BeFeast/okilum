//! Read-only HTTPS file adapter. Own on a background worker; poll returns at most
//! 1 MiB atomically. The owner retains record fragments between polls and displays
//! `status()`, including disconnects. No source or credentials are persisted here.
mod protocol;
use protocol::{Cursor, CHUNK_BYTES};
pub use protocol::{Error, Status};
use std::{
    io::Read,
    time::{Duration, Instant},
};

pub struct HttpLog {
    client: reqwest::blocking::Client,
    url: url::Url,
    cursor: Cursor,
    retry_at: Option<Instant>,
}
impl HttpLog {
    pub fn open(source: &str) -> anyhow::Result<Self> {
        let url = url::Url::parse(source).map_err(|_| anyhow::anyhow!("Invalid remote log URL"))?;
        anyhow::ensure!(
            url.scheme() == "https" && url.host_str().is_some(),
            "Remote log requires HTTPS"
        );
        anyhow::ensure!(
            url.username().is_empty() && url.password().is_none() && url.fragment().is_none(),
            "Remote log URL must not contain credentials or a fragment"
        );
        let client =
            okilum_http::builder(source, env!("CARGO_PKG_VERSION"), Duration::from_secs(10))
                .map_err(|_| anyhow::anyhow!("Cannot configure remote log HTTPS client"))?
                .no_gzip()
                .no_brotli()
                .no_deflate()
                .no_zstd()
                .build()
                .map_err(|_| anyhow::anyhow!("Cannot build remote log HTTPS client"))?;
        Ok(Self {
            client,
            url,
            cursor: Cursor::default(),
            retry_at: None,
        })
    }
    pub fn status(&self) -> &Status {
        &self.cursor.status
    }
    pub fn offset(&self) -> u64 {
        self.cursor.offset
    }

    /// No sleeping or internal retry loop. During backoff, returns no bytes;
    /// after five consecutive failures, an explicit reopen is required.
    pub fn poll(&mut self) -> Result<Vec<u8>, Error> {
        if let Status::Stopped(error) = &self.cursor.status {
            return Err(error.clone());
        }
        if self.retry_at.is_some_and(|at| Instant::now() < at) {
            return Ok(Vec::new());
        }
        let request = self
            .client
            .get(self.url.clone())
            .header(reqwest::header::RANGE, self.cursor.range())
            .header(reqwest::header::ACCEPT_ENCODING, "identity");
        let mut response = match request.send() {
            Ok(response) => response,
            Err(_) => return self.outage(),
        };
        let code = response.status().as_u16();
        if code == 429 || (500..=599).contains(&code) {
            return self.outage();
        }
        let expected = match self.cursor.expected(
            code,
            response
                .headers()
                .get(reqwest::header::CONTENT_RANGE)
                .and_then(|v| v.to_str().ok()),
        ) {
            Ok(expected) => expected,
            Err(error) => return self.stop(error),
        };
        if response
            .headers()
            .get(reqwest::header::CONTENT_ENCODING)
            .is_some_and(|v| v != "identity")
        {
            return self.stop(Error::InvalidRange);
        }
        let mut bytes = Vec::new();
        if expected > 0 {
            if response
                .by_ref()
                .take(CHUNK_BYTES + 1)
                .read_to_end(&mut bytes)
                .is_err()
            {
                return self.outage();
            }
            if (bytes.len() as u64) < expected {
                return self.outage();
            }
            if bytes.len() as u64 != expected {
                return self.stop(Error::InvalidRange);
            }
        }
        self.cursor.commit(bytes.len() as u64);
        self.retry_at = None;
        Ok(bytes)
    }
    fn stop(&mut self, error: Error) -> Result<Vec<u8>, Error> {
        self.cursor.status = Status::Stopped(error.clone());
        Err(error)
    }
    fn outage(&mut self) -> Result<Vec<u8>, Error> {
        self.cursor.disconnected();
        if let Status::Disconnected { retry_after } = self.cursor.status {
            self.retry_at = Some(Instant::now() + retry_after);
            Ok(Vec::new())
        } else {
            Err(Error::RetryExhausted)
        }
    }
}

#[cfg(test)]
mod tests;

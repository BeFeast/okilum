//! Retained PKCE attempt: a closed loopback wait must not discard the verifier.
use super::*;
use std::sync::mpsc::{Receiver, TryRecvError};

pub const DEFAULT_WAIT: Duration = Duration::from_secs(10 * 60);

pub enum Input {
    Callback(String),
    WaitLonger,
    Cancel,
}

pub enum Progress {
    Waiting(Duration),
    TimedOut,
    InvalidCallback(String),
}

pub struct Authorization {
    http: Client,
    metadata: AuthServerMetadata,
    client_id: String,
    pkce: Pkce,
    state: String,
    resource: Url,
    redirect_uri: String,
    listener: Option<TcpListener>,
    url: Url,
}

impl Authorization {
    pub fn prepare(env: &Environment, client_name: &str) -> Result<Self> {
        let http = http_client()?;
        let metadata = discover(&http, env)?;
        let callback = Callback::bind()?;
        callback.listener.set_nonblocking(true)?;
        let client_id = register(&http, &metadata, client_name, callback.redirect_uri())?;
        let pkce = Pkce::generate()?;
        let state = random_token()?;
        let resource = env.mcp_url();
        let url = authorization_url(
            &metadata,
            &client_id,
            callback.redirect_uri(),
            &state,
            &pkce,
            &resource,
        )?;
        Ok(Self {
            http,
            metadata,
            client_id,
            pkce,
            state,
            resource,
            redirect_uri: callback.redirect_uri,
            listener: Some(callback.listener),
            url,
        })
    }

    pub fn url(&self) -> &Url {
        &self.url
    }

    fn code(&self, input: &str) -> Result<String> {
        let url =
            Url::parse(input.trim()).context("Paste the complete callback URL from the browser")?;
        let redirect = Url::parse(&self.redirect_uri)?;
        ensure!(
            url.origin() == redirect.origin()
                && url.path() == CALLBACK_PATH
                && url.username().is_empty()
                && url.password().is_none()
                && url.fragment().is_none(),
            "This callback belongs to another connection; use the URL from the current sign-in"
        );
        let param = |name: &str| -> Result<Option<String>> {
            let values: Vec<_> = url
                .query_pairs()
                .filter(|(k, _)| k == name)
                .map(|(_, v)| v.into_owned())
                .collect();
            ensure!(values.len() <= 1, "The callback contains a repeated {name}");
            Ok(values.into_iter().next())
        };
        ensure!(
            param("state")?.as_deref() == Some(&self.state),
            "The callback has a different state; use the URL from the current sign-in"
        );
        let returned = param("iss")?;
        if self.metadata.authorization_response_iss_parameter_supported || returned.is_some() {
            ensure!(
                returned.as_deref().map(|s| s.trim_end_matches('/'))
                    == Some(self.metadata.issuer.trim_end_matches('/')),
                "The callback has a different issuer"
            );
        }
        if let Some(error) = param("error")? {
            bail!("T3 did not approve the connection: {error}");
        }
        param("code")?
            .filter(|c| !c.is_empty())
            .ok_or_else(|| anyhow!("The callback has no code"))
    }

    /// One foreground worker owns the attempt, including the code exchange.
    /// After timeout the listener closes; manual paste still has the original
    /// redirect URI, state, issuer and verifier. WaitLonger reopens that port.
    pub fn wait(
        mut self,
        input: Receiver<Input>,
        mut progress: impl FnMut(Progress),
        timeout: Duration,
    ) -> Result<StoredToken> {
        let mut listener = self.listener.take();
        let redirect_uri = self.redirect_uri.clone();
        let mut deadline = Instant::now() + timeout;
        let mut last_seconds = u64::MAX;
        loop {
            let command = match input.try_recv() {
                Ok(Input::Cancel) | Err(TryRecvError::Disconnected) => {
                    bail!("Sign-in cancelled. Choose Sign in to start again.")
                }
                Ok(command) => Some(command),
                Err(TryRecvError::Empty) => None,
            };
            if matches!(&command, Some(Input::WaitLonger)) {
                if listener.is_none() {
                    let address = Url::parse(&redirect_uri)?.socket_addrs(|| None)?[0];
                    let reopened = TcpListener::bind(address).context(
                        "Cannot resume browser return; paste its callback URL or start again",
                    )?;
                    reopened.set_nonblocking(true)?;
                    listener = Some(reopened);
                }
                deadline = Instant::now() + timeout;
                last_seconds = u64::MAX;
            }
            let pasted = match command {
                Some(Input::Callback(url)) => Some(url),
                _ => None,
            };
            let mut stream = None;
            let candidate = if pasted.is_some() {
                pasted
            } else if let Some(listener) = &listener {
                match listener.accept() {
                    Ok((socket, _)) => {
                        socket.set_read_timeout(Some(Duration::from_secs(2)))?;
                        let mut line = String::new();
                        BufReader::new(&socket).read_line(&mut line)?;
                        let target = line.split_whitespace().nth(1).unwrap_or_default();
                        let url = Url::parse(&redirect_uri)?.join(target)?;
                        if url.path() != CALLBACK_PATH {
                            respond(&socket, 404, "Not found.");
                            None
                        } else {
                            stream = Some(socket);
                            Some(url.to_string())
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => None,
                    Err(e) => return Err(e.into()),
                }
            } else {
                None
            };
            if let Some(url) = candidate {
                match self.code(&url) {
                    Ok(code) => {
                        let token = exchange_code(
                            &self.http,
                            &self.metadata,
                            &self.client_id,
                            &code,
                            &self.pkce,
                            &redirect_uri,
                            &self.resource,
                        );
                        if let Some(stream) = stream {
                            respond(
                                &stream,
                                if token.is_ok() { 200 } else { 400 },
                                if token.is_ok() {
                                    "Okilum is connected to T3. You can close this tab."
                                } else {
                                    "Return to Okilum and start sign-in again."
                                },
                            );
                        }
                        return token;
                    }
                    Err(e) => {
                        if let Some(stream) = stream {
                            respond(
                                &stream,
                                400,
                                "Return to Okilum and use the current sign-in.",
                            );
                        }
                        progress(Progress::InvalidCallback(e.to_string()));
                    }
                }
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                if listener.take().is_some() {
                    progress(Progress::TimedOut);
                }
            } else if !remaining.is_zero() && remaining.as_secs() != last_seconds {
                last_seconds = remaining.as_secs();
                progress(Progress::Waiting(remaining));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

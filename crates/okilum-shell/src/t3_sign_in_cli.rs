//! `okilum t3-sign-in`: renews the T3 MCP token without a browser.
//!
//! T3 issues MCP tokens for 30 days with no refresh grant. This command signs
//! in with a one-time T3 pairing code ([`okilum_t3::oauth::sign_in_with_pairing_code`])
//! and stores the token where the app reads it: the login-keychain internet
//! password under [`okilum_t3::Environment::credential_url`], written the way
//! GPUI's `write_credentials` writes it. Running the installed app's own binary
//! matters on macOS: an item written by this signed binary carries the app's
//! keychain partition, so the running app reads it without a password prompt,
//! which an item written by `/usr/bin/security` would not.
//!
//! The pairing code comes from `--code-file` or from `--code-command`, run with
//! `/bin/sh -c`, which prints the code or `t3 auth pairing create --json`
//! output (for example over ssh to the T3 host, so the code passes through no
//! chat or log). The code and token are never printed.

use std::time::Duration;

use anyhow::{bail, ensure, Context, Result};

const USAGE: &str = "\
Usage: okilum t3-sign-in --origin URL (--code-command CMD | --code-file PATH)
                         [--access LEVEL] [--client-name NAME] [--min-days DAYS]

Signs in to a T3 server's MCP endpoint with a one-time pairing code and stores
the 30-day token in the keychain item Okilum reads (macOS).

  --origin URL         T3 server origin, e.g. https://t3.example
  --code-command CMD   shell command printing a pairing code, or the output of
                       `t3 auth pairing create --json`
  --code-file PATH     file holding the same
  --access LEVEL       read-only, approval-required, auto-accept-edits, auto or
                       full-access (default full-access)
  --client-name NAME   name in T3's session list (default Okilum)
  --min-days DAYS      do nothing while the stored token has more than DAYS left
";

#[derive(Debug, PartialEq)]
enum CodeSource {
    Command(String),
    File(String),
}

#[derive(Debug, PartialEq)]
struct Request {
    origin: String,
    code: CodeSource,
    access: String,
    client_name: String,
    min_days: Option<f64>,
}

/// `Some(exit code)` when argv is `okilum t3-sign-in …`; `None` leaves every
/// other command line to the ordinary launcher.
pub(crate) fn entry() -> Option<i32> {
    let mut arguments = std::env::args_os().skip(1);
    if arguments.next().as_deref() != Some(std::ffi::OsStr::new("t3-sign-in")) {
        return None;
    }
    let Ok(arguments) = arguments
        .map(std::ffi::OsString::into_string)
        .collect::<Result<Vec<_>, _>>()
    else {
        diagnostic_error!("okilum t3-sign-in: arguments must be UTF-8");
        return Some(2);
    };
    if arguments.iter().any(|a| a == "-h" || a == "--help") {
        print!("{USAGE}");
        return Some(0);
    }
    let request = match parse(&arguments) {
        Ok(request) => request,
        Err(error) => {
            diagnostic_error!("okilum t3-sign-in: {error:#}\n\n{USAGE}");
            return Some(2);
        }
    };
    match run(&request) {
        Ok(message) => {
            diagnostic_println!("{message}");
            Some(0)
        }
        Err(error) => {
            diagnostic_error!("okilum t3-sign-in: {error:#}");
            Some(1)
        }
    }
}

fn parse(arguments: &[String]) -> Result<Request> {
    let mut origin = None;
    let mut code = None;
    let mut access = "full-access".to_owned();
    let mut client_name = "Okilum".to_owned();
    let mut min_days = None;
    let mut iter = arguments.iter();
    while let Some(flag) = iter.next() {
        let mut value = || {
            iter.next()
                .cloned()
                .with_context(|| format!("{flag} needs a value"))
        };
        match flag.as_str() {
            "--origin" => origin = Some(value()?),
            "--code-command" | "--code-file" => {
                ensure!(code.is_none(), "give one of --code-command and --code-file");
                let given = value()?;
                code = Some(if flag == "--code-command" {
                    CodeSource::Command(given)
                } else {
                    CodeSource::File(given)
                });
            }
            "--access" => {
                access = value()?;
                ensure!(
                    okilum_t3::oauth::ACCESS_LEVELS.contains(&access.as_str()),
                    "unknown access level {access}"
                );
            }
            "--client-name" => client_name = value()?,
            "--min-days" => {
                let days: f64 = value()?.parse().context("--min-days takes a number")?;
                ensure!(
                    days.is_finite() && days >= 0.0,
                    "--min-days must be 0 or more"
                );
                min_days = Some(days);
            }
            other => bail!("unknown argument {other}"),
        }
    }
    Ok(Request {
        origin: origin.context("--origin is required")?,
        code: code.context("give --code-command or --code-file")?,
        access,
        client_name,
        min_days,
    })
}

fn run(request: &Request) -> Result<String> {
    let env = okilum_t3::Environment::new("t3", &request.origin)?;
    let key = env.credential_url();
    if let Some(days) = request.min_days {
        if let Some(saved) = keychain::read(&key)? {
            if let Ok(token) = okilum_t3::StoredToken::from_secret(&saved) {
                let left = token.expires_at.saturating_sub(unix_now());
                if left as f64 > days * 86_400.0 {
                    return Ok(format!(
                        "The stored T3 token for {} has {:.1} days left; nothing to do.",
                        env.origin(),
                        left as f64 / 86_400.0
                    ));
                }
            }
        }
    }
    let pairing_code = pairing_code(&request.code)?;
    let token = okilum_t3::oauth::sign_in_with_pairing_code(
        &env,
        &request.client_name,
        &request.access,
        &pairing_code,
    )?;
    keychain::write(&key, "Okilum", &token.to_secret())?;
    Ok(format!(
        "Signed in to {} as \"{}\" ({}; scope {}). The token is in the keychain under {key} \
         and expires in {} days.",
        env.origin(),
        request.client_name,
        request.access,
        token.scope,
        token.expires_at.saturating_sub(unix_now()) / 86_400
    ))
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

fn pairing_code(source: &CodeSource) -> Result<String> {
    let raw = match source {
        CodeSource::File(path) => {
            std::fs::read_to_string(path).with_context(|| format!("cannot read {path}"))?
        }
        CodeSource::Command(command) => run_code_command(command)?,
    };
    extract_code(&raw)
}

fn run_code_command(command: &str) -> Result<String> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    let mut child = Command::new("/bin/sh")
        .args(["-c", command])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("cannot run --code-command")?;
    let deadline = std::time::Instant::now() + Duration::from_secs(120);
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!("--code-command did not finish within 120 s");
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let mut stdout = String::new();
    let mut stderr = String::new();
    if let Some(mut out) = child.stdout.take() {
        out.read_to_string(&mut stdout)?;
    }
    if let Some(mut err) = child.stderr.take() {
        err.read_to_string(&mut stderr)?;
    }
    // Only stderr is quoted: stdout carries the code.
    ensure!(
        status.success(),
        "--code-command failed ({status}): {}",
        tail(stderr.trim(), 400)
    );
    Ok(stdout)
}

fn tail(text: &str, max: usize) -> &str {
    let start = text.len().saturating_sub(max);
    let start = (start..=text.len())
        .find(|&i| text.is_char_boundary(i))
        .unwrap_or(text.len());
    &text[start..]
}

/// The code itself, or the `credential` of `t3 auth pairing create --json`.
fn extract_code(raw: &str) -> Result<String> {
    let raw = raw.trim();
    let code = if raw.starts_with('{') {
        serde_json::from_str::<serde_json::Value>(raw)
            .ok()
            .and_then(|value| value.get("credential")?.as_str().map(str::to_owned))
            .context("the pairing JSON has no credential")?
    } else {
        raw.to_owned()
    };
    ensure!(
        !code.is_empty() && !code.contains(char::is_whitespace),
        "the input holds no single pairing code"
    );
    Ok(code)
}

#[cfg(target_os = "macos")]
mod keychain {
    //! The calls GPUI's macOS `read_credentials`/`write_credentials` make:
    //! an internet password matched by server only, account and data set.
    use std::ptr;

    use anyhow::{bail, Result};
    use core_foundation_sys::base::{kCFAllocatorDefault, CFRelease, CFTypeRef};
    use core_foundation_sys::data::{CFDataCreate, CFDataGetBytePtr, CFDataGetLength, CFDataRef};
    use core_foundation_sys::dictionary::{
        kCFTypeDictionaryKeyCallBacks, kCFTypeDictionaryValueCallBacks, CFDictionaryCreateMutable,
        CFDictionaryRef, CFDictionarySetValue, CFMutableDictionaryRef,
    };
    use core_foundation_sys::number::kCFBooleanTrue;
    use core_foundation_sys::string::{
        kCFStringEncodingUTF8, CFStringCreateWithBytes, CFStringRef,
    };

    type OSStatus = i32;
    const ERR_SEC_SUCCESS: OSStatus = 0;
    const ERR_SEC_ITEM_NOT_FOUND: OSStatus = -25300;
    const ERR_SEC_USER_CANCELED: OSStatus = -128;

    #[link(name = "Security", kind = "framework")]
    extern "C" {
        static kSecClass: CFStringRef;
        static kSecClassInternetPassword: CFStringRef;
        static kSecAttrServer: CFStringRef;
        static kSecAttrAccount: CFStringRef;
        static kSecValueData: CFStringRef;
        static kSecReturnData: CFStringRef;
        fn SecItemUpdate(query: CFDictionaryRef, attributes: CFDictionaryRef) -> OSStatus;
        fn SecItemAdd(attributes: CFDictionaryRef, result: *mut CFTypeRef) -> OSStatus;
        fn SecItemCopyMatching(query: CFDictionaryRef, result: *mut CFTypeRef) -> OSStatus;
    }

    /// A CF object released on drop.
    struct Owned(CFTypeRef);
    impl Drop for Owned {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: created by a CF Create function and released once.
                unsafe { CFRelease(self.0) }
            }
        }
    }

    fn string(value: &str) -> Owned {
        // SAFETY: the bytes are valid UTF-8 for the given length.
        Owned(unsafe {
            CFStringCreateWithBytes(
                kCFAllocatorDefault,
                value.as_ptr(),
                value.len() as _,
                kCFStringEncodingUTF8,
                false as _,
            )
        } as CFTypeRef)
    }

    fn data(value: &[u8]) -> Owned {
        // SAFETY: CFDataCreate copies `len` bytes from a valid slice.
        Owned(
            unsafe { CFDataCreate(kCFAllocatorDefault, value.as_ptr(), value.len() as _) }
                as CFTypeRef,
        )
    }

    fn dictionary(pairs: &[(CFStringRef, CFTypeRef)]) -> Owned {
        // SAFETY: CFType callbacks retain keys and values for the dictionary's life.
        unsafe {
            let dict: CFMutableDictionaryRef = CFDictionaryCreateMutable(
                kCFAllocatorDefault,
                pairs.len() as _,
                &kCFTypeDictionaryKeyCallBacks,
                &kCFTypeDictionaryValueCallBacks,
            );
            for (key, value) in pairs {
                CFDictionarySetValue(dict, *key as _, *value as _);
            }
            Owned(dict as CFTypeRef)
        }
    }

    pub(super) fn read(server: &str) -> Result<Option<Vec<u8>>> {
        let server = string(server);
        // SAFETY: Security framework constants are valid for the process's life.
        let query = unsafe {
            dictionary(&[
                (kSecClass, kSecClassInternetPassword as CFTypeRef),
                (kSecAttrServer, server.0),
                (kSecReturnData, kCFBooleanTrue as CFTypeRef),
            ])
        };
        let mut result: CFTypeRef = ptr::null();
        // SAFETY: a valid query; `result` receives an owned CFData on success.
        let status = unsafe { SecItemCopyMatching(query.0 as CFDictionaryRef, &mut result) };
        let result = Owned(result);
        match status {
            ERR_SEC_SUCCESS => {}
            ERR_SEC_ITEM_NOT_FOUND | ERR_SEC_USER_CANCELED => return Ok(None),
            status => bail!("reading the keychain failed: {status}"),
        }
        // SAFETY: kSecReturnData alone returns one CFData.
        let bytes = unsafe {
            let data = result.0 as CFDataRef;
            std::slice::from_raw_parts(CFDataGetBytePtr(data), CFDataGetLength(data) as usize)
                .to_vec()
        };
        Ok(Some(bytes))
    }

    pub(super) fn write(server: &str, account: &str, secret: &[u8]) -> Result<()> {
        let server = string(server);
        let account = string(account);
        let secret = data(secret);
        // SAFETY: Security framework constants are valid for the process's life.
        let (query, attributes) = unsafe {
            (
                dictionary(&[
                    (kSecClass, kSecClassInternetPassword as CFTypeRef),
                    (kSecAttrServer, server.0),
                ]),
                dictionary(&[
                    (kSecClass, kSecClassInternetPassword as CFTypeRef),
                    (kSecAttrServer, server.0),
                    (kSecAttrAccount, account.0),
                    (kSecValueData, secret.0),
                ]),
            )
        };
        // SAFETY: valid dictionaries; no result is requested from SecItemAdd.
        let mut status =
            unsafe { SecItemUpdate(query.0 as CFDictionaryRef, attributes.0 as CFDictionaryRef) };
        let mut verb = "updating";
        if status == ERR_SEC_ITEM_NOT_FOUND {
            verb = "creating";
            // SAFETY: the same valid attributes; no result is requested.
            status = unsafe { SecItemAdd(attributes.0 as CFDictionaryRef, ptr::null_mut()) };
        }
        if status != ERR_SEC_SUCCESS {
            bail!("{verb} the keychain item failed: {status}");
        }
        Ok(())
    }
}

#[cfg(not(target_os = "macos"))]
mod keychain {
    use anyhow::{bail, Result};

    pub(super) fn read(_server: &str) -> Result<Option<Vec<u8>>> {
        Ok(None)
    }

    pub(super) fn write(_server: &str, _account: &str, _secret: &[u8]) -> Result<()> {
        bail!("storing the token is implemented for the macOS keychain only")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_a_scheduled_sign_in() {
        let request = parse(&args(&[
            "--origin",
            "https://t3.example",
            "--code-command",
            "ssh t3-host t3 auth pairing create --json",
            "--min-days",
            "5",
        ]))
        .unwrap();
        assert_eq!(
            request,
            Request {
                origin: "https://t3.example".into(),
                code: CodeSource::Command("ssh t3-host t3 auth pairing create --json".into()),
                access: "full-access".into(),
                client_name: "Okilum".into(),
                min_days: Some(5.0),
            }
        );
    }

    #[test]
    fn refuses_two_code_sources_unknown_levels_and_missing_origin() {
        let both = parse(&args(&[
            "--origin",
            "https://t3.example",
            "--code-file",
            "a",
            "--code-command",
            "b",
        ]));
        assert!(both.is_err());
        let level = parse(&args(&[
            "--origin",
            "https://t3.example",
            "--code-file",
            "a",
            "--access",
            "root",
        ]));
        assert!(level
            .unwrap_err()
            .to_string()
            .contains("unknown access level"));
        assert!(parse(&args(&["--code-file", "a"]))
            .unwrap_err()
            .to_string()
            .contains("--origin"));
    }

    #[test]
    fn extracts_a_plain_code_or_the_pairing_json_credential() {
        assert_eq!(extract_code("  ABCD2345EFGH\n").unwrap(), "ABCD2345EFGH");
        let json = r#"{"id":"x","credential":"ABCD2345EFGH","expiresAt":"2026-11-25T00:00:00Z"}"#;
        assert_eq!(extract_code(json).unwrap(), "ABCD2345EFGH");
        assert!(extract_code("").is_err());
        assert!(extract_code("two words").is_err());
        assert!(extract_code(r#"{"id":"x"}"#).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn a_failing_code_command_reports_stderr_but_never_stdout() {
        let error = run_code_command("echo SECRETCODE; echo broken >&2; exit 3").unwrap_err();
        let message = error.to_string();
        assert!(message.contains("broken"), "{message}");
        assert!(!message.contains("SECRETCODE"), "{message}");
    }
}

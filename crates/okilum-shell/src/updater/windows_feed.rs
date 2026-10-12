//! Shared client/publisher contract for Windows release channels.
#[derive(serde::Deserialize)]
struct Contract {
    /// The channel CI packs and publishes main builds into. Not the client's
    /// default: an install follows stable unless the user chose beta (#1307).
    #[cfg_attr(not(test), allow(dead_code))]
    default_channel: String,
    public_root: String,
    prefix: String,
}

fn contract() -> Contract {
    serde_json::from_str(include_str!("../../../../scripts/windows/channel.json"))
        .expect("Windows channel contract must be valid")
}

/// The channel an install follows, from the choice saved by Settings →
/// Updates (`windows-update-channel`). Only an explicit «beta» follows beta;
/// no file, or anything else, is stable (#1307). A promoted stable build is
/// the beta build re-signed, so the default cannot come from the binary.
pub(super) fn follows_beta(saved: Option<&str>) -> bool {
    saved.is_some_and(|value| value.trim() == "beta")
}

pub(super) fn endpoint(beta: bool) -> (String, &'static str) {
    let config = contract();
    let channel = if beta { "beta" } else { "stable" };
    (
        format!("{}/{}/{channel}/", config.public_root, config.prefix),
        channel,
    )
}

/// The record that a downloaded package was checked against a channel's feed:
/// `<channel> <sha256>`. Written after a download from that channel, or after
/// the feed was found to list the package; removed when the channel changes.
pub(super) fn verified_record(beta: bool, sha256: &str) -> String {
    format!("{} {sha256}", if beta { "beta" } else { "stable" })
}

/// Whether Velopack may apply the pending package when Okilum starts: only one
/// recorded as checked against the channel the install follows now (#1318).
/// Anything else waits for the next check, which keeps or discards it.
pub(super) fn may_apply_at_start(
    pending_sha256: Option<&str>,
    record: Option<&str>,
    beta: bool,
) -> bool {
    match (pending_sha256, record) {
        (Some(sha), Some(record)) => record
            .trim()
            .eq_ignore_ascii_case(&verified_record(beta, sha)),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pending_package_applies_at_start_only_when_checked_for_the_current_channel() {
        let beta_pkg = "aa11";
        // #1318: a beta package downloaded before the user chose Stable.
        let record = verified_record(true, beta_pkg);
        assert!(!may_apply_at_start(Some(beta_pkg), Some(&record), false));
        assert!(may_apply_at_start(Some(beta_pkg), Some(&record), true));
        // Never checked (older builds, or the record was removed on a channel change).
        assert!(!may_apply_at_start(Some(beta_pkg), None, true));
        // The record names another package.
        assert!(!may_apply_at_start(Some("bb22"), Some(&record), true));
        assert!(!may_apply_at_start(None, Some(&record), true));
        assert!(may_apply_at_start(
            Some("cc33"),
            Some(&format!("{}\r\n", verified_record(false, "cc33"))),
            false
        ));
    }

    #[test]
    fn an_install_follows_stable_unless_beta_was_chosen() {
        assert!(!follows_beta(None), "no saved choice: stable (#1307)");
        assert!(follows_beta(Some("beta")));
        assert!(follows_beta(Some("beta\r\n")));
        assert!(!follows_beta(Some("stable")));
        assert!(
            !follows_beta(Some("")),
            "an empty or unknown value is not a choice of beta"
        );
        assert!(!follows_beta(Some("Beta channel")));
    }
    use std::io::{Read, Write};
    use velopack::{
        bundle::Manifest,
        sources::{HttpSource, UpdateSource},
        HttpOptions,
    };

    #[test]
    fn sdk_requests_the_feed_path_published_by_ci() {
        let config = contract();
        assert_eq!(
            config.default_channel, "beta",
            "main builds publish to beta"
        );
        for beta in [true, false] {
            let (base, channel) = endpoint(beta);
            let expected = format!("/{}/{channel}/releases.{channel}.json", config.prefix);
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                    .unwrap();
                let mut request = Vec::new();
                let mut byte = [0];
                while !request.ends_with(b"\r\n\r\n") {
                    assert!(request.len() < 8192);
                    stream.read_exact(&mut byte).unwrap();
                    request.push(byte[0]);
                }
                let response = b"{\"Assets\":[]}";
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    response.len()
                )
                .unwrap();
                stream.write_all(response).unwrap();
                String::from_utf8(request).unwrap()
            });
            let base = base.replacen(&config.public_root, &format!("http://{address}"), 1);
            let source = HttpSource::new_with_options(
                base,
                HttpOptions {
                    TimeoutMilliseconds: 5000,
                    ..Default::default()
                },
            );
            source
                .get_release_feed(channel, &Manifest::default(), "test")
                .unwrap();
            let request = server.join().unwrap();
            let target = request.split_whitespace().nth(1).unwrap();
            assert_eq!(target.split('?').next().unwrap(), expected);
        }
    }
}

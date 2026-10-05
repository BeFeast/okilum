//! Shared client/publisher contract for Windows release channels.
#[derive(serde::Deserialize)]
struct Contract {
    default_channel: String,
    public_root: String,
    prefix: String,
}

fn contract() -> Contract {
    serde_json::from_str(include_str!("../../../../scripts/windows/channel.json"))
        .expect("Windows channel contract must be valid")
}

pub(super) fn default_beta() -> bool {
    contract().default_channel == "beta"
}

pub(super) fn endpoint(beta: bool) -> (String, &'static str) {
    let config = contract();
    let channel = if beta { "beta" } else { "stable" };
    (
        format!("{}/{}/{channel}/", config.public_root, config.prefix),
        channel,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use velopack::{
        bundle::Manifest,
        sources::{HttpSource, UpdateSource},
        HttpOptions,
    };

    #[test]
    fn sdk_requests_the_feed_path_published_by_ci() {
        let config = contract();
        assert!(
            default_beta(),
            "First publication and default client must both be beta"
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

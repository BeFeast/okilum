//! Versioned encrypted frames. URL possession is a temporary capability; persistent
//! Ed25519 identities bind both directions and the owner's explicit confirmation.
use anyhow::{ensure, Context, Result};
use ring::{
    aead,
    signature::{self, Ed25519KeyPair, KeyPair},
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpStream},
    time::Duration,
};
use uuid::Uuid;

pub(super) const MAX_FRAME: usize = 256 * 1024;
const AAD: &[u8] = b"okilum-direct-pairing-v1";

// Stop serialization at the bound, before an oversized value can allocate its
// complete JSON representation (including the nested signed body).
fn bounded_json<T: Serialize>(value: &T, limit: usize) -> Result<Vec<u8>> {
    struct Bounded {
        bytes: Vec<u8>,
        limit: usize,
    }
    impl Write for Bounded {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > self.limit - self.bytes.len() {
                return Err(std::io::Error::other("pairing frame exceeds bound"));
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut writer = Bounded {
        bytes: Vec::new(),
        limit,
    };
    serde_json::to_writer(&mut writer, value)?;
    Ok(writer.bytes)
}

pub(super) fn random<const N: usize>() -> Result<[u8; N]> {
    let mut bytes = [0; N];
    getrandom::fill(&mut bytes).map_err(|_| anyhow::anyhow!("secure randomness unavailable"))?;
    Ok(bytes)
}
pub(super) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
pub(super) fn key(seed: &[u8; 32]) -> Result<Ed25519KeyPair> {
    Ed25519KeyPair::from_seed_unchecked(seed)
        .map_err(|_| anyhow::anyhow!("invalid local signing identity"))
}
pub(super) fn public(seed: &[u8; 32]) -> Result<Vec<u8>> {
    Ok(key(seed)?.public_key().as_ref().to_vec())
}
pub(super) fn fingerprint(key: &[u8]) -> String {
    hex(&Sha256::digest(key))
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Signed {
    body: Vec<u8>,
    signer: Vec<u8>,
    signature: Vec<u8>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Envelope {
    pub scope: Uuid,
    nonce: [u8; 12],
    body: Vec<u8>,
}

pub(super) fn seal<T: Serialize>(
    scope: Uuid,
    credential: &[u8; 32],
    seed: &[u8; 32],
    body: &T,
) -> Result<Envelope> {
    let body = bounded_json(body, MAX_FRAME)?;
    let pair = key(seed)?;
    let signed = Signed {
        signature: pair.sign(&body).as_ref().to_vec(),
        signer: pair.public_key().as_ref().to_vec(),
        body,
    };
    let mut body = bounded_json(&signed, MAX_FRAME / 2 - 1)?;
    ensure!(body.len() < MAX_FRAME / 2, "pairing response exceeds bound");
    let nonce = random()?;
    let key = aead::LessSafeKey::new(
        aead::UnboundKey::new(&aead::AES_256_GCM, credential)
            .map_err(|_| anyhow::anyhow!("invalid encryption key"))?,
    );
    let mut aad = AAD.to_vec();
    aad.extend_from_slice(scope.as_bytes());
    key.seal_in_place_append_tag(
        aead::Nonce::assume_unique_for_key(nonce),
        aead::Aad::from(aad),
        &mut body,
    )
    .map_err(|_| anyhow::anyhow!("frame encryption failed"))?;
    Ok(Envelope { scope, nonce, body })
}
pub(super) fn open<T: DeserializeOwned>(
    mut envelope: Envelope,
    credential: &[u8; 32],
    expected: Option<&[u8]>,
) -> Result<(T, Vec<u8>)> {
    let key = aead::LessSafeKey::new(
        aead::UnboundKey::new(&aead::AES_256_GCM, credential)
            .map_err(|_| anyhow::anyhow!("invalid encryption key"))?,
    );
    let mut aad = AAD.to_vec();
    aad.extend_from_slice(envelope.scope.as_bytes());
    let clear = key
        .open_in_place(
            aead::Nonce::assume_unique_for_key(envelope.nonce),
            aead::Aad::from(aad),
            &mut envelope.body,
        )
        .map_err(|_| anyhow::anyhow!("pairing authentication failed"))?;
    let signed: Signed = serde_json::from_slice(clear)?;
    ensure!(
        expected.is_none_or(|key| key == signed.signer),
        "unexpected computer identity"
    );
    signature::UnparsedPublicKey::new(&signature::ED25519, &signed.signer)
        .verify(&signed.body, &signed.signature)
        .map_err(|_| anyhow::anyhow!("computer proof invalid"))?;
    Ok((serde_json::from_slice(&signed.body)?, signed.signer))
}
pub(super) fn read<T: DeserializeOwned>(stream: &mut TcpStream) -> Result<T> {
    let mut length = [0; 4];
    stream
        .read_exact(&mut length)
        .map_err(socket_timeout)
        .context("read pairing frame length (socket receive timeout may report WouldBlock)")?;
    let length = u32::from_be_bytes(length) as usize;
    ensure!(
        length > 0 && length <= MAX_FRAME,
        "pairing frame exceeds bound"
    );
    let mut body = vec![0; length];
    stream
        .read_exact(&mut body)
        .map_err(socket_timeout)
        .context("read pairing frame body (socket receive timeout may report WouldBlock)")?;
    Ok(serde_json::from_slice(&body)?)
}
pub(super) fn write<T: Serialize>(stream: &mut TcpStream, value: &T) -> Result<()> {
    let body = bounded_json(value, MAX_FRAME)?;
    ensure!(body.len() <= MAX_FRAME, "pairing frame exceeds bound");
    stream
        .write_all(&(body.len() as u32).to_be_bytes())
        .map_err(socket_timeout)?;
    stream.write_all(&body).map_err(socket_timeout)?;
    stream.flush().map_err(socket_timeout)?;
    Ok(())
}
pub(super) fn connect(endpoint: SocketAddr) -> Result<TcpStream> {
    let stream = TcpStream::connect_timeout(&endpoint, Duration::from_secs(3))
        .context("paired computer unavailable")?;
    limits(&stream)?;
    Ok(stream)
}
pub(super) fn limits(stream: &TcpStream) -> Result<()> {
    // Accepted sockets can inherit the nonblocking listener mode on BSD/macOS.
    // Frame I/O uses blocking sockets with bounded kernel timeouts everywhere.
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_secs(3)))?;
    stream.set_write_timeout(Some(Duration::from_secs(3)))?;
    Ok(())
}

fn socket_timeout(error: std::io::Error) -> std::io::Error {
    // Linux reports SO_RCVTIMEO/SO_SNDTIMEO expiry as EAGAIN/WouldBlock.
    // Preserve the cause but expose the same timeout kind as other platforms so
    // transfer reconciliation can keep an offline peer pending safely.
    if error.kind() == std::io::ErrorKind::WouldBlock {
        std::io::Error::new(std::io::ErrorKind::TimedOut, error)
    } else {
        error
    }
}

pub(super) fn approval_wait_interrupted(error: &anyhow::Error, waited: Duration) -> bool {
    error.downcast_ref::<std::io::Error>().is_some_and(|error| {
        matches!(error.kind(),std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock)
            // A fast close is also how an inviter refuses a different target
            // or a second identity claiming its invitation. Preserve that error.
            // Older peers can instead close at their three-second frame deadline
            // while the owner is still deciding; that remains a pending request.
            || waited>=Duration::from_secs(3) && matches!(error.kind(),std::io::ErrorKind::UnexpectedEof | std::io::ErrorKind::ConnectionReset)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::ser::SerializeSeq;
    use std::cell::Cell;

    #[test]
    fn approval_wait_retries_only_transient_socket_failures() {
        for kind in [
            std::io::ErrorKind::TimedOut,
            std::io::ErrorKind::WouldBlock,
            std::io::ErrorKind::UnexpectedEof,
            std::io::ErrorKind::ConnectionReset,
        ] {
            let error =
                anyhow::Error::new(std::io::Error::from(kind)).context("read pairing frame length");
            assert!(approval_wait_interrupted(&error, Duration::from_secs(3)));
        }
        assert!(!approval_wait_interrupted(
            &anyhow::anyhow!("pairing authentication failed"),
            Duration::from_secs(3)
        ));
        assert!(!approval_wait_interrupted(
            &anyhow::anyhow!("pairing frame exceeds bound"),
            Duration::from_secs(3)
        ));
        assert!(!approval_wait_interrupted(
            &anyhow::Error::new(std::io::Error::from(std::io::ErrorKind::UnexpectedEof)),
            Duration::ZERO
        ));
    }

    #[test]
    fn blocking_socket_timeout_is_recognized_as_an_offline_peer() -> Result<()> {
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let mut sender = connect(listener.local_addr()?)?;
        let (mut receiver, _) = listener.accept()?;
        receiver.set_nonblocking(true)?;
        limits(&receiver)?;
        #[cfg(unix)]
        assert!(!rustix::fs::fcntl_getfl(&receiver)?.contains(rustix::fs::OFlags::NONBLOCK));
        receiver.set_read_timeout(Some(Duration::from_millis(20)))?;
        // Positive control: the same socket and frame reader receive data.
        write(&mut sender, &"delivered")?;
        assert_eq!(read::<String>(&mut receiver)?, "delivered");
        // Keep the peer connected but send no next frame: exercise the real
        // kernel receive timeout, rather than a mocked WouldBlock value.
        let error = read::<String>(&mut receiver).unwrap_err();
        assert_eq!(
            error.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::TimedOut
        );
        assert!(super::super::transfer::offline(&error));
        Ok(())
    }

    struct Oversized<'a>(&'a Cell<usize>);
    impl Serialize for Oversized<'_> {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            let mut seq = serializer.serialize_seq(None)?;
            for _ in 0..MAX_FRAME * 4 {
                self.0.set(self.0.get() + 1);
                seq.serialize_element(&0u8)?;
            }
            seq.end()
        }
    }

    #[test]
    fn bounded_reads_reject_truncation_oversize_and_unknown_fields() -> Result<()> {
        fn input(bytes: &[u8]) -> Result<TcpStream> {
            use std::net::{Shutdown, TcpListener};
            let listener = TcpListener::bind("127.0.0.1:0")?;
            let mut sender = connect(listener.local_addr()?)?;
            let (receiver, _) = listener.accept()?;
            limits(&receiver)?;
            sender.write_all(bytes)?;
            sender.shutdown(Shutdown::Write)?;
            Ok(receiver)
        }
        let envelope = seal(Uuid::new_v4(), &[1; 32], &[2; 32], &"positive")?;
        let body = serde_json::to_vec(&envelope)?;
        let mut frame = (body.len() as u32).to_be_bytes().to_vec();
        frame.extend_from_slice(&body);
        let decoded: Envelope = read(&mut input(&frame)?)?;
        assert_eq!(decoded.scope, envelope.scope);
        assert!(read::<Envelope>(&mut input(&frame[..2])?).is_err());
        assert!(read::<Envelope>(&mut input(&frame[..frame.len() - 1])?).is_err());
        assert!(read::<Envelope>(&mut input(&((MAX_FRAME + 1) as u32).to_be_bytes())?).is_err());
        assert!(read::<Envelope>(&mut input(&0u32.to_be_bytes())?).is_err());
        let mut unknown = serde_json::to_value(&envelope)?;
        unknown["unexpected"] = serde_json::json!(true);
        let body = serde_json::to_vec(&unknown)?;
        let mut frame = (body.len() as u32).to_be_bytes().to_vec();
        frame.extend_from_slice(&body);
        assert!(read::<Envelope>(&mut input(&frame)?).is_err());
        Ok(())
    }
    #[test]
    fn serialization_stops_before_visiting_an_oversized_value() {
        let visited = Cell::new(0);
        let error = bounded_json(&Oversized(&visited), MAX_FRAME).unwrap_err();
        assert!(error.to_string().contains("pairing frame exceeds bound"));
        assert!(
            visited.get() > 0,
            "positive control: serializer was invoked"
        );
        assert!(visited.get() <= MAX_FRAME / 2 + 1);

        visited.set(0);
        assert!(seal(Uuid::nil(), &[1; 32], &[2; 32], &Oversized(&visited)).is_err());
        assert!(visited.get() > 0);
        assert!(visited.get() <= MAX_FRAME / 2 + 1);
    }

    #[test]
    fn bounded_serialization_accepts_exact_limit_and_rejects_next_byte() -> Result<()> {
        let value = "pairing";
        let expected = serde_json::to_vec(value)?;
        assert_eq!(bounded_json(&value, expected.len())?, expected);
        assert!(bounded_json(&value, expected.len() - 1).is_err());
        Ok(())
    }
}

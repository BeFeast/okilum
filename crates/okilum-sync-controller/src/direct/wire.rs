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
    let body = serde_json::to_vec(body)?;
    let pair = key(seed)?;
    let signed = Signed {
        signature: pair.sign(&body).as_ref().to_vec(),
        signer: pair.public_key().as_ref().to_vec(),
        body,
    };
    let mut body = serde_json::to_vec(&signed)?;
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
    stream.read_exact(&mut length)?;
    let length = u32::from_be_bytes(length) as usize;
    ensure!(
        length > 0 && length <= MAX_FRAME,
        "pairing frame exceeds bound"
    );
    let mut body = vec![0; length];
    stream.read_exact(&mut body)?;
    Ok(serde_json::from_slice(&body)?)
}
pub(super) fn write<T: Serialize>(stream: &mut TcpStream, value: &T) -> Result<()> {
    let body = serde_json::to_vec(value)?;
    ensure!(body.len() <= MAX_FRAME, "pairing frame exceeds bound");
    stream.write_all(&(body.len() as u32).to_be_bytes())?;
    stream.write_all(&body)?;
    stream.flush()?;
    Ok(())
}
pub(super) fn connect(endpoint: SocketAddr) -> Result<TcpStream> {
    let stream = TcpStream::connect_timeout(&endpoint, Duration::from_secs(3))
        .context("paired computer unavailable")?;
    limits(&stream)?;
    Ok(stream)
}
pub(super) fn limits(stream: &TcpStream) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(3)))?;
    stream.set_write_timeout(Some(Duration::from_secs(3)))?;
    Ok(())
}

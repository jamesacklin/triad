//! Long messages: text over [`SEND_TEXT_BYTES`], up to [`MAX_TEXT_BYTES`].
//!
//! p2panda-core 0.8.0 reads an encrypted space message back only up to about
//! 460 bytes of text, because spaces carry the ciphertext in the operation
//! header and headers are decoded with a 512-byte limit (see
//! [`SEND_TEXT_BYTES`]). Operation bodies have no such limit. So a long
//! message travels in two parts:
//!
//! 1. The text, encrypted with a fresh random key (ChaCha20-Poly1305), is
//!    published as the body of an operation on a plain p2panda stream beside
//!    the space ([`stream_topic`]). The payload is [`LongBody`].
//! 2. An ordinary, short chat message in the space carries a [`LongRef`]:
//!    the key and the ciphertext's hash. Spaces encrypt it for members only.
//!
//! Anyone who syncs the plain stream sees ciphertext, its size and when it
//! was published, but only members get the key. A member removed before a
//! long message was sent never gets that message's key.
//!
//! [`SEND_TEXT_BYTES`]: crate::envelope::SEND_TEXT_BYTES

use anyhow::{Result, anyhow, ensure};
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use serde::{Deserialize, Serialize};

use crate::envelope::{MAX_DM_TEXT_BYTES, MAX_TEXT_BYTES, VERSION};

/// Largest ciphertext taken from a plain stream: the longest text plus the
/// 16-byte tag. Larger bodies are ignored.
pub const MAX_CIPHERTEXT_BYTES: usize = MAX_TEXT_BYTES + 16;
/// The same for the personal bot's DM ([`MAX_DM_TEXT_BYTES`]).
pub const MAX_DM_CIPHERTEXT_BYTES: usize = MAX_DM_TEXT_BYTES + 16;

/// The `text` of a long message's space message, shown by builds that do not
/// know long messages.
pub const FALLBACK_TEXT: &str = "(a long message; this version of triad cannot show it)";

/// Points from a space message to its long text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LongRef {
    /// BLAKE3 hash of the ciphertext ([`LongBody::ciphertext`]).
    #[serde(with = "serde_bytes")]
    pub hash: [u8; 32],
    /// The key the text was encrypted with, used for this message only.
    #[serde(with = "serde_bytes")]
    pub key: [u8; 32],
    /// Length of the text in bytes.
    pub len: u32,
}

/// The payload of an operation on the plain stream: ciphertext only.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LongBody {
    pub v: u8,
    #[serde(with = "serde_bytes")]
    pub ciphertext: Vec<u8>,
}

// Each key encrypts exactly one text, so a fixed nonce is safe.
const NONCE: [u8; 12] = [0; 12];

fn cipher(key: &[u8; 32]) -> ChaCha20Poly1305 {
    ChaCha20Poly1305::new(Key::from_slice(key))
}

pub fn hash_of(bytes: &[u8]) -> [u8; 32] {
    *p2panda_core::Hash::digest(bytes).as_bytes()
}

/// The topic of the plain stream beside a space.
pub fn stream_topic(space: &[u8; 32]) -> [u8; 32] {
    hash_of(&[b"triad-long-v1".as_slice(), space.as_slice()].concat())
}

/// Encrypts `text` under a fresh key.
pub fn seal(text: &str) -> Result<(LongRef, LongBody)> {
    seal_up_to(text, MAX_TEXT_BYTES)
}

/// [`seal`] with a limit of `max` bytes.
pub fn seal_up_to(text: &str, max: usize) -> Result<(LongRef, LongBody)> {
    ensure!(
        text.len() <= max,
        "message is {} bytes, limit is {max}",
        text.len()
    );
    let mut key = [0u8; 32];
    getrandom::fill(&mut key).map_err(|e| anyhow!("no randomness for a message key: {e}"))?;
    let ciphertext = cipher(&key)
        .encrypt(Nonce::from_slice(&NONCE), text.as_bytes())
        .map_err(|_| anyhow!("encrypting a long message failed"))?;
    let long = LongRef {
        hash: hash_of(&ciphertext),
        key,
        len: text.len() as u32,
    };
    Ok((
        long,
        LongBody {
            v: VERSION,
            ciphertext,
        },
    ))
}

/// Checks `ciphertext` against `long` and decrypts it.
pub fn open(long: &LongRef, ciphertext: &[u8]) -> Result<String> {
    open_up_to(long, ciphertext, MAX_TEXT_BYTES)
}

/// [`open`] with a limit of `max` bytes.
pub fn open_up_to(long: &LongRef, ciphertext: &[u8], max: usize) -> Result<String> {
    ensure!(
        hash_of(ciphertext) == long.hash,
        "long message body does not match its hash"
    );
    let plain = cipher(&long.key)
        .decrypt(Nonce::from_slice(&NONCE), ciphertext)
        .map_err(|_| anyhow!("cannot decrypt a long message"))?;
    ensure!(
        plain.len() == long.len as usize && plain.len() <= max,
        "long message is {} bytes, expected {}",
        plain.len(),
        long.len
    );
    Ok(String::from_utf8(plain)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::cbor_encode;

    #[test]
    fn seal_and_open() {
        let text = "é".repeat(900);
        let (long, body) = seal(&text).unwrap();
        assert_eq!(open(&long, &body.ciphertext).unwrap(), text);
        assert!(!body.ciphertext.windows(4).any(|w| w == "éé".as_bytes()));

        // A wrong body, a wrong key or a tampered body is refused.
        let (other, other_body) = seal(&text).unwrap();
        assert_ne!(other.key, long.key);
        assert!(open(&long, &other_body.ciphertext).is_err());
        let wrong_key = LongRef {
            key: other.key,
            ..long.clone()
        };
        assert!(open(&wrong_key, &body.ciphertext).is_err());
        let mut tampered = body.ciphertext.clone();
        tampered[0] ^= 1;
        assert!(open(&long, &tampered).is_err());

        assert!(seal(&"x".repeat(MAX_TEXT_BYTES + 1)).is_err());
        // The DM's longer limit, and only where it is asked for.
        let dm = "y".repeat(MAX_DM_TEXT_BYTES);
        let (long, body) = seal_up_to(&dm, MAX_DM_TEXT_BYTES).unwrap();
        assert!(open(&long, &body.ciphertext).is_err());
        assert_eq!(
            open_up_to(&long, &body.ciphertext, MAX_DM_TEXT_BYTES).unwrap(),
            dm
        );
        assert!(seal_up_to(&format!("{dm}y"), MAX_DM_TEXT_BYTES).is_err());
    }

    #[test]
    fn sizes_on_the_wire() {
        // The reference rides in a space message; keep it small.
        let (long, body) = seal(&"x".repeat(MAX_TEXT_BYTES)).unwrap();
        assert!(cbor_encode(&long).unwrap().len() < 90);
        let pointer = crate::envelope::ChatBody::new("x", u64::MAX)
            .unwrap()
            .asking(true)
            .into_long(long.clone());
        assert!(pointer.encode().unwrap().len() < 200);
        // Bytes, not an array of integers: the body stays near the text's
        // size, well under p2panda's 4096-byte gossip message limit.
        let wire = cbor_encode(&body).unwrap();
        assert!(wire.len() <= MAX_CIPHERTEXT_BYTES + 32, "{}", wire.len());
        assert_ne!(stream_topic(&[1; 32]), stream_topic(&[2; 32]));
    }
}

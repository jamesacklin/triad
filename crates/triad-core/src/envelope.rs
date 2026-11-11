//! Wire formats: [`Envelope`] (one per p2panda operation), [`ChatBody`] (the
//! plaintext inside an MLS application message) and [`Invite`].

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_bytes::ByteBuf;

/// Wire format version used everywhere.
pub const VERSION: u8 = 1;
/// Maximum UTF-8 text length of one chat message.
pub const MAX_TEXT_BYTES: usize = 2_000;
/// Every encoded envelope must stay under this size.
pub const MAX_ENVELOPE_BYTES: usize = 3_500;
/// Invite prefix.
pub const INVITE_PREFIX: &str = "triad1-";

/// One p2panda operation on the group topic holds exactly one envelope.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Envelope {
    /// Joiner to creator. Public. An MLS KeyPackage.
    KeyPackage { v: u8, key_package: ByteBuf },
    /// Creator to one joiner. An MLS Welcome, addressed by node ID.
    Welcome {
        v: u8,
        to: [u8; 32],
        welcome: ByteBuf,
    },
    /// Creator to existing members. An MLS Commit. Used from 3 members up.
    Commit { v: u8, commit: ByteBuf },
    /// Any member to the group. An MLS PrivateMessage.
    App { v: u8, ciphertext: ByteBuf },
}

impl Envelope {
    pub fn key_package(bytes: Vec<u8>) -> Self {
        Envelope::KeyPackage {
            v: VERSION,
            key_package: ByteBuf::from(bytes),
        }
    }

    pub fn welcome(to: [u8; 32], bytes: Vec<u8>) -> Self {
        Envelope::Welcome {
            v: VERSION,
            to,
            welcome: ByteBuf::from(bytes),
        }
    }

    pub fn commit(bytes: Vec<u8>) -> Self {
        Envelope::Commit {
            v: VERSION,
            commit: ByteBuf::from(bytes),
        }
    }

    pub fn app(bytes: Vec<u8>) -> Self {
        Envelope::App {
            v: VERSION,
            ciphertext: ByteBuf::from(bytes),
        }
    }

    pub fn version(&self) -> u8 {
        match self {
            Envelope::KeyPackage { v, .. }
            | Envelope::Welcome { v, .. }
            | Envelope::Commit { v, .. }
            | Envelope::App { v, .. } => *v,
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Envelope::KeyPackage { .. } => "key_package",
            Envelope::Welcome { .. } => "welcome",
            Envelope::Commit { .. } => "commit",
            Envelope::App { .. } => "app",
        }
    }

    /// CBOR-encoded size, as p2panda will serialize it.
    pub fn encoded_len(&self) -> usize {
        cbor_encode(self).map(|b| b.len()).unwrap_or(usize::MAX)
    }

    /// Refuses envelopes over [`MAX_ENVELOPE_BYTES`].
    pub fn check_size(&self) -> Result<()> {
        let len = self.encoded_len();
        if len >= MAX_ENVELOPE_BYTES {
            bail!(
                "{} envelope is {len} bytes, limit is {MAX_ENVELOPE_BYTES}",
                self.kind()
            );
        }
        Ok(())
    }
}

/// Plaintext inside the MLS application message, CBOR encoded.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatBody {
    pub v: u8,
    pub sent_at_ms: u64,
    pub text: String,
}

impl ChatBody {
    pub fn new(text: &str, sent_at_ms: u64) -> Result<Self> {
        if text.len() > MAX_TEXT_BYTES {
            bail!("message is {} bytes, limit is {MAX_TEXT_BYTES}", text.len());
        }
        Ok(ChatBody {
            v: VERSION,
            sent_at_ms,
            text: text.to_string(),
        })
    }

    pub fn encode(&self) -> Result<Vec<u8>> {
        cbor_encode(self)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let body: ChatBody = cbor_decode(bytes)?;
        if body.v != VERSION {
            bail!("unsupported chat body version {}", body.v);
        }
        if body.text.len() > MAX_TEXT_BYTES {
            bail!(
                "message is {} bytes, limit is {MAX_TEXT_BYTES}",
                body.text.len()
            );
        }
        Ok(body)
    }
}

/// Invite. Rendered as "triad1-" + lowercase hex of the CBOR bytes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Invite {
    pub v: u8,
    pub topic: [u8; 32],
    pub creator: [u8; 32],
    pub relay: String,
}

impl Invite {
    pub fn new(topic: [u8; 32], creator: [u8; 32], relay: &str) -> Self {
        Invite {
            v: VERSION,
            topic,
            creator,
            relay: relay.to_string(),
        }
    }

    pub fn render(&self) -> Result<String> {
        Ok(format!(
            "{INVITE_PREFIX}{}",
            hex::encode(cbor_encode(self)?)
        ))
    }

    pub fn parse(s: &str) -> Result<Self> {
        let hex_part = s
            .trim()
            .strip_prefix(INVITE_PREFIX)
            .ok_or_else(|| anyhow!("invite must start with {INVITE_PREFIX}"))?;
        let bytes = hex::decode(hex_part).context("invite is not valid hex")?;
        let invite: Invite = cbor_decode(&bytes).context("invite is not valid CBOR")?;
        if invite.v != VERSION {
            bail!("unsupported invite version {}", invite.v);
        }
        Ok(invite)
    }
}

pub fn cbor_encode<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    ciborium::into_writer(value, &mut out)?;
    Ok(out)
}

pub fn cbor_decode<T: for<'de> Deserialize<'de>>(bytes: &[u8]) -> Result<T> {
    let mut remaining = bytes;
    let value = ciborium::from_reader(&mut remaining)?;
    if !remaining.is_empty() {
        bail!("trailing bytes after CBOR value");
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invite_round_trip() {
        let invite = Invite::new([1; 32], [2; 32], "https://relay.example./");
        let s = invite.render().unwrap();
        assert!(s.starts_with(INVITE_PREFIX));
        assert_eq!(s, s.to_lowercase());
        assert_eq!(Invite::parse(&s).unwrap(), invite);
        assert!(Invite::parse("nope").is_err());
    }

    #[test]
    fn chat_body_round_trip_and_cap() {
        let body = ChatBody::new("hello", 42).unwrap();
        assert_eq!(ChatBody::decode(&body.encode().unwrap()).unwrap(), body);
        assert!(ChatBody::new(&"x".repeat(MAX_TEXT_BYTES), 0).is_ok());
        assert!(ChatBody::new(&"x".repeat(MAX_TEXT_BYTES + 1), 0).is_err());
    }

    #[test]
    fn received_body_enforces_utf8_byte_limit() {
        let mut body = ChatBody::new(&"é".repeat(1_000), 0).unwrap();
        assert!(ChatBody::decode(&body.encode().unwrap()).is_ok());
        body.text.push('é');
        assert!(ChatBody::decode(&body.encode().unwrap()).is_err());
    }

    #[test]
    fn rejects_trailing_cbor_data() {
        let mut bytes = ChatBody::new("hello", 0).unwrap().encode().unwrap();
        bytes.push(0);
        assert!(ChatBody::decode(&bytes).is_err());
        let invite = Invite::new([1; 32], [2; 32], "https://relay.example/");
        assert!(Invite::parse(&(invite.render().unwrap() + "00")).is_err());
    }

    #[test]
    fn envelope_round_trip_and_size() {
        let env = Envelope::app(vec![7; 100]);
        let bytes = cbor_encode(&env).unwrap();
        assert_eq!(cbor_decode::<Envelope>(&bytes).unwrap(), env);
        assert!(env.check_size().is_ok());
        assert!(
            Envelope::app(vec![0; MAX_ENVELOPE_BYTES])
                .check_size()
                .is_err()
        );
    }
}

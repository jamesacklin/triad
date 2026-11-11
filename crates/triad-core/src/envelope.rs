//! Wire formats: [`ChatBody`] (one chat message, the payload of a space
//! operation) and [`Invite`].

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};

/// Wire format version used everywhere.
pub const VERSION: u8 = 1;
/// Maximum UTF-8 text length of one chat message.
pub const MAX_TEXT_BYTES: usize = 2_000;
/// Invite prefix.
pub const INVITE_PREFIX: &str = "triad1-";

/// One chat message, CBOR encoded. p2panda encrypts it as a whole.
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
    /// The channel name the inviter uses. A suggestion: the joiner may pick
    /// another. Invites written before channels had names carry none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl Invite {
    pub fn new(topic: [u8; 32], creator: [u8; 32], relay: &str) -> Self {
        Invite {
            v: VERSION,
            topic,
            creator,
            relay: relay.to_string(),
            name: None,
        }
    }

    pub fn with_name(mut self, name: &str) -> Self {
        self.name = Some(name.to_string());
        self
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
    fn invite_name_is_optional() {
        let plain = Invite::new([1; 32], [2; 32], "https://relay.example./");
        let named = plain.clone().with_name("general");
        let parsed = Invite::parse(&named.render().unwrap()).unwrap();
        assert_eq!(parsed.name.as_deref(), Some("general"));
        // A nameless invite keeps the bytes it had before names existed.
        assert!(named.render().unwrap().len() > plain.render().unwrap().len());
        assert_eq!(Invite::parse(&plain.render().unwrap()).unwrap().name, None);
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
}

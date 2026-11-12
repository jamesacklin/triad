//! Wire formats for room events and invites.

use anyhow::{Context, Result, anyhow, bail};
use p2panda_core::{Signature, VerifyingKey, identity::Signer};
use serde::{Deserialize, Serialize};

use crate::identity::{Identity, NodeId};
use crate::long::LongRef;

/// Wire format version used everywhere.
pub const VERSION: u8 = 1;
/// Maximum UTF-8 text length of one chat message, in the wire format.
pub const MAX_TEXT_BYTES: usize = 2_000;
/// Longest text sent as one space message. p2panda-core 0.8.0 decodes
/// operation headers with a 512-byte limit on any byte string, and spaces
/// carry the encrypted message in the header. A message over about 460 bytes
/// of text is published but can never be read back, which wedges the
/// author's log in that space: no later message from them is published or
/// synced. 400 leaves a margin. Longer text, up to [`MAX_TEXT_BYTES`], goes
/// as a long message ([`crate::long`]). See FINDINGS.md, "Bots", H7.
pub const SEND_TEXT_BYTES: usize = 400;
const MAX_NICKNAME_CHARS: usize = 24;
/// Invite prefix.
pub const INVITE_PREFIX: &str = "triad1-";

/// One chat message, CBOR encoded. p2panda encrypts it as a whole.
///
/// The bot fields are optional and skipped when empty, so a plain message
/// encodes to the bytes it had before bots existed, and an older build
/// ignores the fields it does not know.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatBody {
    pub v: u8,
    pub sent_at_ms: u64,
    pub text: String,
    #[serde(default, skip_serializing)]
    nickname: Option<String>,
    /// True when the author's bot wrote this, not the author.
    #[serde(default, skip_serializing_if = "is_false")]
    pub bot: bool,
    /// True when this message addresses the channel's bot.
    #[serde(default, skip_serializing_if = "is_false")]
    pub ask: bool,
    /// An announcement. `text` holds a readable fallback for old builds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ctl: Option<Control>,
    /// A long message: the text travels encrypted beside the space (see
    /// [`crate::long`]), and `text` holds [`crate::long::FALLBACK_TEXT`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub long: Option<LongRef>,
}

/// A channel announcement about its bot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Control {
    /// I run this channel's bot. Shown to members: where their messages go,
    /// and the names of the MCP servers the bot can reach.
    BotOn {
        provider: String,
        servers: Vec<String>,
    },
    /// This channel's bot is off.
    BotOff,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// Cuts `text` to at most `max` bytes at a character boundary. Text that
/// had to be cut ends with `…`.
pub fn truncate_text(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let ellipsis = '…';
    let mut end = max.saturating_sub(ellipsis.len_utf8());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = text[..end].to_string();
    out.push(ellipsis);
    out
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
            bot: false,
            ask: false,
            ctl: None,
            long: None,
            nickname: None,
        })
    }

    /// Marks this message as written by the author's bot.
    pub fn from_bot(mut self) -> Self {
        self.bot = true;
        self
    }

    /// Marks this message as addressed to the channel's bot.
    pub fn asking(mut self, ask: bool) -> Self {
        self.ask = ask;
        self
    }

    /// Makes this message an announcement.
    pub fn with_ctl(mut self, ctl: Control) -> Self {
        self.ctl = Some(ctl);
        self
    }

    /// Turns this message into a pointer to its long text, sealed in
    /// `long`: `text` becomes [`crate::long::FALLBACK_TEXT`].
    pub fn into_long(mut self, long: LongRef) -> Self {
        self.text = crate::long::FALLBACK_TEXT.to_string();
        self.long = Some(long);
        self
    }

    /// Puts the opened long text back in place of the fallback.
    pub fn with_text(mut self, text: String) -> Self {
        self.text = text;
        self
    }

    pub fn nickname(&self) -> Option<&str> {
        normalize_nickname(self.nickname.as_deref()?).ok().flatten()
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RoomEvent {
    Profile(ProfileUpdate),
    Chat(ChatBody),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileUpdate {
    v: u8,
    profile: String,
    author: NodeId,
    revision: u64,
    signature: Vec<u8>,
}

impl ProfileUpdate {
    pub fn sign(
        identity: &Identity,
        topic: &[u8; 32],
        revision: u64,
        nickname: &str,
    ) -> Result<Self> {
        let profile = normalize_nickname(nickname)?.unwrap_or_default().to_owned();
        let author = identity.node_id();
        let bytes = profile_bytes(VERSION, topic, &author, revision, &profile)?;
        Ok(Self {
            v: VERSION,
            profile,
            author,
            revision,
            signature: identity
                .p2panda_credentials()
                .sign(&bytes)
                .to_bytes()
                .to_vec(),
        })
    }

    pub fn verify(&self, topic: &[u8; 32]) -> Result<(NodeId, u64, &str)> {
        if self.v != VERSION {
            bail!("unsupported profile version {}", self.v);
        }
        let name = normalize_nickname(&self.profile)?.unwrap_or_default();
        let bytes = profile_bytes(self.v, topic, &self.author, self.revision, name)?;
        let signature = Signature::try_from(self.signature.as_slice())?;
        if !VerifyingKey::from_bytes(&self.author)?.verify(&bytes, &signature) {
            bail!("invalid profile signature");
        }
        Ok((self.author, self.revision, name))
    }
}

fn profile_bytes(
    v: u8,
    topic: &[u8; 32],
    author: &NodeId,
    revision: u64,
    nickname: &str,
) -> Result<Vec<u8>> {
    // Spaces can deliver several buffered payloads with one outer operation's metadata.
    cbor_encode(&("triad-room-profile", v, topic, author, revision, nickname))
}

pub fn normalize_nickname(raw: &str) -> Result<Option<&str>> {
    let name = raw.trim();
    if name.chars().count() > MAX_NICKNAME_CHARS
        || name
            .chars()
            .any(|c| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}'))
    {
        bail!("nickname must be one line and at most {MAX_NICKNAME_CHARS} characters");
    }
    Ok((!name.is_empty()).then_some(name))
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
    fn room_events_keep_the_original_chat_format() {
        let chat = ChatBody::new("hi", 42).unwrap();
        let chat_wire = cbor_encode(&RoomEvent::Chat(chat.clone())).unwrap();
        assert_eq!(ChatBody::decode(&chat_wire).unwrap(), chat);
        assert_eq!(
            cbor_decode::<RoomEvent>(&chat_wire).unwrap(),
            RoomEvent::Chat(chat)
        );
        let dir = tempfile::tempdir().unwrap();
        let identity = Identity::load_or_create(dir.path()).unwrap();
        let topic = [4; 32];
        let profile = ProfileUpdate::sign(&identity, &topic, 2, "  Renée  ").unwrap();
        assert_eq!(
            profile.verify(&topic).unwrap(),
            (identity.node_id(), 2, "Renée")
        );
        assert!(profile.verify(&[5; 32]).is_err());
        let mut tampered = profile.clone();
        tampered.profile = "Someone else".into();
        assert!(tampered.verify(&topic).is_err());
        let profile_wire = cbor_encode(&RoomEvent::Profile(profile.clone())).unwrap();
        assert_eq!(
            cbor_decode::<RoomEvent>(&profile_wire).unwrap(),
            RoomEvent::Profile(profile)
        );
        assert!(ChatBody::decode(&profile_wire).is_err());
        assert!(ProfileUpdate::sign(&identity, &topic, 3, "two\nlines").is_err());
    }

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

    /// `ChatBody` as it was before the bot fields.
    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct ChatBodyV1 {
        v: u8,
        sent_at_ms: u64,
        text: String,
    }

    #[test]
    fn plain_body_keeps_todays_bytes() {
        let old = ChatBodyV1 {
            v: VERSION,
            sent_at_ms: 1_700_000_000_123,
            text: "hello".into(),
        };
        let new = ChatBody::new("hello", 1_700_000_000_123).unwrap();
        assert_eq!(new.encode().unwrap(), cbor_encode(&old).unwrap());
        // And today's bytes decode as a plain new body.
        assert_eq!(ChatBody::decode(&cbor_encode(&old).unwrap()).unwrap(), new);
    }

    #[test]
    fn old_build_decodes_a_body_with_bot_fields() {
        let new = ChatBody::new("I run this bot", 7)
            .unwrap()
            .from_bot()
            .asking(true)
            .with_ctl(Control::BotOn {
                provider: "anthropic".into(),
                servers: vec!["calendar".into()],
            });
        let bytes = new.encode().unwrap();
        let old: ChatBodyV1 = cbor_decode(&bytes).unwrap();
        assert_eq!(old.text, "I run this bot");
        assert_eq!(old.sent_at_ms, 7);
        assert_eq!(ChatBody::decode(&bytes).unwrap(), new);
        let off = ChatBody::new("off", 1).unwrap().with_ctl(Control::BotOff);
        assert_eq!(ChatBody::decode(&off.encode().unwrap()).unwrap(), off);
    }

    #[test]
    fn truncates_at_a_character_boundary() {
        assert_eq!(truncate_text("short", 10), "short");
        let cut = truncate_text(&"é".repeat(10), 9);
        assert!(cut.len() <= 9, "{cut}");
        assert!(cut.ends_with('…'));
        assert_eq!(cut, "ééé…");
        let long = truncate_text(&"x".repeat(MAX_TEXT_BYTES + 50), MAX_TEXT_BYTES);
        assert!(long.len() <= MAX_TEXT_BYTES && long.ends_with('…'));
        assert!(ChatBody::new(&long, 0).is_ok());
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

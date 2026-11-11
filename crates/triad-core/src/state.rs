//! `state.sqlite`: group metadata, processed operation IDs and decrypted
//! message history. Owned by the app, not by p2panda.
//!
//! One data directory holds any number of groups (channels). Each has a
//! locally unique name, and every operation and message is filed under its
//! group's topic.

use std::path::Path;

use anyhow::{Result, anyhow, bail};
use rusqlite::{Connection, OptionalExtension, params};

use crate::identity::NodeId;

pub const STATE_FILE: &str = "state.sqlite";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Creator,
    Member,
}

impl Role {
    pub fn as_str(&self) -> &'static str {
        match self {
            Role::Creator => "creator",
            Role::Member => "member",
        }
    }

    fn parse(s: &str) -> Result<Self> {
        match s {
            "creator" => Ok(Role::Creator),
            "member" => Ok(Role::Member),
            other => bail!("unknown role {other}"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JoinStatus {
    None,
    Pending,
    Joined,
}

impl JoinStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            JoinStatus::None => "none",
            JoinStatus::Pending => "pending",
            JoinStatus::Joined => "joined",
        }
    }

    fn parse(s: &str) -> Result<Self> {
        match s {
            "none" => Ok(JoinStatus::None),
            "pending" => Ok(JoinStatus::Pending),
            "joined" => Ok(JoinStatus::Joined),
            other => bail!("unknown join status {other}"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Ok,
    Skipped,
    Failed,
}

impl Outcome {
    pub fn as_str(&self) -> &'static str {
        match self {
            Outcome::Ok => "ok",
            Outcome::Skipped => "skipped",
            Outcome::Failed => "failed",
        }
    }

    fn parse(s: &str) -> Result<Self> {
        match s {
            "ok" => Ok(Outcome::Ok),
            "skipped" => Ok(Outcome::Skipped),
            "failed" => Ok(Outcome::Failed),
            other => bail!("unknown outcome {other}"),
        }
    }
}

/// Longest channel name, in bytes.
pub const MAX_NAME_BYTES: usize = 32;

/// Normalizes a channel name: an optional leading `#` is dropped, and the
/// rest must be 1 to 32 characters of `a-z`, `0-9`, `-` or `_`.
pub fn channel_name(raw: &str) -> Result<String> {
    let name = raw.trim().trim_start_matches('#').to_ascii_lowercase();
    if name.is_empty() || name.len() > MAX_NAME_BYTES {
        bail!("channel name must be 1 to {MAX_NAME_BYTES} characters");
    }
    if !name
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
    {
        bail!("channel name may only contain a-z, 0-9, '-' and '_'");
    }
    Ok(name)
}

/// One group (channel) in this data directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Meta {
    /// Local name of the channel, unique in this data directory.
    pub name: String,
    /// The space ID.
    pub topic: [u8; 32],
    /// The node this one reaches the channel through: its creator, or
    /// whoever handed out the invite.
    pub creator: NodeId,
    pub relay: String,
    pub role: Role,
    pub status: JoinStatus,
}

/// A decrypted chat message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredMessage {
    pub op_hash: [u8; 32],
    pub author: NodeId,
    pub sent_at_ms: u64,
    /// p2panda operation timestamp (microseconds), once known.
    pub op_ts: Option<u64>,
    pub text: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OutcomeCounts {
    pub ok: u64,
    pub skipped: u64,
    pub failed: u64,
}

pub struct State {
    conn: Connection,
}

impl State {
    pub fn open(data_dir: &Path) -> Result<Self> {
        Self::open_path(&data_dir.join(STATE_FILE))
    }

    pub fn open_path(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE IF NOT EXISTS groups (
                 topic BLOB PRIMARY KEY,
                 name TEXT NOT NULL UNIQUE,
                 creator BLOB NOT NULL,
                 relay TEXT NOT NULL,
                 role TEXT NOT NULL,
                 status TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS processed_ops (
                 op_hash BLOB PRIMARY KEY,
                 outcome TEXT NOT NULL,
                 reason TEXT,
                 recorded_at_ms INTEGER NOT NULL,
                 topic BLOB
             );
             CREATE TABLE IF NOT EXISTS messages (
                 op_hash BLOB PRIMARY KEY,
                 author BLOB NOT NULL,
                 sent_at_ms INTEGER NOT NULL,
                 op_ts INTEGER,
                 text TEXT NOT NULL,
                 topic BLOB
             );",
        )?;
        migrate_single_group(&conn)?;
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS messages_topic ON messages (topic);
             CREATE INDEX IF NOT EXISTS processed_ops_topic ON processed_ops (topic);",
        )?;
        Ok(State { conn })
    }

    /// Every group in this data directory, in the order they were added.
    pub fn groups(&self) -> Result<Vec<Meta>> {
        let mut stmt = self.conn.prepare(
            "SELECT name, topic, creator, relay, role, status FROM groups ORDER BY rowid",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Vec<u8>>(1)?,
                r.get::<_, Vec<u8>>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (name, topic, creator, relay, role, status) = row?;
            out.push(Meta {
                name,
                topic: to_32(&topic)?,
                creator: to_32(&creator)?,
                relay,
                role: Role::parse(&role)?,
                status: JoinStatus::parse(&status)?,
            });
        }
        Ok(out)
    }

    /// The group with this name (a leading `#` is accepted).
    pub fn group_named(&self, name: &str) -> Result<Option<Meta>> {
        let name = channel_name(name)?;
        Ok(self.groups()?.into_iter().find(|g| g.name == name))
    }

    /// The first group added, the default for commands given no name.
    pub fn meta(&self) -> Result<Option<Meta>> {
        Ok(self.groups()?.into_iter().next())
    }

    /// Adds a group. Fails if its topic or name is already taken.
    pub fn insert_group(&self, meta: &Meta) -> Result<()> {
        let name = channel_name(&meta.name)?;
        if name != meta.name {
            bail!("channel name {:?} is not normalized", meta.name);
        }
        for g in self.groups()? {
            if g.topic == meta.topic {
                bail!("already in this channel, as #{}", g.name);
            }
            if g.name == meta.name {
                bail!("a channel named #{} already exists", g.name);
            }
        }
        self.conn.execute(
            "INSERT INTO groups (topic, name, creator, relay, role, status)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                meta.topic.as_slice(),
                meta.name,
                meta.creator.as_slice(),
                meta.relay,
                meta.role.as_str(),
                meta.status.as_str(),
            ],
        )?;
        Ok(())
    }

    pub fn set_joined(&self, topic: &[u8; 32]) -> Result<()> {
        self.conn.execute(
            "UPDATE groups SET status = 'joined' WHERE topic = ?1",
            params![topic.as_slice()],
        )?;
        Ok(())
    }

    pub fn outcome(&self, op_hash: &[u8; 32]) -> Result<Option<Outcome>> {
        let s: Option<String> = self
            .conn
            .query_row(
                "SELECT outcome FROM processed_ops WHERE op_hash = ?1",
                params![op_hash.as_slice()],
                |r| r.get(0),
            )
            .optional()?;
        s.map(|s| Outcome::parse(&s)).transpose()
    }

    /// Records an operation's outcome and, optionally, its decrypted message,
    /// under the group's topic, in one transaction. Re-recording the same
    /// hash is a no-op.
    pub fn record(
        &mut self,
        topic: &[u8; 32],
        op_hash: &[u8; 32],
        outcome: Outcome,
        reason: Option<&str>,
        message: Option<&StoredMessage>,
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        if let Some(m) = message {
            tx.execute(
                "INSERT OR IGNORE INTO messages (op_hash, author, sent_at_ms, op_ts, text, topic)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    m.op_hash.as_slice(),
                    m.author.as_slice(),
                    m.sent_at_ms as i64,
                    m.op_ts.map(|t| t as i64),
                    m.text,
                    topic.as_slice(),
                ],
            )?;
        }
        tx.execute(
            "INSERT OR IGNORE INTO processed_ops (op_hash, outcome, reason, recorded_at_ms, topic)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                op_hash.as_slice(),
                outcome.as_str(),
                reason,
                now_ms() as i64,
                topic.as_slice(),
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Fills in the p2panda timestamp of a message once its operation is seen.
    pub fn set_op_ts(&self, op_hash: &[u8; 32], op_ts: u64) -> Result<()> {
        self.conn.execute(
            "UPDATE messages SET op_ts = ?2 WHERE op_hash = ?1 AND op_ts IS NULL",
            params![op_hash.as_slice(), op_ts as i64],
        )?;
        Ok(())
    }

    /// One group's stored messages, oldest first, sorted by p2panda operation
    /// timestamp, then operation hash. With a limit, returns the newest
    /// `limit` messages.
    pub fn history(&self, topic: &[u8; 32], limit: Option<usize>) -> Result<Vec<StoredMessage>> {
        let mut stmt = self.conn.prepare(
            "SELECT op_hash, author, sent_at_ms, op_ts, text FROM messages
             WHERE topic = ?1
             ORDER BY COALESCE(op_ts, sent_at_ms * 1000) ASC, op_hash ASC",
        )?;
        let rows = stmt.query_map(params![topic.as_slice()], |r| {
            Ok((
                r.get::<_, Vec<u8>>(0)?,
                r.get::<_, Vec<u8>>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, Option<i64>>(3)?,
                r.get::<_, String>(4)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (h, a, sent, ts, text) = row?;
            out.push(StoredMessage {
                op_hash: to_32(&h)?,
                author: to_32(&a)?,
                sent_at_ms: sent as u64,
                op_ts: ts.map(|t| t as u64),
                text,
            });
        }
        if let Some(limit) = limit
            && out.len() > limit
        {
            out.drain(..out.len() - limit);
        }
        Ok(out)
    }

    /// Operation counts by outcome, for one group.
    pub fn counts(&self, topic: &[u8; 32]) -> Result<OutcomeCounts> {
        let mut counts = OutcomeCounts::default();
        let mut stmt = self.conn.prepare(
            "SELECT outcome, COUNT(*) FROM processed_ops WHERE topic = ?1 GROUP BY outcome",
        )?;
        let rows = stmt.query_map(params![topic.as_slice()], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        })?;
        for row in rows {
            let (outcome, n) = row?;
            match Outcome::parse(&outcome)? {
                Outcome::Ok => counts.ok = n as u64,
                Outcome::Skipped => counts.skipped = n as u64,
                Outcome::Failed => counts.failed = n as u64,
            }
        }
        Ok(counts)
    }
}

/// Moves a database written when a data directory held exactly one group
/// (a single `meta` row, no `topic` columns) to the multi-group layout. The
/// old group becomes the channel `main`.
fn migrate_single_group(conn: &Connection) -> Result<()> {
    let has_column = |table: &str, column: &str| -> Result<bool> {
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM pragma_table_info(?1) WHERE name = ?2",
            params![table, column],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    };
    for table in ["processed_ops", "messages"] {
        if !has_column(table, "topic")? {
            conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN topic BLOB;"))?;
        }
    }
    if !has_column("meta", "topic")? {
        return Ok(());
    }
    conn.execute_batch(
        "BEGIN;
         INSERT OR IGNORE INTO groups (topic, name, creator, relay, role, status)
             SELECT topic, 'main', creator, relay, role, status FROM meta;
         UPDATE processed_ops SET topic = (SELECT topic FROM meta) WHERE topic IS NULL;
         UPDATE messages SET topic = (SELECT topic FROM meta) WHERE topic IS NULL;
         DROP TABLE meta;
         COMMIT;",
    )?;
    Ok(())
}

fn to_32(bytes: &[u8]) -> Result<[u8; 32]> {
    bytes
        .try_into()
        .map_err(|_| anyhow!("expected 32 bytes, found {}", bytes.len()))
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(name: &str, topic: u8) -> Meta {
        Meta {
            name: name.into(),
            topic: [topic; 32],
            creator: [2; 32],
            relay: "https://r/".into(),
            role: Role::Member,
            status: JoinStatus::Pending,
        }
    }

    fn msg(hash: u8, ts: u64, text: &str) -> StoredMessage {
        StoredMessage {
            op_hash: [hash; 32],
            author: [9; 32],
            sent_at_ms: 1,
            op_ts: Some(ts),
            text: text.into(),
        }
    }

    #[test]
    fn groups_round_trip_and_stay_separate() {
        let dir = tempfile::tempdir().unwrap();
        let state = State::open(dir.path()).unwrap();
        assert!(state.meta().unwrap().is_none());
        state.insert_group(&meta("general", 1)).unwrap();
        state.insert_group(&meta("random", 3)).unwrap();
        assert_eq!(state.meta().unwrap().unwrap(), meta("general", 1));
        assert_eq!(
            state.groups().unwrap(),
            [meta("general", 1), meta("random", 3)]
        );
        // Neither a topic nor a name can be used twice.
        assert!(state.insert_group(&meta("other", 1)).is_err());
        assert!(state.insert_group(&meta("general", 4)).is_err());
        assert!(state.insert_group(&meta("Not Valid", 5)).is_err());

        state.set_joined(&[3; 32]).unwrap();
        let m = state.group_named("#random").unwrap().unwrap();
        assert_eq!(m.status, JoinStatus::Joined);
        assert_eq!(state.meta().unwrap().unwrap(), meta("general", 1));
        assert!(state.group_named("missing").unwrap().is_none());
    }

    #[test]
    fn channel_names() {
        assert_eq!(channel_name("#General").unwrap(), "general");
        assert_eq!(channel_name(" dev_ops-2 ").unwrap(), "dev_ops-2");
        for bad in ["", "#", "two words", "é", &"x".repeat(MAX_NAME_BYTES + 1)] {
            assert!(channel_name(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn record_is_idempotent_and_history_sorted_per_group() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = State::open(dir.path()).unwrap();
        let (a, b) = ([1; 32], [3; 32]);
        let (m1, m2, other) = (
            msg(1, 1_000, "first"),
            msg(2, 2_000, "second"),
            msg(5, 1_500, "elsewhere"),
        );
        state
            .record(&a, &[2; 32], Outcome::Ok, None, Some(&m2))
            .unwrap();
        state
            .record(&a, &[1; 32], Outcome::Ok, None, Some(&m1))
            .unwrap();
        state
            .record(&a, &[1; 32], Outcome::Ok, None, Some(&m1))
            .unwrap();
        state
            .record(&a, &[3; 32], Outcome::Skipped, Some("x"), None)
            .unwrap();
        state
            .record(&b, &[5; 32], Outcome::Ok, None, Some(&other))
            .unwrap();
        let texts = |topic: &[u8; 32]| -> Vec<String> {
            let h = state.history(topic, None).unwrap();
            h.into_iter().map(|m| m.text).collect()
        };
        assert_eq!(texts(&a), ["first", "second"]);
        assert_eq!(texts(&b), ["elsewhere"]);
        assert_eq!(state.history(&a, Some(1)).unwrap()[0].text, "second");
        assert_eq!(state.outcome(&[3; 32]).unwrap(), Some(Outcome::Skipped));
        assert_eq!(state.outcome(&[4; 32]).unwrap(), None);
        let c = state.counts(&a).unwrap();
        assert_eq!((c.ok, c.skipped, c.failed), (2, 1, 0));
        assert_eq!(state.counts(&b).unwrap().ok, 1);
    }

    #[test]
    fn migrates_a_single_group_database() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(STATE_FILE);
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE meta (
                 id INTEGER PRIMARY KEY CHECK (id = 1), topic BLOB NOT NULL,
                 group_id BLOB NOT NULL, creator BLOB NOT NULL, relay TEXT NOT NULL,
                 role TEXT NOT NULL, status TEXT NOT NULL,
                 key_package_published INTEGER NOT NULL DEFAULT 0, joined_epoch INTEGER);
             CREATE TABLE processed_ops (op_hash BLOB PRIMARY KEY, outcome TEXT NOT NULL,
                 reason TEXT, recorded_at_ms INTEGER NOT NULL);
             CREATE TABLE messages (op_hash BLOB PRIMARY KEY, author BLOB NOT NULL,
                 sent_at_ms INTEGER NOT NULL, op_ts INTEGER, text TEXT NOT NULL);",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO meta VALUES (1, ?1, ?1, ?2, 'https://r/', 'creator', 'joined', 0, 0)",
            params![[7u8; 32].as_slice(), [2u8; 32].as_slice()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO messages VALUES (?1, ?2, 1, 1000, 'old')",
            params![[1u8; 32].as_slice(), [9u8; 32].as_slice()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO processed_ops VALUES (?1, 'ok', NULL, 1)",
            params![[1u8; 32].as_slice()],
        )
        .unwrap();
        drop(conn);

        for _ in 0..2 {
            let state = State::open(dir.path()).unwrap();
            let groups = state.groups().unwrap();
            assert_eq!(groups.len(), 1);
            assert_eq!(groups[0].name, "main");
            assert_eq!(groups[0].role, Role::Creator);
            assert_eq!(state.history(&[7; 32], None).unwrap()[0].text, "old");
            assert_eq!(state.counts(&[7; 32]).unwrap().ok, 1);
        }
    }
}

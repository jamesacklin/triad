//! `state.sqlite`: group metadata, processed operation IDs and decrypted
//! message history. Owned by the app, not by p2panda.
//!
//! One data directory holds any number of groups (channels). Each has a
//! locally unique name, and every operation and message is filed under its
//! group's topic.

use std::{collections::BTreeMap, path::Path};

use anyhow::{Result, anyhow, bail};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use crate::identity::NodeId;

pub const STATE_FILE: &str = "state.sqlite";

/// The environment variable holding the key for `state.sqlite`. Unset, the
/// file is plain SQLite. Set, the build must have SQLCipher (the
/// `sqlcipher` feature), or opening fails rather than ignoring the key.
pub const STATE_KEY_ENV: &str = "TRIAD_STATE_KEY";

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
    /// The author's bot wrote it (as the author claimed; see `bots`).
    pub bot: bool,
    pub nickname: Option<String>,
    /// An action (`/me waves`), shown as `* name waves`.
    pub action: bool,
}

/// A space message whose long text has not arrived yet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WaitingLong {
    pub op_hash: [u8; 32],
    pub topic: [u8; 32],
    /// Hash of the ciphertext it waits for.
    pub hash: [u8; 32],
    pub author: NodeId,
    /// p2panda operation timestamp (microseconds).
    pub op_ts: u64,
    /// The space message, CBOR encoded.
    pub body: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OutcomeCounts {
    pub ok: u64,
    pub skipped: u64,
    pub failed: u64,
}

/// The active bot of one channel, this node's or someone else's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BotRow {
    pub topic: [u8; 32],
    /// The node that runs it (the accepted host).
    pub host: NodeId,
    pub provider: String,
    pub servers: Vec<String>,
    pub enabled: bool,
    pub changed_at_ms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobState {
    Queued,
    Started,
    Done,
    Refused,
    Abandoned,
    Failed,
}

impl JobState {
    pub fn as_str(&self) -> &'static str {
        match self {
            JobState::Queued => "queued",
            JobState::Started => "started",
            JobState::Done => "done",
            JobState::Refused => "refused",
            JobState::Abandoned => "abandoned",
            JobState::Failed => "failed",
        }
    }

    fn parse(s: &str) -> Result<Self> {
        Ok(match s {
            "queued" => JobState::Queued,
            "started" => JobState::Started,
            "done" => JobState::Done,
            "refused" => JobState::Refused,
            "abandoned" => JobState::Abandoned,
            "failed" => JobState::Failed,
            other => bail!("unknown job state {other}"),
        })
    }
}

/// A bot job to insert with its message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewJob {
    pub asker: NodeId,
    pub state: JobState,
    pub reason: Option<String>,
    /// The asker's tool set, rendered.
    pub tools: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Job {
    pub op_hash: [u8; 32],
    pub topic: [u8; 32],
    pub asker: NodeId,
    pub state: JobState,
    pub reason: Option<String>,
    pub tools: String,
    pub created_at_ms: u64,
}

/// Answers counted against the limits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AnswerCounts {
    pub asker_last_hour: u64,
    pub channel_last_day: u64,
    pub channel_last_minute: u64,
}

/// One row of the audit log, with the job's asker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolCallRow {
    pub id: i64,
    pub op_hash: [u8; 32],
    pub asker: NodeId,
    pub tool: String,
    /// `sent`, `ok`, `error` or `denied`.
    pub state: String,
    pub result_bytes: Option<u64>,
    pub at_ms: u64,
}

pub struct State {
    conn: Connection,
}

impl State {
    pub fn open(data_dir: &Path) -> Result<Self> {
        Self::open_path(&data_dir.join(STATE_FILE))
    }

    pub fn open_path(path: &Path) -> Result<Self> {
        let mut conn = Connection::open(path)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        if let Some(key) = std::env::var(STATE_KEY_ENV).ok().filter(|k| !k.is_empty()) {
            // Must come before any other statement on the connection.
            conn.pragma_update(None, "key", &key)?;
            let cipher: Option<String> = conn
                .query_row("PRAGMA cipher_version", [], |r| r.get(0))
                .optional()?;
            if cipher.is_none() {
                bail!(
                    "{STATE_KEY_ENV} is set, but this build has no SQLCipher (feature `sqlcipher`)"
                );
            }
        }
        // Fails here, not later, on a wrong key or a missing one.
        conn.query_row("SELECT count(*) FROM sqlite_master", [], |r| {
            r.get::<_, i64>(0)
        })
        .map_err(|e| {
            anyhow!(
                "cannot read {}: {e} (wrong or missing {STATE_KEY_ENV}?)",
                path.display()
            )
        })?;
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
                 topic BLOB,
                 nickname TEXT
             );
             CREATE TABLE IF NOT EXISTS room_profiles (
                 topic BLOB NOT NULL,
                 author BLOB NOT NULL,
                 revision INTEGER NOT NULL,
                 nickname TEXT NOT NULL,
                 PRIMARY KEY (topic, author)
             );
             CREATE TABLE IF NOT EXISTS room_profile_revisions (
                 topic BLOB PRIMARY KEY,
                 revision INTEGER NOT NULL
             );",
        )?;
        migrate_layout(&mut conn)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS settings (
                 key TEXT PRIMARY KEY,
                 value BLOB NOT NULL
             );
             CREATE TABLE IF NOT EXISTS bots (
                 topic BLOB PRIMARY KEY,
                 host BLOB NOT NULL,
                 provider TEXT NOT NULL,
                 servers TEXT NOT NULL,
                 enabled INTEGER NOT NULL,
                 changed_at_ms INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS bot_grants (
                 topic BLOB NOT NULL,
                 node BLOB NOT NULL,
                 pattern TEXT NOT NULL,
                 PRIMARY KEY (topic, node, pattern)
             );
             CREATE TABLE IF NOT EXISTS bot_blocks (
                 node BLOB PRIMARY KEY
             );
             CREATE TABLE IF NOT EXISTS bot_jobs (
                 op_hash BLOB PRIMARY KEY,
                 topic BLOB NOT NULL,
                 asker BLOB NOT NULL,
                 state TEXT NOT NULL,
                 reason TEXT,
                 tools TEXT NOT NULL,
                 created_at_ms INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS bot_jobs_topic ON bot_jobs (topic, created_at_ms);
             CREATE TABLE IF NOT EXISTS bot_tool_calls (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 op_hash BLOB NOT NULL,
                 tool TEXT NOT NULL,
                 args_json TEXT NOT NULL,
                 state TEXT NOT NULL,
                 result_bytes INTEGER,
                 at_ms INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS long_bodies (
                 hash BLOB PRIMARY KEY,
                 topic BLOB NOT NULL,
                 ciphertext BLOB NOT NULL,
                 received_at_ms INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS long_waiting (
                 op_hash BLOB PRIMARY KEY,
                 topic BLOB NOT NULL,
                 hash BLOB NOT NULL,
                 author BLOB NOT NULL,
                 op_ts INTEGER NOT NULL,
                 body BLOB NOT NULL
             );
             CREATE INDEX IF NOT EXISTS long_waiting_hash ON long_waiting (hash);
             CREATE INDEX IF NOT EXISTS messages_topic ON messages (topic);
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

    pub fn profiles(&self, topic: &[u8; 32]) -> Result<BTreeMap<NodeId, String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT author, nickname FROM room_profiles WHERE topic = ?1")?;
        let rows = stmt.query_map([topic.as_slice()], |row| {
            Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut profiles = BTreeMap::new();
        for row in rows {
            let (author, nickname) = row?;
            profiles.insert(to_32(&author)?, nickname);
        }
        Ok(profiles)
    }

    pub fn profile(&self, topic: &[u8; 32], author: &NodeId) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT nickname FROM room_profiles WHERE topic = ?1 AND author = ?2",
                params![topic.as_slice(), author.as_slice()],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub fn next_profile_revision(&self, topic: &[u8; 32]) -> Result<u64> {
        let revision: i64 = self.conn.query_row(
            "INSERT INTO room_profile_revisions (topic, revision) VALUES (?1, 1)
             ON CONFLICT (topic) DO UPDATE SET revision = revision + 1
             RETURNING revision",
            [topic.as_slice()],
            |row| row.get(0),
        )?;
        Ok(u64::try_from(revision)?)
    }

    pub fn record_profile(
        &self,
        topic: &[u8; 32],
        author: &NodeId,
        revision: u64,
        nickname: &str,
    ) -> Result<bool> {
        Ok(self.conn.execute(
            "INSERT INTO room_profiles (topic, author, revision, nickname)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (topic, author) DO UPDATE
             SET revision = excluded.revision, nickname = excluded.nickname
             WHERE excluded.revision > room_profiles.revision",
            params![
                topic.as_slice(),
                author.as_slice(),
                i64::try_from(revision)?,
                nickname
            ],
        )? != 0)
    }

    /// Stores a long message's ciphertext from the plain stream beside a
    /// space. False when it was already stored.
    pub fn put_long_body(
        &self,
        topic: &[u8; 32],
        hash: &[u8; 32],
        ciphertext: &[u8],
    ) -> Result<bool> {
        Ok(self.conn.execute(
            "INSERT OR IGNORE INTO long_bodies (hash, topic, ciphertext, received_at_ms)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                hash.as_slice(),
                topic.as_slice(),
                ciphertext,
                now_ms() as i64
            ],
        )? != 0)
    }

    pub fn long_body(&self, hash: &[u8; 32]) -> Result<Option<Vec<u8>>> {
        Ok(self
            .conn
            .query_row(
                "SELECT ciphertext FROM long_bodies WHERE hash = ?1",
                [hash.as_slice()],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Keeps a space message whose long text has not arrived yet.
    pub fn wait_long(&self, w: &WaitingLong) -> Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO long_waiting (op_hash, topic, hash, author, op_ts, body)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                w.op_hash.as_slice(),
                w.topic.as_slice(),
                w.hash.as_slice(),
                w.author.as_slice(),
                w.op_ts as i64,
                w.body
            ],
        )?;
        Ok(())
    }

    /// Removes and returns the space messages waiting for this ciphertext.
    pub fn take_waiting(&mut self, hash: &[u8; 32]) -> Result<Vec<WaitingLong>> {
        let tx = self.conn.transaction()?;
        let mut out = Vec::new();
        {
            let mut stmt = tx.prepare(
                "SELECT op_hash, topic, author, op_ts, body FROM long_waiting WHERE hash = ?1
                 ORDER BY op_ts",
            )?;
            let rows = stmt.query_map([hash.as_slice()], |r| {
                Ok((
                    r.get::<_, Vec<u8>>(0)?,
                    r.get::<_, Vec<u8>>(1)?,
                    r.get::<_, Vec<u8>>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, Vec<u8>>(4)?,
                ))
            })?;
            for row in rows {
                let (op_hash, topic, author, op_ts, body) = row?;
                out.push(WaitingLong {
                    op_hash: to_32(&op_hash)?,
                    topic: to_32(&topic)?,
                    hash: *hash,
                    author: to_32(&author)?,
                    op_ts: op_ts as u64,
                    body,
                });
            }
        }
        tx.execute(
            "DELETE FROM long_waiting WHERE hash = ?1",
            [hash.as_slice()],
        )?;
        tx.commit()?;
        Ok(out)
    }

    /// How many space messages wait for their long text.
    pub fn waiting_count(&self) -> Result<u64> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM long_waiting", [], |r| r.get(0))?;
        Ok(n as u64)
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
        self.record_with_job(topic, op_hash, outcome, reason, message, None)
    }

    /// [`record`](Self::record), and a bot job for the message in the same
    /// transaction: a message is either recorded and queued, or neither.
    pub fn record_with_job(
        &mut self,
        topic: &[u8; 32],
        op_hash: &[u8; 32],
        outcome: Outcome,
        reason: Option<&str>,
        message: Option<&StoredMessage>,
        job: Option<&NewJob>,
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        if let Some(j) = job {
            tx.execute(
                "INSERT OR IGNORE INTO bot_jobs (op_hash, topic, asker, state, reason, tools, created_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    op_hash.as_slice(),
                    topic.as_slice(),
                    j.asker.as_slice(),
                    j.state.as_str(),
                    j.reason,
                    j.tools,
                    now_ms() as i64,
                ],
            )?;
        }
        if let Some(m) = message {
            tx.execute(
                "INSERT OR IGNORE INTO messages (op_hash, author, sent_at_ms, op_ts, text, topic, bot, nickname, action)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    m.op_hash.as_slice(),
                    m.author.as_slice(),
                    m.sent_at_ms as i64,
                    m.op_ts.map(|t| t as i64),
                    m.text,
                    topic.as_slice(),
                    m.bot,
                    m.nickname,
                    m.action,
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
            "SELECT op_hash, author, sent_at_ms, op_ts, text, bot, nickname, action FROM messages
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
                r.get::<_, bool>(5)?,
                r.get::<_, Option<String>>(6)?,
                r.get::<_, bool>(7)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (h, a, sent, ts, text, bot, nickname, action) = row?;
            out.push(StoredMessage {
                op_hash: to_32(&h)?,
                author: to_32(&a)?,
                sent_at_ms: sent as u64,
                op_ts: ts.map(|t| t as u64),
                text,
                bot,
                nickname,
                action,
            });
        }
        if let Some(limit) = limit
            && out.len() > limit
        {
            out.drain(..out.len() - limit);
        }
        Ok(out)
    }

    /// One stored message by operation hash.
    pub fn message(&self, op_hash: &[u8; 32]) -> Result<Option<(StoredMessage, [u8; 32])>> {
        let row = self
            .conn
            .query_row(
                "SELECT author, sent_at_ms, op_ts, text, bot, topic, nickname, action FROM messages WHERE op_hash = ?1",
                params![op_hash.as_slice()],
                |r| {
                    Ok((
                        r.get::<_, Vec<u8>>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, Option<i64>>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, bool>(4)?,
                        r.get::<_, Vec<u8>>(5)?,
                        r.get::<_, Option<String>>(6)?,
                        r.get::<_, bool>(7)?,
                    ))
                },
            )
            .optional()?;
        row.map(|(a, sent, ts, text, bot, topic, nickname, action)| {
            Ok((
                StoredMessage {
                    op_hash: *op_hash,
                    author: to_32(&a)?,
                    sent_at_ms: sent as u64,
                    op_ts: ts.map(|t| t as u64),
                    text,
                    bot,
                    nickname,
                    action,
                },
                to_32(&topic)?,
            ))
        })
        .transpose()
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

    // --- settings -------------------------------------------------------

    pub fn setting(&self, key: &str) -> Result<Option<Vec<u8>>> {
        Ok(self
            .conn
            .query_row(
                "SELECT value FROM settings WHERE key = ?1",
                params![key],
                |r| r.get(0),
            )
            .optional()?)
    }

    pub fn set_setting(&self, key: &str, value: &[u8]) -> Result<()> {
        self.conn.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    /// The space ID of this node's personal bot DM, once created.
    pub fn dm_topic(&self) -> Result<Option<[u8; 32]>> {
        self.setting("dm_topic")?.map(|v| to_32(&v)).transpose()
    }

    pub fn set_dm_topic(&self, topic: &[u8; 32]) -> Result<()> {
        self.set_setting("dm_topic", topic)
    }

    // --- bots -----------------------------------------------------------

    pub fn bot(&self, topic: &[u8; 32]) -> Result<Option<BotRow>> {
        let row = self
            .conn
            .query_row(
                "SELECT host, provider, servers, enabled, changed_at_ms FROM bots WHERE topic = ?1",
                params![topic.as_slice()],
                |r| {
                    Ok((
                        r.get::<_, Vec<u8>>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, bool>(3)?,
                        r.get::<_, i64>(4)?,
                    ))
                },
            )
            .optional()?;
        row.map(|(host, provider, servers, enabled, at)| {
            Ok(BotRow {
                topic: *topic,
                host: to_32(&host)?,
                provider,
                servers: servers
                    .split(',')
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect(),
                enabled,
                changed_at_ms: at as u64,
            })
        })
        .transpose()
    }

    pub fn set_bot(&self, bot: &BotRow) -> Result<()> {
        if bot.servers.iter().any(|s| s.contains(',')) {
            bail!("server names may not contain commas");
        }
        self.conn.execute(
            "INSERT INTO bots (topic, host, provider, servers, enabled, changed_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT (topic) DO UPDATE SET host = excluded.host,
                 provider = excluded.provider, servers = excluded.servers,
                 enabled = excluded.enabled, changed_at_ms = excluded.changed_at_ms",
            params![
                bot.topic.as_slice(),
                bot.host.as_slice(),
                bot.provider,
                bot.servers.join(","),
                bot.enabled,
                bot.changed_at_ms as i64,
            ],
        )?;
        Ok(())
    }

    // --- grants and blocks ----------------------------------------------

    pub fn grant(&self, topic: &[u8; 32], node: &NodeId, pattern: &str) -> Result<bool> {
        let n = self.conn.execute(
            "INSERT OR IGNORE INTO bot_grants (topic, node, pattern) VALUES (?1, ?2, ?3)",
            params![topic.as_slice(), node.as_slice(), pattern],
        )?;
        Ok(n > 0)
    }

    /// Removes one grant, or all of a node's grants in the channel.
    pub fn revoke(&self, topic: &[u8; 32], node: &NodeId, pattern: Option<&str>) -> Result<usize> {
        Ok(match pattern {
            Some(p) => self.conn.execute(
                "DELETE FROM bot_grants WHERE topic = ?1 AND node = ?2 AND pattern = ?3",
                params![topic.as_slice(), node.as_slice(), p],
            )?,
            None => self.conn.execute(
                "DELETE FROM bot_grants WHERE topic = ?1 AND node = ?2",
                params![topic.as_slice(), node.as_slice()],
            )?,
        })
    }

    /// `(node, pattern)` grants in a channel, sorted.
    pub fn grants(&self, topic: &[u8; 32]) -> Result<Vec<(NodeId, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT node, pattern FROM bot_grants WHERE topic = ?1 ORDER BY node, pattern",
        )?;
        let rows = stmt.query_map(params![topic.as_slice()], |r| {
            Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, String>(1)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (n, p) = row?;
            out.push((to_32(&n)?, p));
        }
        Ok(out)
    }

    pub fn block(&self, node: &NodeId) -> Result<bool> {
        let n = self.conn.execute(
            "INSERT OR IGNORE INTO bot_blocks (node) VALUES (?1)",
            params![node.as_slice()],
        )?;
        Ok(n > 0)
    }

    pub fn unblock(&self, node: &NodeId) -> Result<bool> {
        let n = self.conn.execute(
            "DELETE FROM bot_blocks WHERE node = ?1",
            params![node.as_slice()],
        )?;
        Ok(n > 0)
    }

    pub fn blocked(&self) -> Result<std::collections::BTreeSet<NodeId>> {
        let mut stmt = self.conn.prepare("SELECT node FROM bot_blocks")?;
        let rows = stmt.query_map([], |r| r.get::<_, Vec<u8>>(0))?;
        let mut out = std::collections::BTreeSet::new();
        for row in rows {
            out.insert(to_32(&row?)?);
        }
        Ok(out)
    }

    // --- jobs -----------------------------------------------------------

    fn jobs_where(&self, clause: &str, args: &[&dyn rusqlite::ToSql]) -> Result<Vec<Job>> {
        let sql = format!(
            "SELECT op_hash, topic, asker, state, reason, tools, created_at_ms FROM bot_jobs
             WHERE {clause} ORDER BY created_at_ms, rowid"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(args, |r| {
            Ok((
                r.get::<_, Vec<u8>>(0)?,
                r.get::<_, Vec<u8>>(1)?,
                r.get::<_, Vec<u8>>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, i64>(6)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (h, t, a, state, reason, tools, at) = row?;
            out.push(Job {
                op_hash: to_32(&h)?,
                topic: to_32(&t)?,
                asker: to_32(&a)?,
                state: JobState::parse(&state)?,
                reason,
                tools,
                created_at_ms: at as u64,
            });
        }
        Ok(out)
    }

    pub fn job(&self, op_hash: &[u8; 32]) -> Result<Option<Job>> {
        Ok(self
            .jobs_where("op_hash = ?1", &[&op_hash.as_slice()])?
            .into_iter()
            .next())
    }

    pub fn set_job_state(
        &self,
        op_hash: &[u8; 32],
        state: JobState,
        reason: Option<&str>,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE bot_jobs SET state = ?2, reason = COALESCE(?3, reason) WHERE op_hash = ?1",
            params![op_hash.as_slice(), state.as_str(), reason],
        )?;
        Ok(())
    }

    /// Queued jobs, oldest first, across every channel.
    pub fn queued_jobs(&self) -> Result<Vec<Job>> {
        self.jobs_where("state = 'queued'", &[])
    }

    /// Marks every started job abandoned, so it is never retried, and
    /// returns them. Run once at start.
    pub fn abandon_started(&self) -> Result<Vec<Job>> {
        let jobs = self.jobs_where("state = 'started'", &[])?;
        self.conn.execute(
            "UPDATE bot_jobs SET state = 'abandoned', reason = 'interrupted' WHERE state = 'started'",
            [],
        )?;
        Ok(jobs
            .into_iter()
            .map(|mut j| {
                j.state = JobState::Abandoned;
                j.reason = Some("interrupted".into());
                j
            })
            .collect())
    }

    /// Answers (every job that was not refused) in a channel: by `asker` in
    /// the last hour, and by anyone in the last day and the last minute.
    pub fn answer_counts(
        &self,
        topic: &[u8; 32],
        asker: &NodeId,
        now_ms: u64,
    ) -> Result<AnswerCounts> {
        let count = |since: u64, asker: Option<&NodeId>| -> Result<u64> {
            let n: i64 = match asker {
                Some(a) => self.conn.query_row(
                    "SELECT COUNT(*) FROM bot_jobs WHERE topic = ?1 AND asker = ?2
                     AND state != 'refused' AND created_at_ms >= ?3",
                    params![topic.as_slice(), a.as_slice(), since as i64],
                    |r| r.get(0),
                )?,
                None => self.conn.query_row(
                    "SELECT COUNT(*) FROM bot_jobs WHERE topic = ?1
                     AND state != 'refused' AND created_at_ms >= ?2",
                    params![topic.as_slice(), since as i64],
                    |r| r.get(0),
                )?,
            };
            Ok(n as u64)
        };
        Ok(AnswerCounts {
            asker_last_hour: count(now_ms.saturating_sub(3_600_000), Some(asker))?,
            channel_last_day: count(now_ms.saturating_sub(86_400_000), None)?,
            channel_last_minute: count(now_ms.saturating_sub(60_000), None)?,
        })
    }

    // --- audit log ------------------------------------------------------

    /// Records a tool call before it is sent. Returns its ID.
    pub fn tool_call_sent(&self, op_hash: &[u8; 32], tool: &str, args_json: &str) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO bot_tool_calls (op_hash, tool, args_json, state, at_ms)
             VALUES (?1, ?2, ?3, 'sent', ?4)",
            params![op_hash.as_slice(), tool, args_json, now_ms() as i64],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn tool_call_finished(&self, id: i64, ok: bool, result_bytes: u64) -> Result<()> {
        self.conn.execute(
            "UPDATE bot_tool_calls SET state = ?2, result_bytes = ?3 WHERE id = ?1",
            params![id, if ok { "ok" } else { "error" }, result_bytes as i64],
        )?;
        Ok(())
    }

    /// Records a call the gate refused; it was never sent.
    pub fn tool_call_denied(&self, op_hash: &[u8; 32], tool: &str, args_json: &str) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO bot_tool_calls (op_hash, tool, args_json, state, at_ms)
             VALUES (?1, ?2, ?3, 'denied', ?4)",
            params![op_hash.as_slice(), tool, args_json, now_ms() as i64],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// The newest `limit` tool calls in a channel, oldest first.
    pub fn tool_calls(&self, topic: &[u8; 32], limit: usize) -> Result<Vec<ToolCallRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT c.id, c.op_hash, j.asker, c.tool, c.state, c.result_bytes, c.at_ms
             FROM bot_tool_calls c JOIN bot_jobs j ON j.op_hash = c.op_hash
             WHERE j.topic = ?1 ORDER BY c.id DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![topic.as_slice(), limit as i64], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, Vec<u8>>(1)?,
                r.get::<_, Vec<u8>>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, Option<i64>>(5)?,
                r.get::<_, i64>(6)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, h, a, tool, state, bytes, at) = row?;
            out.push(ToolCallRow {
                id,
                op_hash: to_32(&h)?,
                asker: to_32(&a)?,
                tool,
                state,
                result_bytes: bytes.map(|b| b as u64),
                at_ms: at as u64,
            });
        }
        out.reverse();
        Ok(out)
    }
}

fn migrate_layout(conn: &mut Connection) -> Result<()> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let has_column = |table: &str, column: &str| -> Result<bool> {
        let n: i64 = tx.query_row(
            "SELECT COUNT(*) FROM pragma_table_info(?1) WHERE name = ?2",
            params![table, column],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    };
    for table in ["processed_ops", "messages"] {
        if !has_column(table, "topic")? {
            tx.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN topic BLOB;"))?;
        }
    }
    if !has_column("messages", "bot")? {
        tx.execute_batch("ALTER TABLE messages ADD COLUMN bot INTEGER NOT NULL DEFAULT 0;")?;
    }
    if !has_column("messages", "nickname")? {
        tx.execute_batch("ALTER TABLE messages ADD COLUMN nickname TEXT;")?;
    }
    if !has_column("messages", "action")? {
        tx.execute_batch("ALTER TABLE messages ADD COLUMN action INTEGER NOT NULL DEFAULT 0;")?;
    }
    if has_column("meta", "topic")? {
        // The old single room becomes the channel called main.
        tx.execute_batch(
            "INSERT OR IGNORE INTO groups (topic, name, creator, relay, role, status)
                 SELECT topic, 'main', creator, relay, role, status FROM meta;
             UPDATE processed_ops SET topic = (SELECT topic FROM meta) WHERE topic IS NULL;
             UPDATE messages SET topic = (SELECT topic FROM meta) WHERE topic IS NULL;
             DROP TABLE meta;",
        )?;
    }
    tx.commit()?;
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
            bot: hash.is_multiple_of(2),
            nickname: None,
            action: hash == 3,
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
        let (mut m1, m2, other) = (
            msg(1, 1_000, "first"),
            msg(2, 2_000, "second"),
            msg(5, 1_500, "elsewhere"),
        );
        m1.nickname = Some("Alice".into());
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
        let bots: Vec<bool> = state
            .history(&a, None)
            .unwrap()
            .iter()
            .map(|m| m.bot)
            .collect();
        assert_eq!(bots, [false, true]);
        assert_eq!(
            state.history(&a, None).unwrap()[0].nickname.as_deref(),
            Some("Alice")
        );
        assert_eq!(texts(&b), ["elsewhere"]);
        assert_eq!(state.history(&a, Some(1)).unwrap()[0].text, "second");
        assert_eq!(state.outcome(&[3; 32]).unwrap(), Some(Outcome::Skipped));
        assert_eq!(state.outcome(&[4; 32]).unwrap(), None);
        let c = state.counts(&a).unwrap();
        assert_eq!((c.ok, c.skipped, c.failed), (2, 1, 0));
        assert_eq!(state.counts(&b).unwrap().ok, 1);
    }

    #[test]
    fn bot_tables() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = State::open(dir.path()).unwrap();
        let (t, me, b) = ([1u8; 32], [2u8; 32], [3u8; 32]);
        assert_eq!(state.dm_topic().unwrap(), None);
        state.set_dm_topic(&[9; 32]).unwrap();
        assert_eq!(state.dm_topic().unwrap(), Some([9; 32]));

        let row = BotRow {
            topic: t,
            host: me,
            provider: "mock".into(),
            servers: vec!["calendar".into(), "linear".into()],
            enabled: true,
            changed_at_ms: 5,
        };
        state.set_bot(&row).unwrap();
        assert_eq!(state.bot(&t).unwrap().unwrap(), row);
        state
            .set_bot(&BotRow {
                enabled: false,
                servers: vec![],
                ..row.clone()
            })
            .unwrap();
        let off = state.bot(&t).unwrap().unwrap();
        assert!(!off.enabled && off.servers.is_empty());

        assert!(state.grant(&t, &b, "calendar.*").unwrap());
        assert!(!state.grant(&t, &b, "calendar.*").unwrap());
        state.grant(&t, &b, "linear.create_issue").unwrap();
        assert_eq!(state.grants(&t).unwrap().len(), 2);
        assert_eq!(state.revoke(&t, &b, Some("calendar.*")).unwrap(), 1);
        assert_eq!(state.revoke(&t, &b, None).unwrap(), 1);
        assert!(state.grants(&t).unwrap().is_empty());

        assert!(state.block(&b).unwrap());
        assert!(state.blocked().unwrap().contains(&b));
        assert!(state.unblock(&b).unwrap());
        assert!(state.blocked().unwrap().is_empty());

        // A job is inserted with its message, once.
        let job = NewJob {
            asker: b,
            state: JobState::Queued,
            reason: None,
            tools: "".into(),
        };
        state
            .record_with_job(
                &t,
                &[7; 32],
                Outcome::Ok,
                None,
                Some(&msg(7, 1, "@bot hi")),
                Some(&job),
            )
            .unwrap();
        state
            .record_with_job(&t, &[7; 32], Outcome::Ok, None, None, Some(&job))
            .unwrap();
        assert_eq!(state.queued_jobs().unwrap().len(), 1);
        let refused = NewJob {
            state: JobState::Refused,
            reason: Some("blocked".into()),
            ..job.clone()
        };
        state
            .record_with_job(&t, &[8; 32], Outcome::Ok, None, None, Some(&refused))
            .unwrap();
        let c = state.answer_counts(&t, &b, now_ms()).unwrap();
        assert_eq!(
            (c.asker_last_hour, c.channel_last_day, c.channel_last_minute),
            (1, 1, 1)
        );
        assert_eq!(
            state
                .answer_counts(&t, &me, now_ms())
                .unwrap()
                .asker_last_hour,
            0
        );

        state
            .set_job_state(&[7; 32], JobState::Started, None)
            .unwrap();
        let id = state
            .tool_call_sent(&[7; 32], "calendar.create_event", "{}")
            .unwrap();
        state
            .tool_call_denied(&[7; 32], "linear.create_issue", "{}")
            .unwrap();
        drop(state);
        // Restart: the started job is abandoned, never retried; its sent
        // tool call stays recorded as sent.
        let state = State::open(dir.path()).unwrap();
        let abandoned = state.abandon_started().unwrap();
        assert_eq!(abandoned.len(), 1);
        assert_eq!(
            state.job(&[7; 32]).unwrap().unwrap().state,
            JobState::Abandoned
        );
        assert!(state.queued_jobs().unwrap().is_empty());
        assert!(state.abandon_started().unwrap().is_empty());
        let calls = state.tool_calls(&t, 10).unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(
            (calls[0].id, calls[0].state.as_str(), calls[0].asker),
            (id, "sent", b)
        );
        assert_eq!(calls[1].state, "denied");
        state.tool_call_finished(id, true, 2).unwrap();
        assert_eq!(state.tool_calls(&t, 1).unwrap()[0].state, "denied");
        assert_eq!(state.tool_calls(&t, 10).unwrap()[0].state, "ok");
    }

    #[test]
    fn profiles_keep_the_latest_update_per_author_and_room() {
        let dir = tempfile::tempdir().unwrap();
        let (room, other, author) = ([1; 32], [2; 32], [3; 32]);
        {
            let state = State::open(dir.path()).unwrap();
            assert_eq!(state.next_profile_revision(&room).unwrap(), 1);
            assert!(state.record_profile(&room, &author, 2, "Renée").unwrap());
            assert!(!state.record_profile(&room, &author, 1, "Old").unwrap());
            assert!(state.record_profile(&other, &author, 1, "Other").unwrap());
            assert!(state.record_profile(&room, &author, 3, "").unwrap());
            assert!(!state.record_profile(&room, &author, 2, "Renée").unwrap());
        }
        let state = State::open(dir.path()).unwrap();
        assert_eq!(state.profile(&room, &author).unwrap().as_deref(), Some(""));
        assert_eq!(state.next_profile_revision(&room).unwrap(), 2);
        assert_eq!(
            state
                .profiles(&other)
                .unwrap()
                .get(&author)
                .map(String::as_str),
            Some("Other")
        );
    }

    #[test]
    fn long_bodies_and_waiting_messages() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = State::open(dir.path()).unwrap();
        let (t, h) = ([1; 32], [2; 32]);
        assert_eq!(state.long_body(&h).unwrap(), None);
        let w = WaitingLong {
            op_hash: [3; 32],
            topic: t,
            hash: h,
            author: [4; 32],
            op_ts: 9,
            body: vec![1, 2, 3],
        };
        state.wait_long(&w).unwrap();
        state.wait_long(&w).unwrap();
        assert_eq!(state.waiting_count().unwrap(), 1);
        assert!(state.put_long_body(&t, &h, b"ct").unwrap());
        assert!(!state.put_long_body(&t, &h, b"ct").unwrap());
        assert_eq!(state.long_body(&h).unwrap().as_deref(), Some(&b"ct"[..]));
        assert_eq!(state.take_waiting(&h).unwrap(), vec![w]);
        assert!(state.take_waiting(&h).unwrap().is_empty());
        assert_eq!(state.waiting_count().unwrap(), 0);
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
            let history = state.history(&[7; 32], None).unwrap();
            assert_eq!(history[0].text, "old");
            assert_eq!(history[0].nickname, None);
            assert_eq!(state.counts(&[7; 32]).unwrap().ok, 1);
        }
    }
}

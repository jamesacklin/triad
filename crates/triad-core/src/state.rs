//! `state.sqlite`: group metadata, processed operation IDs and decrypted
//! message history. Owned by the app, not by p2panda or OpenMLS.

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

/// The single `meta` row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Meta {
    pub topic: [u8; 32],
    pub group_id: Vec<u8>,
    pub creator: NodeId,
    pub relay: String,
    pub role: Role,
    pub status: JoinStatus,
    /// Whether this member already published its KeyPackage (joiners only).
    pub key_package_published: bool,
    /// MLS epoch at which this member joined (0 for the creator).
    pub joined_epoch: Option<u64>,
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
             CREATE TABLE IF NOT EXISTS meta (
                 id INTEGER PRIMARY KEY CHECK (id = 1),
                 topic BLOB NOT NULL,
                 group_id BLOB NOT NULL,
                 creator BLOB NOT NULL,
                 relay TEXT NOT NULL,
                 role TEXT NOT NULL,
                 status TEXT NOT NULL,
                 key_package_published INTEGER NOT NULL DEFAULT 0,
                 joined_epoch INTEGER
             );
             CREATE TABLE IF NOT EXISTS processed_ops (
                 op_hash BLOB PRIMARY KEY,
                 outcome TEXT NOT NULL,
                 reason TEXT,
                 recorded_at_ms INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS messages (
                 op_hash BLOB PRIMARY KEY,
                 author BLOB NOT NULL,
                 sent_at_ms INTEGER NOT NULL,
                 op_ts INTEGER,
                 text TEXT NOT NULL
             );",
        )?;
        Ok(State { conn })
    }

    pub fn meta(&self) -> Result<Option<Meta>> {
        let row = self
            .conn
            .query_row(
                "SELECT topic, group_id, creator, relay, role, status,
                        key_package_published, joined_epoch FROM meta WHERE id = 1",
                [],
                |r| {
                    Ok((
                        r.get::<_, Vec<u8>>(0)?,
                        r.get::<_, Vec<u8>>(1)?,
                        r.get::<_, Vec<u8>>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, String>(4)?,
                        r.get::<_, String>(5)?,
                        r.get::<_, bool>(6)?,
                        r.get::<_, Option<i64>>(7)?,
                    ))
                },
            )
            .optional()?;
        let Some((topic, group_id, creator, relay, role, status, kp, joined_epoch)) = row else {
            return Ok(None);
        };
        Ok(Some(Meta {
            topic: to_32(&topic)?,
            group_id,
            creator: to_32(&creator)?,
            relay,
            role: Role::parse(&role)?,
            status: JoinStatus::parse(&status)?,
            key_package_published: kp,
            joined_epoch: joined_epoch.map(|e| e as u64),
        }))
    }

    /// Writes the meta row. Fails if one already exists: one group per data directory.
    pub fn insert_meta(&self, meta: &Meta) -> Result<()> {
        if self.meta()?.is_some() {
            bail!("this data directory already holds a group; use a fresh --data-dir");
        }
        self.conn.execute(
            "INSERT INTO meta (id, topic, group_id, creator, relay, role, status,
                               key_package_published, joined_epoch)
             VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                meta.topic.as_slice(),
                meta.group_id,
                meta.creator.as_slice(),
                meta.relay,
                meta.role.as_str(),
                meta.status.as_str(),
                meta.key_package_published,
                meta.joined_epoch.map(|e| e as i64),
            ],
        )?;
        Ok(())
    }

    pub fn set_joined(&self, epoch: u64) -> Result<()> {
        self.conn.execute(
            "UPDATE meta SET status = 'joined', joined_epoch = ?1 WHERE id = 1",
            params![epoch as i64],
        )?;
        Ok(())
    }

    pub fn set_key_package_published(&self) -> Result<()> {
        self.conn
            .execute("UPDATE meta SET key_package_published = 1 WHERE id = 1", [])?;
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
    /// in one transaction. Re-recording the same hash is a no-op.
    pub fn record(
        &mut self,
        op_hash: &[u8; 32],
        outcome: Outcome,
        reason: Option<&str>,
        message: Option<&StoredMessage>,
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        if let Some(m) = message {
            tx.execute(
                "INSERT OR IGNORE INTO messages (op_hash, author, sent_at_ms, op_ts, text)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    m.op_hash.as_slice(),
                    m.author.as_slice(),
                    m.sent_at_ms as i64,
                    m.op_ts.map(|t| t as i64),
                    m.text,
                ],
            )?;
        }
        tx.execute(
            "INSERT OR IGNORE INTO processed_ops (op_hash, outcome, reason, recorded_at_ms)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                op_hash.as_slice(),
                outcome.as_str(),
                reason,
                now_ms() as i64
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

    /// Stored messages, oldest first, sorted by p2panda operation timestamp,
    /// then operation hash. With a limit, returns the newest `limit` messages.
    pub fn history(&self, limit: Option<usize>) -> Result<Vec<StoredMessage>> {
        let mut stmt = self.conn.prepare(
            "SELECT op_hash, author, sent_at_ms, op_ts, text FROM messages
             ORDER BY COALESCE(op_ts, sent_at_ms * 1000) ASC, op_hash ASC",
        )?;
        let rows = stmt.query_map([], |r| {
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

    pub fn counts(&self) -> Result<OutcomeCounts> {
        let mut counts = OutcomeCounts::default();
        let mut stmt = self
            .conn
            .prepare("SELECT outcome, COUNT(*) FROM processed_ops GROUP BY outcome")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
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

    fn meta() -> Meta {
        Meta {
            topic: [1; 32],
            group_id: vec![1; 32],
            creator: [2; 32],
            relay: "https://r/".into(),
            role: Role::Member,
            status: JoinStatus::Pending,
            key_package_published: false,
            joined_epoch: None,
        }
    }

    #[test]
    fn meta_round_trip_and_single_group() {
        let dir = tempfile::tempdir().unwrap();
        let state = State::open(dir.path()).unwrap();
        assert!(state.meta().unwrap().is_none());
        state.insert_meta(&meta()).unwrap();
        assert_eq!(state.meta().unwrap().unwrap(), meta());
        assert!(state.insert_meta(&meta()).is_err());
        state.set_key_package_published().unwrap();
        state.set_joined(1).unwrap();
        let m = state.meta().unwrap().unwrap();
        assert!(m.key_package_published);
        assert_eq!(m.status, JoinStatus::Joined);
        assert_eq!(m.joined_epoch, Some(1));
    }

    #[test]
    fn record_is_idempotent_and_history_sorted() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = State::open(dir.path()).unwrap();
        let m2 = StoredMessage {
            op_hash: [2; 32],
            author: [9; 32],
            sent_at_ms: 1,
            op_ts: Some(2_000),
            text: "second".into(),
        };
        let m1 = StoredMessage {
            op_hash: [1; 32],
            author: [9; 32],
            sent_at_ms: 1,
            op_ts: Some(1_000),
            text: "first".into(),
        };
        state
            .record(&[2; 32], Outcome::Ok, None, Some(&m2))
            .unwrap();
        state
            .record(&[1; 32], Outcome::Ok, None, Some(&m1))
            .unwrap();
        state
            .record(&[1; 32], Outcome::Ok, None, Some(&m1))
            .unwrap();
        state
            .record(&[3; 32], Outcome::Skipped, Some("x"), None)
            .unwrap();
        let h = state.history(None).unwrap();
        assert_eq!(
            h.iter().map(|m| m.text.as_str()).collect::<Vec<_>>(),
            ["first", "second"]
        );
        assert_eq!(state.history(Some(1)).unwrap()[0].text, "second");
        assert_eq!(state.outcome(&[3; 32]).unwrap(), Some(Outcome::Skipped));
        assert_eq!(state.outcome(&[4; 32]).unwrap(), None);
        let c = state.counts().unwrap();
        assert_eq!((c.ok, c.skipped, c.failed), (2, 1, 0));
    }
}

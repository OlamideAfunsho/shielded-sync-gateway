//! Verified-chain storage (SQLite).
//!
//! Only verified blocks are written, one SQLite transaction per block, so
//! the stored chain is always a prefix of verified blocks: a crash leaves
//! either the whole block or none of it. Rollback for reorgs is a single
//! transaction too.

use std::path::Path;
use std::sync::Mutex;

use anyhow::{bail, Context};
use lwproto::rpc::{ChainMetadata, CompactBlock};
use prost::Message;
use rusqlite::{params, Connection, OptionalExtension};

use crate::checkpoint::Checkpoint;

pub struct Store {
    conn: Mutex<Connection>,
    checkpoint: Checkpoint,
}

/// The newest verified block, or the checkpoint if none yet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tip {
    pub height: u32,
    pub hash: [u8; 32],
    /// Sapling, Orchard, Ironwood tree sizes after this block.
    pub sizes: [u64; 3],
}

impl Store {
    pub fn open(path: &Path, checkpoint: Checkpoint) -> anyhow::Result<Self> {
        let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=FULL;
             CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS blocks (
                 height INTEGER PRIMARY KEY,
                 hash BLOB NOT NULL UNIQUE,
                 prev_hash BLOB NOT NULL,
                 compact BLOB NOT NULL,
                 sapling_size INTEGER NOT NULL,
                 orchard_size INTEGER NOT NULL,
                 ironwood_size INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS txids (txid BLOB PRIMARY KEY, height INTEGER NOT NULL);
             CREATE INDEX IF NOT EXISTS txids_height ON txids(height);",
        )?;
        let pinned = serde_json::to_string(&checkpoint)?;
        let stored: Option<String> = conn
            .query_row("SELECT value FROM meta WHERE key = 'checkpoint'", [], |r| {
                r.get(0)
            })
            .optional()?;
        match stored {
            None => {
                conn.execute("INSERT INTO meta VALUES ('checkpoint', ?1)", [&pinned])?;
            }
            Some(s) if s == pinned => {}
            Some(_) => bail!(
                "{} was built from a different checkpoint; use a fresh state file",
                path.display()
            ),
        }
        Ok(Store {
            conn: Mutex::new(conn),
            checkpoint,
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn
            .lock()
            .expect("store mutex poisoned by a panicking writer")
    }

    pub fn checkpoint(&self) -> &Checkpoint {
        &self.checkpoint
    }

    pub fn tip(&self) -> anyhow::Result<Tip> {
        let c = self.lock();
        let row = c
            .query_row(
                "SELECT height, hash, sapling_size, orchard_size, ironwood_size FROM blocks ORDER BY height DESC LIMIT 1",
                [],
                |r| Ok((r.get::<_, u32>(0)?, r.get::<_, Vec<u8>>(1)?, [r.get::<_, i64>(2)?, r.get(3)?, r.get(4)?])),
            )
            .optional()?;
        Ok(match row {
            Some((height, hash, s)) => Tip {
                height,
                hash: hash
                    .try_into()
                    .map_err(|_| anyhow::anyhow!("bad stored hash"))?,
                sizes: s.map(|v| v as u64),
            },
            None => Tip {
                height: self.checkpoint.height,
                hash: self.checkpoint.hash_bytes()?,
                sizes: self.checkpoint.sizes(),
            },
        })
    }

    /// Appends a verified block. It must extend the current tip.
    pub fn append(
        &self,
        block: &CompactBlock,
        txids: &[[u8; 32]],
        sizes: [u64; 3],
    ) -> anyhow::Result<()> {
        let tip = self.tip()?;
        if block.height != u64::from(tip.height) + 1 || block.prev_hash.as_slice() != tip.hash {
            bail!(
                "block {} does not extend stored tip {}",
                block.height,
                tip.height
            );
        }
        let mut stored = block.clone();
        stored.chain_metadata = Some(ChainMetadata {
            sapling_commitment_tree_size: sizes[0] as u32,
            orchard_commitment_tree_size: sizes[1] as u32,
            ironwood_commitment_tree_size: sizes[2] as u32,
        });
        let mut c = self.lock();
        let tx = c.transaction()?;
        tx.execute(
            "INSERT INTO blocks VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                block.height as i64,
                block.hash,
                block.prev_hash,
                stored.encode_to_vec(),
                sizes[0] as i64,
                sizes[1] as i64,
                sizes[2] as i64
            ],
        )?;
        for t in txids {
            tx.execute(
                "INSERT INTO txids VALUES (?1, ?2)",
                params![t.as_slice(), block.height as i64],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Deletes every block above `height`.
    pub fn rollback_to(&self, height: u32) -> anyhow::Result<usize> {
        let mut c = self.lock();
        let tx = c.transaction()?;
        let n = tx.execute("DELETE FROM blocks WHERE height > ?1", [height])?;
        tx.execute("DELETE FROM txids WHERE height > ?1", [height])?;
        tx.commit()?;
        Ok(n)
    }

    pub fn block(&self, height: u32) -> anyhow::Result<Option<CompactBlock>> {
        let c = self.lock();
        let bytes: Option<Vec<u8>> = c
            .query_row(
                "SELECT compact FROM blocks WHERE height = ?1",
                [height],
                |r| r.get(0),
            )
            .optional()?;
        Ok(match bytes {
            Some(b) => Some(CompactBlock::decode(b.as_slice())?),
            None => None,
        })
    }

    pub fn hash_at(&self, height: u32) -> anyhow::Result<Option<[u8; 32]>> {
        if height == self.checkpoint.height {
            return Ok(Some(self.checkpoint.hash_bytes()?));
        }
        let c = self.lock();
        let h: Option<Vec<u8>> = c
            .query_row("SELECT hash FROM blocks WHERE height = ?1", [height], |r| {
                r.get(0)
            })
            .optional()?;
        Ok(h.and_then(|v| v.try_into().ok()))
    }

    pub fn height_of_hash(&self, hash: &[u8]) -> anyhow::Result<Option<u32>> {
        let c = self.lock();
        Ok(
            c.query_row("SELECT height FROM blocks WHERE hash = ?1", [hash], |r| {
                r.get(0)
            })
            .optional()?,
        )
    }

    pub fn sizes_at(&self, height: u32) -> anyhow::Result<Option<[u64; 3]>> {
        if height == self.checkpoint.height {
            return Ok(Some(self.checkpoint.sizes()));
        }
        let c = self.lock();
        Ok(c.query_row(
            "SELECT sapling_size, orchard_size, ironwood_size FROM blocks WHERE height = ?1",
            [height],
            |r| {
                Ok([
                    r.get::<_, i64>(0)? as u64,
                    r.get::<_, i64>(1)? as u64,
                    r.get::<_, i64>(2)? as u64,
                ])
            },
        )
        .optional()?)
    }

    pub fn tx_height(&self, txid: &[u8]) -> anyhow::Result<Option<u32>> {
        let c = self.lock();
        Ok(
            c.query_row("SELECT height FROM txids WHERE txid = ?1", [txid], |r| {
                r.get(0)
            })
            .optional()?,
        )
    }
}

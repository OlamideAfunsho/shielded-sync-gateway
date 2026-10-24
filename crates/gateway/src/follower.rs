//! Verification loop: follows the trusted chain, verifies each block from
//! untrusted compact data plus an untrusted record, and stores it.
//!
//! Invariants:
//! * The stored chain only ever grows by a verified block whose parent is
//!   the stored tip, so the verified height never advances past a failure.
//! * If the trusted chain no longer contains the stored tip, blocks are
//!   rolled back to the last common height before anything else happens.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context};
use lwproto::record::BlockRecord;
use lwproto::rpc::compact_tx_streamer_client::CompactTxStreamerClient;
use lwproto::rpc::{BlockId, BlockRange, CompactBlock, PoolType};
use prost::Message;
use serde::Serialize;
use ssg_verifier::{verify::display, verify_block, TrustedHeader};
use tokio::sync::watch;
use tonic::transport::Channel;
use tracing::{info, warn};

use crate::store::Store;
use crate::trusted::Trusted;

#[derive(Clone, Debug, Serialize)]
pub struct Failure {
    pub height: u32,
    pub reason: String,
    pub first_seen_unix_ms: u128,
    pub attempts: u64,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Status {
    pub checkpoint_height: u32,
    pub verified_height: u32,
    pub trusted_tip: u32,
    pub failure: Option<Failure>,
    pub blocks_verified: u64,
    pub rollbacks: u64,
    pub rolled_back_blocks: u64,
    pub verify_seconds: f64,
    pub compact_bytes_in: u64,
    pub record_bytes_in: u64,
    pub trusted_bytes_in: u64,
    pub failures_seen: u64,
    /// Peak resident memory of this process (VmHWM), kB.
    pub peak_rss_kb: u64,
}

pub struct Shared {
    pub store: Store,
    pub status: RwLock<Status>,
    pub changed: watch::Sender<u64>,
    pub trusted: Trusted,
    pub upstream: CompactTxStreamerClient<Channel>,
    pub producer_url: String,
    pub http: reqwest::Client,
    version: AtomicU64,
}

fn peak_rss_kb() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("VmHWM:"))
                .and_then(|l| l.split_whitespace().nth(1)?.parse().ok())
        })
        .unwrap_or(0)
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or_default()
}

/// Why a height is not (yet) servable.
#[derive(Debug)]
pub enum NotReady {
    Failed(Failure),
    BeyondTip(u32),
    Timeout(u32),
}

impl Shared {
    pub fn new(
        store: Store,
        trusted: Trusted,
        upstream: CompactTxStreamerClient<Channel>,
        producer_url: String,
    ) -> anyhow::Result<Arc<Self>> {
        let tip = store.tip()?;
        let status = Status {
            checkpoint_height: store.checkpoint().height,
            verified_height: tip.height,
            ..Status::default()
        };
        let (changed, _) = watch::channel(0);
        Ok(Arc::new(Shared {
            store,
            status: RwLock::new(status),
            changed,
            trusted,
            upstream,
            producer_url,
            http: reqwest::Client::builder().no_proxy().build()?,
            version: AtomicU64::new(0),
        }))
    }

    pub fn status(&self) -> Status {
        self.status.read().expect("status lock").clone()
    }

    fn update(&self, f: impl FnOnce(&mut Status)) {
        {
            let mut s = self.status.write().expect("status lock");
            f(&mut s);
            s.trusted_bytes_in = self.trusted.bytes_in.load(Ordering::Relaxed);
            s.peak_rss_kb = peak_rss_kb();
        }
        let v = self.version.fetch_add(1, Ordering::Relaxed) + 1;
        self.changed.send_replace(v);
    }

    /// Waits until `height` is verified, verification fails at or below
    /// it, or the timeout passes.
    pub async fn wait_verified(&self, height: u32, timeout: Duration) -> Result<(), NotReady> {
        let deadline = Instant::now() + timeout;
        let mut rx = self.changed.subscribe();
        loop {
            {
                let s = self.status.read().expect("status lock");
                if s.verified_height >= height {
                    return Ok(());
                }
                if let Some(f) = s.failure.as_ref().filter(|f| f.height <= height) {
                    return Err(NotReady::Failed(f.clone()));
                }
                if s.trusted_tip != 0 && height > s.trusted_tip {
                    return Err(NotReady::BeyondTip(s.trusted_tip));
                }
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() || tokio::time::timeout(left, rx.changed()).await.is_err() {
                return Err(NotReady::Timeout(height));
            }
        }
    }

    async fn record(&self, header: &TrustedHeader) -> anyhow::Result<(BlockRecord, usize)> {
        let url = format!(
            "{}/v1/record/{}?hash={}",
            self.producer_url,
            header.height,
            display(&header.hash)
        );
        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .context("producer unreachable")?;
        let status = resp.status();
        let body = resp.bytes().await?;
        if !status.is_success() {
            bail!(
                "producer returned {status}: {}",
                String::from_utf8_lossy(&body)
            );
        }
        let rec = BlockRecord::decode(body.as_ref())
            .map_err(|e| anyhow!("record does not decode: {e}"))?;
        Ok((rec, body.len()))
    }

    async fn compact_blocks(&self, start: u32, end: u32) -> anyhow::Result<Vec<CompactBlock>> {
        let req = BlockRange {
            start: Some(BlockId {
                height: u64::from(start),
                hash: vec![],
            }),
            end: Some(BlockId {
                height: u64::from(end),
                hash: vec![],
            }),
            pool_types: vec![
                PoolType::Transparent as i32,
                PoolType::Sapling as i32,
                PoolType::Orchard as i32,
                PoolType::Ironwood as i32,
            ],
        };
        let mut client = self.upstream.clone();
        let mut stream = client
            .get_block_range(req)
            .await
            .context("indexer GetBlockRange")?
            .into_inner();
        let mut out = vec![];
        while let Some(b) = stream.message().await.context("indexer stream")? {
            out.push(b);
        }
        Ok(out)
    }

    /// Rolls back stored blocks the trusted chain no longer contains.
    async fn handle_reorg(&self, trusted_tip: u32) -> anyhow::Result<()> {
        let cp = self.store.checkpoint().height;
        let tip = self.store.tip()?;
        if tip.height <= cp {
            return Ok(());
        }
        let mut h = tip.height.min(trusted_tip);
        loop {
            if h <= cp {
                let th = self.trusted.header(cp).await?;
                if th.hash != self.store.checkpoint().hash_bytes()? {
                    bail!("trusted chain reorganized below the checkpoint at {cp}; refusing to continue");
                }
                break;
            }
            let ours = self
                .store
                .hash_at(h)?
                .ok_or_else(|| anyhow!("missing stored block {h}"))?;
            if self.trusted.header(h).await?.hash == ours {
                break;
            }
            h -= 1;
        }
        if h < tip.height {
            let n = self.store.rollback_to(h)?;
            warn!(
                from = tip.height,
                to = h,
                blocks = n,
                "trusted chain changed: rolled back verified blocks"
            );
            self.update(|s| {
                s.verified_height = h;
                s.rollbacks += 1;
                s.rolled_back_blocks += n as u64;
                if s.failure.as_ref().is_some_and(|f| f.height > h + 1) {
                    s.failure = None;
                }
            });
        }
        Ok(())
    }

    fn fail(&self, height: u32, reason: String) {
        let mut log_it = false;
        self.update(|s| match s.failure.as_mut() {
            Some(f) if f.height == height && f.reason == reason => f.attempts += 1,
            _ => {
                s.failure = Some(Failure {
                    height,
                    reason: reason.clone(),
                    first_seen_unix_ms: now_ms(),
                    attempts: 1,
                });
                s.failures_seen += 1;
                log_it = true;
            }
        });
        if log_it {
            warn!(
                height,
                "verification failed, verified height stays at {}: {reason}",
                height - 1
            );
        }
    }

    /// One pass: reorg check, then verify forward up to `batch` blocks.
    pub async fn step(&self, batch: u32) -> anyhow::Result<()> {
        let (trusted_tip, _) = self.trusted.tip().await?;
        self.update(|s| s.trusted_tip = trusted_tip);
        self.handle_reorg(trusted_tip).await?;

        let tip = self.store.tip()?;
        if tip.height >= trusted_tip {
            return Ok(());
        }
        let start = tip.height + 1;
        let end = trusted_tip.min(tip.height + batch);

        // Fetch trusted headers, records and trusted tree sizes for the batch
        // concurrently; verify and store strictly in order.
        let mut tasks = tokio::task::JoinSet::new();
        let permits = Arc::new(tokio::sync::Semaphore::new(8));
        for h in start..=end {
            let t = self.trusted.clone();
            let permits = permits.clone();
            tasks.spawn(async move {
                let _p = permits.acquire_owned().await;
                (h, t.header(h).await, t.tree_states(h).await)
            });
        }
        let mut fetched = std::collections::BTreeMap::new();
        while let Some(r) = tasks.join_next().await {
            let (h, header, trees) = r?;
            fetched.insert(h, (header?, trees?));
        }
        let compact = match self.compact_blocks(start, end).await {
            Ok(c) => c,
            Err(e) => {
                self.fail(
                    start,
                    format!("indexer did not serve blocks {start}..={end}: {e:#}"),
                );
                return Ok(());
            }
        };
        let compact_bytes: usize = compact.iter().map(|b| b.encoded_len()).sum();
        self.update(|s| s.compact_bytes_in += compact_bytes as u64);
        let mut by_height: std::collections::HashMap<u64, CompactBlock> =
            compact.into_iter().map(|b| (b.height, b)).collect();

        let mut parent = tip;
        for h in start..=end {
            let (header, trees) = &fetched[&h];
            if header.prev_hash != parent.hash {
                // The trusted chain moved under us; the next step's reorg
                // check sorts it out.
                return Ok(());
            }
            let Some(cb) = by_height.remove(&u64::from(h)) else {
                self.fail(h, format!("indexer omitted block {h}"));
                return Ok(());
            };
            let (record, record_len) = match self.record(header).await {
                Ok(r) => r,
                Err(e) => {
                    self.fail(h, format!("no usable record: {e:#}"));
                    return Ok(());
                }
            };
            self.update(|s| s.record_bytes_in += record_len as u64);
            let t0 = Instant::now();
            let verified = match verify_block(header, &record, &cb) {
                Ok(v) => v,
                Err(e) => {
                    self.fail(h, e.to_string());
                    return Ok(());
                }
            };
            let sizes = [
                parent.sizes[0] + u64::from(verified.sapling_outputs),
                parent.sizes[1] + u64::from(verified.orchard_actions),
                parent.sizes[2] + u64::from(verified.ironwood_actions),
            ];
            let trusted_sizes = [trees.sapling.size, trees.orchard.size, trees.ironwood.size];
            if sizes != trusted_sizes {
                self.fail(h, format!("tree sizes {sizes:?} from verified blocks disagree with trusted tree state {trusted_sizes:?}"));
                return Ok(());
            }
            self.store
                .append(&verified.compact, &verified.txids, sizes)?;
            let dt = t0.elapsed().as_secs_f64();
            let recovered = self.status().failure.is_some();
            self.update(|s| {
                s.verified_height = h;
                s.blocks_verified += 1;
                s.verify_seconds += dt;
                if s.failure.as_ref().is_some_and(|f| f.height <= h) {
                    s.failure = None;
                }
            });
            if recovered {
                info!(height = h, "verification recovered");
            }
            parent = crate::store::Tip {
                height: h,
                hash: header.hash,
                sizes,
            };
        }
        Ok(())
    }

    pub async fn run(self: Arc<Self>, interval: Duration, batch: u32) {
        let mut logged_at = 0u32;
        loop {
            if let Err(e) = self.step(batch).await {
                warn!("follower: {e:#}");
                tokio::time::sleep(interval).await;
                continue;
            }
            let s = self.status();
            if s.verified_height != logged_at && s.verified_height == s.trusted_tip {
                info!(
                    height = s.verified_height,
                    blocks = s.blocks_verified,
                    "verified up to trusted tip"
                );
                logged_at = s.verified_height;
            }
            if s.verified_height < s.trusted_tip && s.failure.is_none() {
                continue;
            }
            tokio::time::sleep(interval).await;
        }
    }
}

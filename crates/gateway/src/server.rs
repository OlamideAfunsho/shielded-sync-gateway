//! The wallet-facing CompactTxStreamer.
//!
//! Scanning data (compact blocks, including their transparent inputs and
//! outputs, tree states, subtree roots, the tip) is served only from
//! verified state or the trusted node. Calls outside the
//! shielded-scanning claim are forwarded to the indexer, and the README
//! says which ones.

// tonic::Status is the error type the generated trait requires.
#![allow(clippy::result_large_err)]

use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use lwproto::rpc::compact_tx_streamer_server::CompactTxStreamer;
use lwproto::rpc::*;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::Stream;
use tonic::{Request, Response, Status, Streaming};
use tracing::warn;

use crate::follower::{NotReady, Shared};

type Out<T> = Pin<Box<dyn Stream<Item = Result<T, Status>> + Send>>;

pub struct Gateway {
    pub shared: Arc<Shared>,
    pub wait: Duration,
}

fn not_ready(e: NotReady) -> Status {
    match e {
        NotReady::Failed(f) => Status::data_loss(format!(
            "ssg-gateway: block {} failed verification; refusing to serve it: {}",
            f.height, f.reason
        )),
        NotReady::BeyondTip(t) => {
            Status::out_of_range(format!("ssg-gateway: beyond trusted tip {t}"))
        }
        NotReady::Timeout(h) => {
            Status::unavailable(format!("ssg-gateway: block {h} is not verified yet"))
        }
    }
}

fn internal(e: impl std::fmt::Display) -> Status {
    Status::internal(format!("ssg-gateway: {e}"))
}

/// Which pools a request wants. An empty list means the shielded pools,
/// as in lightwalletd.
#[derive(Clone, Copy)]
struct Pools {
    transparent: bool,
    sapling: bool,
    orchard: bool,
    ironwood: bool,
}

impl Pools {
    fn from_request(types: &[i32]) -> Result<Self, Status> {
        if types.is_empty() {
            return Ok(Pools {
                transparent: false,
                sapling: true,
                orchard: true,
                ironwood: true,
            });
        }
        let mut p = Pools {
            transparent: false,
            sapling: false,
            orchard: false,
            ironwood: false,
        };
        for t in types {
            match PoolType::try_from(*t) {
                Ok(PoolType::Sapling) => p.sapling = true,
                Ok(PoolType::Orchard) => p.orchard = true,
                Ok(PoolType::Ironwood) => p.ironwood = true,
                Ok(PoolType::Transparent) => p.transparent = true,
                _ => return Err(Status::invalid_argument(format!("unknown pool type {t}"))),
            }
        }
        Ok(p)
    }

    fn prune(self, mut b: CompactBlock) -> CompactBlock {
        for tx in &mut b.vtx {
            if !self.transparent {
                tx.vin.clear();
                tx.vout.clear();
            }
            if !self.sapling {
                tx.spends.clear();
                tx.outputs.clear();
            }
            if !self.orchard {
                tx.actions.clear();
            }
            if !self.ironwood {
                tx.ironwood_actions.clear();
            }
        }
        b.vtx.retain(|t| {
            !(t.vin.is_empty()
                && t.vout.is_empty()
                && t.spends.is_empty()
                && t.outputs.is_empty()
                && t.actions.is_empty()
                && t.ironwood_actions.is_empty())
        });
        b
    }
}

/// The nullifier-only form used by the *Nullifiers methods.
fn nullifiers_only(mut b: CompactBlock) -> CompactBlock {
    for tx in &mut b.vtx {
        tx.vin.clear();
        tx.vout.clear();
        tx.outputs.clear();
        for a in tx.actions.iter_mut().chain(tx.ironwood_actions.iter_mut()) {
            a.cmx.clear();
            a.ephemeral_key.clear();
            a.ciphertext.clear();
        }
    }
    b.chain_metadata = None;
    b
}

impl Gateway {
    async fn verified_block(&self, height: u32) -> Result<CompactBlock, Status> {
        self.shared
            .wait_verified(height, self.wait)
            .await
            .map_err(not_ready)?;
        self.shared
            .store
            .block(height)
            .map_err(internal)?
            .ok_or_else(|| {
                Status::not_found(format!(
                    "ssg-gateway: no verified block {height} (at or below the checkpoint?)"
                ))
            })
    }

    async fn resolve(&self, id: &BlockId) -> Result<u32, Status> {
        if !id.hash.is_empty() {
            return self
                .shared
                .store
                .height_of_hash(&id.hash)
                .map_err(internal)?
                .ok_or_else(|| Status::not_found("ssg-gateway: unknown or unverified block hash"));
        }
        u32::try_from(id.height).map_err(|_| Status::invalid_argument("height out of range"))
    }

    fn stream_range(
        &self,
        range: BlockRange,
        nullifiers: bool,
    ) -> Result<Out<CompactBlock>, Status> {
        let pools = Pools::from_request(&range.pool_types)?;
        let start = range.start.map(|b| b.height).unwrap_or(0);
        let end = range.end.map(|b| b.height).unwrap_or(0);
        let (start, end) = (
            u32::try_from(start).map_err(|_| Status::invalid_argument("start out of range"))?,
            u32::try_from(end).map_err(|_| Status::invalid_argument("end out of range"))?,
        );
        let heights: Vec<u32> = if start <= end {
            (start..=end).collect()
        } else {
            (end..=start).rev().collect()
        };
        let (tx, rx) = mpsc::channel(64);
        let shared = self.shared.clone();
        let wait = self.wait;
        tokio::spawn(async move {
            let gw = Gateway { shared, wait };
            for h in heights {
                let item = gw.verified_block(h).await.map(|b| {
                    let b = pools.prune(b);
                    if nullifiers {
                        nullifiers_only(b)
                    } else {
                        b
                    }
                });
                let stop = item.is_err();
                if let Err(e) = &item {
                    warn!(height = h, "GetBlockRange refused: {}", e.message());
                }
                if tx.send(item).await.is_err() || stop {
                    break;
                }
            }
        });
        Ok(Box::pin(ReceiverStream::new(rx)))
    }

    fn upstream(
        &self,
    ) -> compact_tx_streamer_client::CompactTxStreamerClient<tonic::transport::Channel> {
        self.shared.upstream.clone()
    }
}

/// Forwards a server-streaming call unchanged.
fn forward_stream<T: Send + 'static>(s: Streaming<T>) -> Out<T> {
    Box::pin(s)
}

#[tonic::async_trait]
impl CompactTxStreamer for Gateway {
    async fn get_latest_block(&self, _: Request<ChainSpec>) -> Result<Response<BlockId>, Status> {
        let (height, hash) = self.shared.trusted.tip().await.map_err(internal)?;
        Ok(Response::new(BlockId {
            height: u64::from(height),
            hash: hash.to_vec(),
        }))
    }

    async fn get_block(&self, r: Request<BlockId>) -> Result<Response<CompactBlock>, Status> {
        // GetBlock returns every pool, transparent included.
        let h = self.resolve(r.get_ref()).await?;
        Ok(Response::new(self.verified_block(h).await?))
    }

    async fn get_block_nullifiers(
        &self,
        r: Request<BlockId>,
    ) -> Result<Response<CompactBlock>, Status> {
        let h = self.resolve(r.get_ref()).await?;
        Ok(Response::new(nullifiers_only(
            self.verified_block(h).await?,
        )))
    }

    type GetBlockRangeStream = Out<CompactBlock>;
    async fn get_block_range(
        &self,
        r: Request<BlockRange>,
    ) -> Result<Response<Self::GetBlockRangeStream>, Status> {
        Ok(Response::new(self.stream_range(r.into_inner(), false)?))
    }

    type GetBlockRangeNullifiersStream = Out<CompactBlock>;
    async fn get_block_range_nullifiers(
        &self,
        r: Request<BlockRange>,
    ) -> Result<Response<Self::GetBlockRangeNullifiersStream>, Status> {
        Ok(Response::new(self.stream_range(r.into_inner(), true)?))
    }

    async fn get_transaction(
        &self,
        r: Request<TxFilter>,
    ) -> Result<Response<RawTransaction>, Status> {
        let want = r.get_ref().hash.clone();
        if want.len() != 32 {
            return Err(Status::invalid_argument(
                "ssg-gateway: GetTransaction needs a 32-byte txid",
            ));
        }
        let mut raw = self.upstream().get_transaction(r).await?.into_inner();
        let got = ssg_verifier::produce::raw_txid(&raw.data).map_err(|e| {
            Status::data_loss(format!(
                "ssg-gateway: indexer returned an unparseable transaction: {e}"
            ))
        })?;
        if got.as_slice() != want.as_slice() {
            return Err(Status::data_loss(format!(
                "ssg-gateway: indexer returned transaction {} for requested {}",
                ssg_verifier::verify::display(&got),
                ssg_verifier::verify::display(&want)
            )));
        }
        // The bytes are now authenticated by the txid. The height is the
        // indexer's claim; replace it with where the verified chain has it.
        let mut known = self.shared.store.tx_height(&want).map_err(internal)?;
        if known.is_none() && raw.height > 0 {
            let claimed =
                u32::try_from(raw.height).map_err(|_| Status::data_loss("height out of range"))?;
            if claimed > self.shared.status().verified_height {
                // Not verified that far yet; wait, then look again.
                let _ = self.shared.wait_verified(claimed, self.wait).await;
                known = self.shared.store.tx_height(&want).map_err(internal)?;
            }
            if known.is_none() {
                return Err(Status::data_loss(format!(
                    "ssg-gateway: indexer places transaction at height {claimed}, but no verified block contains it"
                )));
            }
        }
        // Unmined (height 0) transactions pass through: mempool data is
        // outside the claim.
        if let Some(h) = known {
            raw.height = u64::from(h);
        }
        Ok(Response::new(raw))
    }

    async fn send_transaction(
        &self,
        r: Request<RawTransaction>,
    ) -> Result<Response<SendResponse>, Status> {
        self.upstream().send_transaction(r).await
    }

    type GetTaddressTxidsStream = Out<RawTransaction>;
    async fn get_taddress_txids(
        &self,
        r: Request<TransparentAddressBlockFilter>,
    ) -> Result<Response<Self::GetTaddressTxidsStream>, Status> {
        Ok(Response::new(forward_stream(
            self.upstream().get_taddress_txids(r).await?.into_inner(),
        )))
    }

    type GetTaddressTransactionsStream = Out<RawTransaction>;
    async fn get_taddress_transactions(
        &self,
        r: Request<TransparentAddressBlockFilter>,
    ) -> Result<Response<Self::GetTaddressTransactionsStream>, Status> {
        Ok(Response::new(forward_stream(
            self.upstream()
                .get_taddress_transactions(r)
                .await?
                .into_inner(),
        )))
    }

    async fn get_taddress_balance(
        &self,
        r: Request<AddressList>,
    ) -> Result<Response<Balance>, Status> {
        self.upstream().get_taddress_balance(r).await
    }

    async fn get_taddress_balance_stream(
        &self,
        r: Request<Streaming<Address>>,
    ) -> Result<Response<Balance>, Status> {
        let mut inbound = r.into_inner();
        let mut addrs = vec![];
        while let Some(a) = inbound.message().await? {
            addrs.push(a);
        }
        self.upstream()
            .get_taddress_balance_stream(tokio_stream::iter(addrs))
            .await
    }

    type GetMempoolTxStream = Out<CompactTx>;
    async fn get_mempool_tx(
        &self,
        r: Request<GetMempoolTxRequest>,
    ) -> Result<Response<Self::GetMempoolTxStream>, Status> {
        Ok(Response::new(forward_stream(
            self.upstream().get_mempool_tx(r).await?.into_inner(),
        )))
    }

    type GetMempoolStreamStream = Out<RawTransaction>;
    async fn get_mempool_stream(
        &self,
        r: Request<Empty>,
    ) -> Result<Response<Self::GetMempoolStreamStream>, Status> {
        Ok(Response::new(forward_stream(
            self.upstream().get_mempool_stream(r).await?.into_inner(),
        )))
    }

    async fn get_tree_state(&self, r: Request<BlockId>) -> Result<Response<TreeState>, Status> {
        let h = self.resolve(r.get_ref()).await?;
        let cp = self.shared.store.checkpoint().height;
        if h > cp {
            self.shared
                .wait_verified(h, self.wait)
                .await
                .map_err(not_ready)?;
        } else if h < cp {
            return Err(Status::out_of_range(format!(
                "ssg-gateway: height {h} is below the checkpoint {cp}"
            )));
        }
        let t = self.shared.trusted.tree_states(h).await.map_err(internal)?;
        // Same node, same height: these must agree with what we verified.
        let stored_hash = self.shared.store.hash_at(h).map_err(internal)?;
        let stored_sizes = self.shared.store.sizes_at(h).map_err(internal)?;
        if stored_hash.map(|b| ssg_verifier::verify::display(&b)) != Some(t.hash.clone())
            || stored_sizes != Some([t.sapling.size, t.orchard.size, t.ironwood.size])
        {
            return Err(Status::unavailable(format!(
                "ssg-gateway: trusted tree state at {h} does not match the verified chain (reorg in progress?)"
            )));
        }
        let network = self.shared.store.checkpoint().network.clone();
        Ok(Response::new(TreeState {
            network,
            height: u64::from(h),
            hash: t.hash,
            time: t.time,
            sapling_tree: t.sapling.final_state,
            orchard_tree: t.orchard.final_state,
            ironwood_tree: t.ironwood.final_state,
        }))
    }

    async fn get_latest_tree_state(
        &self,
        _: Request<Empty>,
    ) -> Result<Response<TreeState>, Status> {
        let h = self.shared.status().verified_height;
        self.get_tree_state(Request::new(BlockId {
            height: u64::from(h),
            hash: vec![],
        }))
        .await
    }

    type GetSubtreeRootsStream = Out<SubtreeRoot>;
    async fn get_subtree_roots(
        &self,
        r: Request<GetSubtreeRootsArg>,
    ) -> Result<Response<Self::GetSubtreeRootsStream>, Status> {
        let a = r.into_inner();
        let pool = match ShieldedProtocol::try_from(a.shielded_protocol) {
            Ok(ShieldedProtocol::Sapling) => "sapling",
            Ok(ShieldedProtocol::Orchard) => "orchard",
            Ok(ShieldedProtocol::Ironwood) => "ironwood",
            Err(_) => return Err(Status::invalid_argument("unknown shielded protocol")),
        };
        let limit = (a.max_entries > 0).then_some(a.max_entries);
        let roots = self
            .shared
            .trusted
            .subtrees(pool, a.start_index, limit)
            .await
            .map_err(internal)?;
        let verified = self.shared.status().verified_height;
        let mut out = vec![];
        for (root, height) in roots {
            // Only subtrees completed inside the verified chain.
            if height > verified {
                break;
            }
            let header = self.shared.trusted.header(height).await.map_err(internal)?;
            let mut display_order = header.hash;
            display_order.reverse();
            out.push(Ok(SubtreeRoot {
                root_hash: root,
                // zaino 0.10.1 sends this hash in display byte order.
                completing_block_hash: display_order.to_vec(),
                completing_block_height: u64::from(height),
            }));
        }
        Ok(Response::new(Box::pin(tokio_stream::iter(out))))
    }

    async fn get_address_utxos(
        &self,
        r: Request<GetAddressUtxosArg>,
    ) -> Result<Response<GetAddressUtxosReplyList>, Status> {
        self.upstream().get_address_utxos(r).await
    }

    type GetAddressUtxosStreamStream = Out<GetAddressUtxosReply>;
    async fn get_address_utxos_stream(
        &self,
        r: Request<GetAddressUtxosArg>,
    ) -> Result<Response<Self::GetAddressUtxosStreamStream>, Status> {
        Ok(Response::new(forward_stream(
            self.upstream()
                .get_address_utxos_stream(r)
                .await?
                .into_inner(),
        )))
    }

    async fn get_lightd_info(&self, r: Request<Empty>) -> Result<Response<LightdInfo>, Status> {
        let mut info = self.upstream().get_lightd_info(r).await?.into_inner();
        let network = &self.shared.store.checkpoint().network;
        if &info.chain_name != network {
            return Err(Status::failed_precondition(format!(
                "ssg-gateway: indexer is on {:?}, trusted node on {network:?}",
                info.chain_name
            )));
        }
        let (tip, _) = self.shared.trusted.tip().await.map_err(internal)?;
        info.block_height = u64::from(tip);
        info.estimated_height = u64::from(tip);
        info.vendor = format!(
            "ssg-gateway {} (indexer: {})",
            env!("CARGO_PKG_VERSION"),
            info.vendor
        );
        Ok(Response::new(info))
    }

    async fn ping(&self, r: Request<Duration_>) -> Result<Response<PingResponse>, Status> {
        self.upstream().ping(r).await
    }
}

type Duration_ = lwproto::rpc::Duration;

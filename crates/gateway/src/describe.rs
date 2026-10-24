//! Turns CompactTxStreamer payloads into small JSON descriptions for the
//! call log.
//!
//! Requests are logged in full (they are tiny). Responses are folded into
//! an aggregate per call, because a single GetBlockRange can stream
//! thousands of blocks and we care about what the wallet asked for and
//! which fields it was handed, not every ciphertext.

use std::collections::BTreeMap;

use lwproto::rpc;
use prost::Message;
use serde_json::{json, Value};

/// Display form of a 32-byte protocol-order hash (byte-reversed hex, as
/// block explorers and zcashd RPCs print it).
fn display_hash(bytes: &[u8]) -> String {
    let mut v = bytes.to_vec();
    v.reverse();
    hex::encode(v)
}

/// Transaction version and version group from the first eight bytes of a
/// serialized transaction (overwintered format: header, then group id).
fn tx_version(raw: &[u8]) -> Option<(u32, u32)> {
    let header = u32::from_le_bytes(raw.get(0..4)?.try_into().ok()?);
    let group = u32::from_le_bytes(raw.get(4..8)?.try_into().ok()?);
    (header >> 31 == 1).then_some((header & 0x7fff_ffff, group))
}

fn pool_name(p: i32) -> String {
    rpc::PoolType::try_from(p)
        .map(|p| p.as_str_name().to_string())
        .unwrap_or_else(|_| format!("unknown({p})"))
}

fn block_id(b: &Option<rpc::BlockId>) -> Value {
    match b {
        None => Value::Null,
        Some(b) if b.hash.is_empty() => json!(b.height),
        Some(b) => json!({ "height": b.height, "hash": display_hash(&b.hash) }),
    }
}

/// Decodes a request message. Unknown methods and undecodable payloads
/// are reported by size, never dropped.
pub fn request(method: &str, payload: &[u8]) -> Value {
    fn dec<M: Message + Default>(p: &[u8]) -> Result<M, String> {
        M::decode(p).map_err(|e| e.to_string())
    }
    let r: Result<Value, String> = (|| {
        Ok(match method {
            "GetLatestBlock" => json!({}),
            "GetLightdInfo" | "GetLatestTreeState" | "GetMempoolStream" => json!({}),
            "GetBlock" | "GetBlockNullifiers" | "GetTreeState" => {
                let m: rpc::BlockId = dec(payload)?;
                json!({ "block": block_id(&Some(m)) })
            }
            "GetBlockRange" | "GetBlockRangeNullifiers" => {
                let m: rpc::BlockRange = dec(payload)?;
                json!({
                    "start": block_id(&m.start),
                    "end": block_id(&m.end),
                    "pool_types": m.pool_types.iter().map(|p| pool_name(*p)).collect::<Vec<_>>(),
                })
            }
            "GetSubtreeRoots" => {
                let m: rpc::GetSubtreeRootsArg = dec(payload)?;
                let proto = rpc::ShieldedProtocol::try_from(m.shielded_protocol)
                    .map(|p| p.as_str_name().to_string())
                    .unwrap_or_else(|_| m.shielded_protocol.to_string());
                json!({ "protocol": proto, "start_index": m.start_index, "max_entries": m.max_entries })
            }
            "GetTransaction" => {
                let m: rpc::TxFilter = dec(payload)?;
                json!({ "txid": display_hash(&m.hash), "block": block_id(&m.block), "index": m.index })
            }
            "SendTransaction" => {
                let m: rpc::RawTransaction = dec(payload)?;
                json!({ "tx_bytes": m.data.len() })
            }
            "GetTaddressTxids" | "GetTaddressTransactions" => {
                let m: rpc::TransparentAddressBlockFilter = dec(payload)?;
                let (s, e) = m
                    .range
                    .map(|r| (block_id(&r.start), block_id(&r.end)))
                    .unwrap_or_default();
                json!({ "address": m.address, "start": s, "end": e })
            }
            "GetTaddressBalance" => {
                let m: rpc::AddressList = dec(payload)?;
                json!({ "addresses": m.addresses.len() })
            }
            "GetAddressUtxos" | "GetAddressUtxosStream" => {
                let m: rpc::GetAddressUtxosArg = dec(payload)?;
                json!({ "addresses": m.addresses.len(), "start_height": m.start_height, "max_entries": m.max_entries })
            }
            "GetMempoolTx" => {
                let m: rpc::GetMempoolTxRequest = dec(payload)?;
                json!({
                    "exclude": m.exclude_txid_suffixes.len(),
                    "pool_types": m.pool_types.iter().map(|p| pool_name(*p)).collect::<Vec<_>>(),
                })
            }
            "Ping" => {
                let m: rpc::Duration = dec(payload)?;
                json!({ "interval_us": m.interval_us })
            }
            _ => json!({ "undecoded_bytes": payload.len() }),
        })
    })();
    r.unwrap_or_else(|e| json!({ "decode_error": e, "bytes": payload.len() }))
}

/// Running per-call response aggregate.
#[derive(Default)]
pub struct Responses {
    messages: u64,
    decode_errors: u64,
    last: Option<Value>,
    blocks: Option<BlockStats>,
    subtrees: Option<SubtreeStats>,
}

#[derive(Default)]
struct BlockStats {
    first_height: Option<u64>,
    last_height: Option<u64>,
    /// Heights must arrive strictly increasing by one inside a range. A
    /// gap or repeat here is worth knowing about before any verification
    /// exists.
    out_of_order: u64,
    txs: u64,
    sapling_spends: u64,
    sapling_outputs: u64,
    orchard_actions: u64,
    ironwood_actions: u64,
    transparent_in: u64,
    transparent_out: u64,
    /// How often each field arrived non-empty. This is the inventory of
    /// what the wallet is actually given to scan with.
    fields: BTreeMap<&'static str, u64>,
    final_tree_sizes: Option<Value>,
}

const ORCHARD_FIELDS: [&str; 4] = [
    "orchard_action.nullifier",
    "orchard_action.cmx",
    "orchard_action.ephemeral_key",
    "orchard_action.ciphertext",
];
const IRONWOOD_FIELDS: [&str; 4] = [
    "ironwood_action.nullifier",
    "ironwood_action.cmx",
    "ironwood_action.ephemeral_key",
    "ironwood_action.ciphertext",
];

#[derive(Default)]
struct SubtreeStats {
    roots: u64,
    first_completing_height: Option<u64>,
    last_completing_height: Option<u64>,
}

impl BlockStats {
    fn bump(&mut self, field: &'static str, present: bool) {
        if present {
            *self.fields.entry(field).or_default() += 1;
        }
    }

    fn observe(&mut self, b: &rpc::CompactBlock) {
        if let Some(prev) = self.last_height {
            if b.height != prev + 1 {
                self.out_of_order += 1;
            }
        }
        self.first_height.get_or_insert(b.height);
        self.last_height = Some(b.height);

        self.bump("block.hash", !b.hash.is_empty());
        self.bump("block.prev_hash", !b.prev_hash.is_empty());
        self.bump("block.time", b.time != 0);
        self.bump("block.header", !b.header.is_empty());
        self.bump("block.chain_metadata", b.chain_metadata.is_some());
        if let Some(m) = &b.chain_metadata {
            self.final_tree_sizes = Some(json!({
                "sapling": m.sapling_commitment_tree_size,
                "orchard": m.orchard_commitment_tree_size,
                "ironwood": m.ironwood_commitment_tree_size,
            }));
        }

        for tx in &b.vtx {
            self.txs += 1;
            self.bump("tx.txid", !tx.txid.is_empty());
            self.bump("tx.fee", tx.fee != 0);
            self.sapling_spends += tx.spends.len() as u64;
            self.sapling_outputs += tx.outputs.len() as u64;
            self.orchard_actions += tx.actions.len() as u64;
            self.ironwood_actions += tx.ironwood_actions.len() as u64;
            self.transparent_in += tx.vin.len() as u64;
            self.transparent_out += tx.vout.len() as u64;
            for s in &tx.spends {
                self.bump("sapling_spend.nf", !s.nf.is_empty());
            }
            for o in &tx.outputs {
                self.bump("sapling_output.cmu", !o.cmu.is_empty());
                self.bump("sapling_output.ephemeral_key", !o.ephemeral_key.is_empty());
                self.bump("sapling_output.ciphertext", !o.ciphertext.is_empty());
            }
            for (names, actions) in [
                (&ORCHARD_FIELDS, &tx.actions),
                (&IRONWOOD_FIELDS, &tx.ironwood_actions),
            ] {
                for a in actions {
                    self.bump(names[0], !a.nullifier.is_empty());
                    self.bump(names[1], !a.cmx.is_empty());
                    self.bump(names[2], !a.ephemeral_key.is_empty());
                    self.bump(names[3], !a.ciphertext.is_empty());
                }
            }
            self.bump("tx.vin", !tx.vin.is_empty());
            self.bump("tx.vout", !tx.vout.is_empty());
        }
    }

    fn to_json(&self) -> Value {
        json!({
            "first_height": self.first_height,
            "last_height": self.last_height,
            "out_of_order": self.out_of_order,
            "txs": self.txs,
            "sapling_spends": self.sapling_spends,
            "sapling_outputs": self.sapling_outputs,
            "orchard_actions": self.orchard_actions,
            "ironwood_actions": self.ironwood_actions,
            "transparent_in": self.transparent_in,
            "transparent_out": self.transparent_out,
            "fields_present": self.fields,
            "final_tree_sizes": self.final_tree_sizes,
        })
    }
}

impl Responses {
    pub fn observe(&mut self, method: &str, payload: &[u8]) {
        self.messages += 1;
        match self.try_observe(method, payload) {
            Ok(()) => {}
            Err(e) => {
                self.decode_errors += 1;
                self.last = Some(json!({ "decode_error": e.to_string(), "bytes": payload.len() }));
            }
        }
    }

    fn try_observe(&mut self, method: &str, p: &[u8]) -> Result<(), prost::DecodeError> {
        match method {
            "GetBlockRange" | "GetBlockRangeNullifiers" | "GetBlock" | "GetBlockNullifiers" => {
                let b = rpc::CompactBlock::decode(p)?;
                self.blocks.get_or_insert_with(Default::default).observe(&b);
            }
            "GetLatestBlock" => {
                let m = rpc::BlockId::decode(p)?;
                self.last = Some(block_id(&Some(m)));
            }
            "GetTreeState" | "GetLatestTreeState" => {
                let t = rpc::TreeState::decode(p)?;
                // The serialized frontiers are what the wallet seeds its
                // note-tree state from; their sizes are enough to see which
                // pools are populated without dumping them.
                self.last = Some(json!({
                    "network": t.network,
                    "height": t.height,
                    "hash": t.hash,
                    "time": t.time,
                    "sapling_tree_hex_len": t.sapling_tree.len(),
                    "orchard_tree_hex_len": t.orchard_tree.len(),
                    "ironwood_tree_hex_len": t.ironwood_tree.len(),
                }));
            }
            "GetSubtreeRoots" => {
                let r = rpc::SubtreeRoot::decode(p)?;
                let s = self.subtrees.get_or_insert_with(Default::default);
                s.roots += 1;
                s.first_completing_height
                    .get_or_insert(r.completing_block_height);
                s.last_completing_height = Some(r.completing_block_height);
            }
            "GetLightdInfo" => {
                let i = rpc::LightdInfo::decode(p)?;
                self.last = Some(json!({
                    "vendor": i.vendor,
                    "version": i.version,
                    "chain_name": i.chain_name,
                    "consensus_branch_id": i.consensus_branch_id,
                    "block_height": i.block_height,
                    "estimated_height": i.estimated_height,
                    "sapling_activation_height": i.sapling_activation_height,
                    "upgrade_name": i.upgrade_name,
                    "upgrade_height": i.upgrade_height,
                    "lightwallet_protocol_version": i.lightwallet_protocol_version,
                    "taddr_support": i.taddr_support,
                }));
            }
            "GetTransaction"
            | "GetTaddressTxids"
            | "GetTaddressTransactions"
            | "GetMempoolStream" => {
                let t = rpc::RawTransaction::decode(p)?;
                let mut v = json!({ "tx_bytes": t.data.len(), "height": t.height });
                if let Some((version, group)) = tx_version(&t.data) {
                    v["tx_version"] = json!(version);
                    v["version_group_id"] = json!(format!("{group:08x}"));
                }
                self.last = Some(v);
            }
            "SendTransaction" => {
                let r = rpc::SendResponse::decode(p)?;
                self.last =
                    Some(json!({ "error_code": r.error_code, "error_message": r.error_message }));
            }
            "GetMempoolTx" => {
                let t = rpc::CompactTx::decode(p)?;
                self.last = Some(json!({ "txid": display_hash(&t.txid) }));
            }
            "GetTaddressBalance" => {
                let b = rpc::Balance::decode(p)?;
                self.last = Some(json!({ "value_zat": b.value_zat }));
            }
            "GetAddressUtxos" => {
                let l = rpc::GetAddressUtxosReplyList::decode(p)?;
                self.last = Some(json!({ "utxos": l.address_utxos.len() }));
            }
            "GetAddressUtxosStream" => {
                rpc::GetAddressUtxosReply::decode(p)?;
            }
            _ => {}
        }
        Ok(())
    }

    pub fn to_json(&self) -> Value {
        let mut v = json!({ "messages": self.messages });
        if self.decode_errors > 0 {
            v["decode_errors"] = json!(self.decode_errors);
        }
        if let Some(b) = &self.blocks {
            v["blocks"] = b.to_json();
        }
        if let Some(s) = &self.subtrees {
            v["subtree_roots"] = json!({
                "count": s.roots,
                "first_completing_height": s.first_completing_height,
                "last_completing_height": s.last_completing_height,
            });
        }
        if let Some(l) = &self.last {
            v["value"] = l.clone();
        }
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_range_request_is_decoded() {
        let req = rpc::BlockRange {
            start: Some(rpc::BlockId {
                height: 10,
                hash: vec![],
            }),
            end: Some(rpc::BlockId {
                height: 20,
                hash: vec![],
            }),
            pool_types: vec![],
        };
        let v = request("GetBlockRange", &req.encode_to_vec());
        assert_eq!(v["start"], 10);
        assert_eq!(v["end"], 20);
    }

    #[test]
    fn block_stream_tracks_order_and_fields() {
        let mut r = Responses::default();
        for h in [5u64, 6, 8] {
            let b = rpc::CompactBlock {
                height: h,
                hash: vec![1; 32],
                vtx: vec![rpc::CompactTx {
                    txid: vec![2; 32],
                    actions: vec![rpc::CompactOrchardAction {
                        nullifier: vec![3; 32],
                        cmx: vec![4; 32],
                        ephemeral_key: vec![5; 32],
                        ciphertext: vec![6; 52],
                    }],
                    ..Default::default()
                }],
                ..Default::default()
            };
            r.observe("GetBlockRange", &b.encode_to_vec());
        }
        let v = r.to_json();
        assert_eq!(v["blocks"]["first_height"], 5);
        assert_eq!(v["blocks"]["last_height"], 8);
        assert_eq!(v["blocks"]["out_of_order"], 1);
        assert_eq!(
            v["blocks"]["fields_present"]["orchard_action.ciphertext"],
            3
        );
        assert!(v["blocks"]["fields_present"].get("block.header").is_none());
    }

    #[test]
    fn reads_v5_header() {
        // fOverwintered | 5, then the NU5 version group id 0x26A7270A.
        let raw = [0x05, 0x00, 0x00, 0x80, 0x0a, 0x27, 0xa7, 0x26];
        assert_eq!(tx_version(&raw), Some((5, 0x26a7_270a)));
        assert_eq!(tx_version(&raw[..3]), None);
    }

    #[test]
    fn garbage_is_counted_not_fatal() {
        let mut r = Responses::default();
        r.observe("GetLatestBlock", &[0xff, 0xff, 0xff]);
        assert_eq!(r.to_json()["decode_errors"], 1);
    }
}

//! ZIP 244 txid digests for v5 and v6 transactions, computed from a
//! verification record plus the fields a compact block carries.
//!
//! These follow `zcash_primitives` 0.30.0 (`transaction/txid.rs`) and
//! `orchard` 0.15.3 (`bundle/commitments.rs`) exactly. Tests in this crate
//! compare every result against `Transaction::txid()` from those crates.

use blake2b_simd::{Hash, Params, State};

pub const V5_TX_VERSION: u32 = 5;
pub const V5_VERSION_GROUP_ID: u32 = 0x26A7_270A;
pub const V6_TX_VERSION: u32 = 6;
pub const V6_VERSION_GROUP_ID: u32 = 0xD884_B698;

const TX_PERSONALIZATION_PREFIX: &[u8; 12] = b"ZcashTxHash_";
const HEADERS: &[u8; 16] = b"ZTxIdHeadersHash";
const TRANSPARENT: &[u8; 16] = b"ZTxIdTranspaHash";
const PREVOUTS: &[u8; 16] = b"ZTxIdPrevoutHash";
const OUTPUTS: &[u8; 16] = b"ZTxIdOutputsHash";
const SAPLING: &[u8; 16] = b"ZTxIdSaplingHash";
const SAPLING_SPENDS: &[u8; 16] = b"ZTxIdSSpendsHash";
const SAPLING_SPENDS_COMPACT: &[u8; 16] = b"ZTxIdSSpendCHash";
const SAPLING_OUTPUTS: &[u8; 16] = b"ZTxIdSOutputHash";
const SAPLING_OUTPUTS_COMPACT: &[u8; 16] = b"ZTxIdSOutC__Hash";

/// Personalizations for one Orchard-shaped bundle format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ActionsFormat {
    bundle: &'static [u8; 16],
    compact: &'static [u8; 16],
    /// v5 Orchard is the only format whose txid digest includes the anchor.
    pub anchor_in_txid: bool,
    pub name: &'static str,
}

pub const ORCHARD_V5: ActionsFormat = ActionsFormat {
    bundle: b"ZTxIdOrchardHash",
    compact: b"ZTxIdOrcActCHash",
    anchor_in_txid: true,
    name: "orchard",
};
pub const ORCHARD_V6: ActionsFormat = ActionsFormat {
    bundle: b"ZTxIdOrchardH_v6",
    compact: b"ZTxIdOrcActCHash",
    anchor_in_txid: false,
    name: "orchard",
};
pub const IRONWOOD_V6: ActionsFormat = ActionsFormat {
    bundle: b"ZTxIdIronwd_H_v6",
    compact: b"ZTxIdIrnActCH_v6",
    anchor_in_txid: false,
    name: "ironwood",
};

fn hasher(personal: &[u8; 16]) -> State {
    Params::new().hash_length(32).personal(personal).to_state()
}

/// T.1 header digest.
pub fn header_digest(header: u32, group: u32, branch: u32, lock_time: u32, expiry: u32) -> Hash {
    let mut h = hasher(HEADERS);
    for v in [header, group, branch, lock_time, expiry] {
        h.update(&v.to_le_bytes());
    }
    h.finalize()
}

/// Bitcoin CompactSize encoding.
pub fn compact_size(n: u64, out: &mut Vec<u8>) {
    match n {
        0..=0xfc => out.push(n as u8),
        0xfd..=0xffff => {
            out.push(0xfd);
            out.extend_from_slice(&(n as u16).to_le_bytes());
        }
        0x1_0000..=0xffff_ffff => {
            out.push(0xfe);
            out.extend_from_slice(&(n as u32).to_le_bytes());
        }
        _ => {
            out.push(0xff);
            out.extend_from_slice(&n.to_le_bytes());
        }
    }
}

/// T.2 transparent digest of a present bundle. `prevouts` must include the
/// coinbase null outpoint when the transaction is a coinbase (compact
/// blocks omit it). `outputs` are (value, scriptPubKey).
pub fn transparent_digest(
    prevouts: &[([u8; 32], u32)],
    sequence: &[u8; 32],
    outputs: &[(u64, &[u8])],
) -> Hash {
    let mut p = hasher(PREVOUTS);
    for (txid, n) in prevouts {
        p.update(txid);
        p.update(&n.to_le_bytes());
    }
    let mut o = hasher(OUTPUTS);
    let mut buf = Vec::new();
    for (value, script) in outputs {
        buf.clear();
        buf.extend_from_slice(&value.to_le_bytes());
        compact_size(script.len() as u64, &mut buf);
        buf.extend_from_slice(script);
        o.update(&buf);
    }
    let mut h = hasher(TRANSPARENT);
    h.update(p.finalize().as_bytes());
    h.update(sequence);
    h.update(o.finalize().as_bytes());
    h.finalize()
}

/// T.2 digest of an absent transparent bundle.
pub fn transparent_empty() -> Hash {
    hasher(TRANSPARENT).finalize()
}

/// Compact fields of one Sapling output, already length-checked.
pub struct SaplingOutputCompact<'a> {
    pub cmu: &'a [u8; 32],
    pub epk: &'a [u8; 32],
    pub ciphertext: &'a [u8; 52],
}

/// Compact fields of one Orchard or Ironwood action, already length-checked.
pub struct ActionCompact<'a> {
    pub nullifier: &'a [u8; 32],
    pub cmx: &'a [u8; 32],
    pub epk: &'a [u8; 32],
    pub ciphertext: &'a [u8; 52],
}

/// T.3 Sapling digest for a present bundle with at least one spend or
/// output.
pub fn sapling_digest(
    nullifiers: &[&[u8; 32]],
    spends_noncompact: &[u8; 32],
    outputs: &[SaplingOutputCompact<'_>],
    outputs_memos: &[u8; 32],
    outputs_noncompact: &[u8; 32],
    value_balance: i64,
) -> Hash {
    let mut spends = hasher(SAPLING_SPENDS);
    if !nullifiers.is_empty() {
        let mut c = hasher(SAPLING_SPENDS_COMPACT);
        for nf in nullifiers {
            c.update(*nf);
        }
        spends.update(c.finalize().as_bytes());
        spends.update(spends_noncompact);
    }

    let mut outs = hasher(SAPLING_OUTPUTS);
    if !outputs.is_empty() {
        let mut c = hasher(SAPLING_OUTPUTS_COMPACT);
        for o in outputs {
            c.update(o.cmu);
            c.update(o.epk);
            c.update(o.ciphertext);
        }
        outs.update(c.finalize().as_bytes());
        outs.update(outputs_memos);
        outs.update(outputs_noncompact);
    }

    let mut h = hasher(SAPLING);
    h.update(spends.finalize().as_bytes());
    h.update(outs.finalize().as_bytes());
    h.update(&value_balance.to_le_bytes());
    h.finalize()
}

/// T.3 digest of an absent Sapling bundle.
pub fn sapling_empty() -> Hash {
    hasher(SAPLING).finalize()
}

/// T.4 digest of a present Orchard or Ironwood bundle.
pub fn actions_digest(
    format: ActionsFormat,
    actions: &[ActionCompact<'_>],
    memos: &[u8; 32],
    noncompact: &[u8; 32],
    flags: u8,
    value_balance: i64,
    anchor: Option<&[u8; 32]>,
) -> Hash {
    let mut c = hasher(format.compact);
    for a in actions {
        c.update(a.nullifier);
        c.update(a.cmx);
        c.update(a.epk);
        c.update(a.ciphertext);
    }
    let mut h = hasher(format.bundle);
    h.update(c.finalize().as_bytes());
    h.update(memos);
    h.update(noncompact);
    h.update(&[flags]);
    h.update(&value_balance.to_le_bytes());
    if let Some(a) = anchor {
        h.update(a);
    }
    h.finalize()
}

/// T.4 digest of an absent Orchard or Ironwood bundle.
pub fn actions_empty(format: ActionsFormat) -> Hash {
    hasher(format.bundle).finalize()
}

/// Root of the txid digest tree. `ironwood` is `Some` exactly for v6.
pub fn txid(
    branch: u32,
    header: &Hash,
    transparent: &Hash,
    sapling: &Hash,
    orchard: &Hash,
    ironwood: Option<&Hash>,
) -> [u8; 32] {
    let mut personal = [0u8; 16];
    personal[..12].copy_from_slice(TX_PERSONALIZATION_PREFIX);
    personal[12..].copy_from_slice(&branch.to_le_bytes());
    let mut h = hasher(&personal);
    h.update(header.as_bytes());
    h.update(transparent.as_bytes());
    h.update(sapling.as_bytes());
    h.update(orchard.as_bytes());
    if let Some(i) = ironwood {
        h.update(i.as_bytes());
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(h.finalize().as_bytes());
    out
}

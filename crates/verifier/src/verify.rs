//! Block verification: bind every compact field to a txid, and every txid
//! to the trusted header's transaction Merkle root.

use std::collections::{BTreeMap, HashSet};
use std::io::Cursor;

use lwproto::record::{tx_record, ActionsRemainder, BlockRecord, PrunedTx, SaplingRemainder};
use lwproto::rpc::{
    CompactBlock, CompactOrchardAction, CompactSaplingOutput, CompactSaplingSpend, CompactTx,
    CompactTxIn, TxOut,
};
use zcash_primitives::transaction::Transaction;
use zcash_protocol::consensus::BranchId;

use crate::digest::{self, ActionCompact, ActionsFormat, SaplingOutputCompact};
use crate::merkle::{self, MerkleError};

/// Header data from the trusted validator. Hashes in protocol byte order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrustedHeader {
    pub height: u32,
    pub hash: [u8; 32],
    pub prev_hash: [u8; 32],
    pub merkle_root: [u8; 32],
    pub time: u32,
}

/// A block whose scanning data has been checked. `compact` is rebuilt from
/// verified values only; `chain_metadata` is left for the caller, which
/// tracks tree sizes across blocks.
#[derive(Clone, Debug, PartialEq)]
pub struct VerifiedBlock {
    pub compact: CompactBlock,
    pub txids: Vec<[u8; 32]>,
    pub sapling_outputs: u32,
    pub orchard_actions: u32,
    pub ironwood_actions: u32,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum VerifyError {
    #[error("record is for height {got}, expected {want}")]
    RecordHeight { got: u32, want: u32 },
    #[error("record block hash {got} does not match trusted hash {want}")]
    RecordHash { got: String, want: String },
    #[error("compact block {field} does not match the trusted header")]
    CompactHeader { field: &'static str },
    #[error("record lists no transactions")]
    EmptyRecord,
    #[error("transaction {tx}: malformed record: {reason}")]
    Malformed { tx: usize, reason: String },
    #[error("transaction {tx} has shielded data but the compact block omits it")]
    CompactTxMissing { tx: usize },
    #[error("compact block contains transaction index {index}, which does not exist in the block")]
    CompactTxUnexpected { index: u64 },
    #[error("compact block transactions are out of order or repeated at index {index}")]
    CompactOrder { index: u64 },
    #[error("transaction {tx}: compact block has {got} {what}, record says {want}")]
    CompactCount {
        tx: usize,
        what: &'static str,
        got: usize,
        want: usize,
    },
    #[error("transaction {tx}: {what} has wrong length {len}")]
    FieldLength {
        tx: usize,
        what: &'static str,
        len: usize,
    },
    #[error("transaction {tx}: compact {what} differs from the full transaction")]
    FieldMismatch { tx: usize, what: &'static str },
    #[error("transaction {tx}: compact block claims txid {claimed}, fields hash to {computed}")]
    TxidMismatch {
        tx: usize,
        claimed: String,
        computed: String,
    },
    #[error("duplicate txid {0}")]
    DuplicateTxid(String),
    #[error("transaction list is ambiguous (duplicate pair at level {level}, index {index})")]
    MutatedTree { level: usize, index: usize },
    #[error("transaction Merkle root {computed} does not match trusted header root {want}")]
    MerkleRoot { computed: String, want: String },
}

/// Hex in the usual display order (byte-reversed).
pub fn display(h: &[u8]) -> String {
    let mut v = h.to_vec();
    v.reverse();
    hex::encode(v)
}

fn arr32(tx: usize, what: &'static str, b: &[u8]) -> Result<[u8; 32], VerifyError> {
    b.try_into().map_err(|_| VerifyError::FieldLength {
        tx,
        what,
        len: b.len(),
    })
}

fn arr52(tx: usize, what: &'static str, b: &[u8]) -> Result<[u8; 52], VerifyError> {
    b.try_into().map_err(|_| VerifyError::FieldLength {
        tx,
        what,
        len: b.len(),
    })
}

fn malformed(tx: usize, reason: impl Into<String>) -> VerifyError {
    VerifyError::Malformed {
        tx,
        reason: reason.into(),
    }
}

/// Owned, length-checked copies of the compact fields of one transaction.
#[derive(Default)]
struct Fields {
    /// Transparent inputs as compact blocks carry them: the coinbase null
    /// outpoint is omitted.
    vin: Vec<([u8; 32], u32)>,
    vout: Vec<(u64, Vec<u8>)>,
    nullifiers: Vec<[u8; 32]>,
    outputs: Vec<([u8; 32], [u8; 32], [u8; 52])>,
    orchard: Vec<[[u8; 32]; 3]>,
    orchard_ct: Vec<[u8; 52]>,
    ironwood: Vec<[[u8; 32]; 3]>,
    ironwood_ct: Vec<[u8; 52]>,
}

impl Fields {
    fn from_compact(tx: usize, c: &CompactTx) -> Result<Self, VerifyError> {
        let mut f = Fields::default();
        for i in &c.vin {
            f.vin
                .push((arr32(tx, "prevout txid", &i.prevout_txid)?, i.prevout_index));
        }
        for o in &c.vout {
            f.vout.push((o.value, o.script_pub_key.clone()));
        }
        for s in &c.spends {
            f.nullifiers.push(arr32(tx, "sapling nullifier", &s.nf)?);
        }
        for o in &c.outputs {
            f.outputs.push((
                arr32(tx, "sapling cmu", &o.cmu)?,
                arr32(tx, "sapling ephemeral key", &o.ephemeral_key)?,
                arr52(tx, "sapling ciphertext", &o.ciphertext)?,
            ));
        }
        for (src, dst, ct) in [
            (&c.actions, &mut f.orchard, &mut f.orchard_ct),
            (&c.ironwood_actions, &mut f.ironwood, &mut f.ironwood_ct),
        ] {
            for a in src {
                dst.push([
                    arr32(tx, "action nullifier", &a.nullifier)?,
                    arr32(tx, "action cmx", &a.cmx)?,
                    arr32(tx, "action ephemeral key", &a.ephemeral_key)?,
                ]);
                ct.push(arr52(tx, "action ciphertext", &a.ciphertext)?);
            }
        }
        Ok(f)
    }

    fn is_empty(&self) -> bool {
        self.vin.is_empty()
            && self.vout.is_empty()
            && self.nullifiers.is_empty()
            && self.outputs.is_empty()
            && self.orchard.is_empty()
            && self.ironwood.is_empty()
    }

    fn to_compact(&self, index: usize, txid: &[u8; 32]) -> CompactTx {
        let action = |a: &[[u8; 32]; 3], ct: &[u8; 52]| CompactOrchardAction {
            nullifier: a[0].to_vec(),
            cmx: a[1].to_vec(),
            ephemeral_key: a[2].to_vec(),
            ciphertext: ct.to_vec(),
        };
        CompactTx {
            index: index as u64,
            txid: txid.to_vec(),
            fee: 0,
            spends: self
                .nullifiers
                .iter()
                .map(|nf| CompactSaplingSpend { nf: nf.to_vec() })
                .collect(),
            outputs: self
                .outputs
                .iter()
                .map(|(cmu, epk, ct)| CompactSaplingOutput {
                    cmu: cmu.to_vec(),
                    ephemeral_key: epk.to_vec(),
                    ciphertext: ct.to_vec(),
                })
                .collect(),
            actions: self
                .orchard
                .iter()
                .zip(&self.orchard_ct)
                .map(|(a, c)| action(a, c))
                .collect(),
            ironwood_actions: self
                .ironwood
                .iter()
                .zip(&self.ironwood_ct)
                .map(|(a, c)| action(a, c))
                .collect(),
            vin: self
                .vin
                .iter()
                .map(|(t, n)| CompactTxIn {
                    prevout_txid: t.to_vec(),
                    prevout_index: *n,
                })
                .collect(),
            vout: self
                .vout
                .iter()
                .map(|(v, s)| TxOut {
                    value: *v,
                    script_pub_key: s.clone(),
                })
                .collect(),
        }
    }

    fn has_shielded(&self) -> bool {
        !(self.nullifiers.is_empty()
            && self.outputs.is_empty()
            && self.orchard.is_empty()
            && self.ironwood.is_empty())
    }
}

/// Expected compact fields of a fully parsed transaction.
pub(crate) fn fields_of(tx: &Transaction) -> FieldsOwned {
    let mut f = Fields::default();
    if let Some(t) = tx.transparent_bundle() {
        if !t.is_coinbase() {
            for i in &t.vin {
                f.vin.push((*i.prevout().hash(), i.prevout().n()));
            }
        }
        for o in &t.vout {
            let mut enc = Vec::new();
            o.write(&mut enc).expect("writing to a Vec cannot fail");
            // value (8) || CompactSize(len) || script
            let (len, skip) = read_compact_size(&enc[8..]);
            f.vout.push((
                u64::from(o.value()),
                enc[8 + skip..8 + skip + len as usize].to_vec(),
            ));
        }
    }
    if let Some(b) = tx.sapling_bundle() {
        for s in b.shielded_spends() {
            let mut nf = [0u8; 32];
            nf.copy_from_slice(s.nullifier().as_ref());
            f.nullifiers.push(nf);
        }
        for o in b.shielded_outputs() {
            let mut ct = [0u8; 52];
            ct.copy_from_slice(&o.enc_ciphertext()[..52]);
            f.outputs
                .push((o.cmu().to_bytes(), o.ephemeral_key().0, ct));
        }
    }
    for (bundle, dst, cts) in [
        (tx.orchard_bundle(), &mut f.orchard, &mut f.orchard_ct),
        (tx.ironwood_bundle(), &mut f.ironwood, &mut f.ironwood_ct),
    ] {
        if let Some(b) = bundle {
            for a in b.actions().iter() {
                let n = a.encrypted_note();
                dst.push([a.nullifier().to_bytes(), a.cmx().to_bytes(), n.epk_bytes]);
                let mut ct = [0u8; 52];
                ct.copy_from_slice(&n.enc_ciphertext[..52]);
                cts.push(ct);
            }
        }
    }
    FieldsOwned(f)
}

fn read_compact_size(b: &[u8]) -> (u64, usize) {
    match b[0] {
        0xfd => (u64::from(u16::from_le_bytes([b[1], b[2]])), 3),
        0xfe => (u64::from(u32::from_le_bytes([b[1], b[2], b[3], b[4]])), 5),
        0xff => (u64::from_le_bytes(b[1..9].try_into().expect("8 bytes")), 9),
        v => (u64::from(v), 1),
    }
}

/// Opaque wrapper so the producer can build reference compact txs.
pub struct FieldsOwned(Fields);

impl FieldsOwned {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    pub fn has_shielded(&self) -> bool {
        self.0.has_shielded()
    }
    pub fn to_compact(&self, index: usize, txid: &[u8; 32]) -> CompactTx {
        self.0.to_compact(index, txid)
    }
}

fn check_count(tx: usize, what: &'static str, got: usize, want: u32) -> Result<(), VerifyError> {
    if got != want as usize {
        return Err(VerifyError::CompactCount {
            tx,
            what,
            got,
            want: want as usize,
        });
    }
    Ok(())
}

fn digest32(tx: usize, what: &'static str, b: &[u8]) -> Result<[u8; 32], VerifyError> {
    b.try_into()
        .map_err(|_| malformed(tx, format!("{what} must be 32 bytes, got {}", b.len())))
}

fn sapling_part(
    tx: usize,
    s: Option<&SaplingRemainder>,
    f: &Fields,
) -> Result<blake2b_simd::Hash, VerifyError> {
    let Some(s) = s else {
        if !f.nullifiers.is_empty() || !f.outputs.is_empty() {
            return Err(VerifyError::CompactCount {
                tx,
                what: "sapling spends/outputs",
                got: f.nullifiers.len() + f.outputs.len(),
                want: 0,
            });
        }
        return Ok(digest::sapling_empty());
    };
    if s.spends == 0 && s.outputs == 0 {
        return Err(malformed(
            tx,
            "sapling bundle with no spends and no outputs",
        ));
    }
    check_count(tx, "sapling spends", f.nullifiers.len(), s.spends)?;
    check_count(tx, "sapling outputs", f.outputs.len(), s.outputs)?;
    let zero = [0u8; 32];
    let spends_nc = if s.spends > 0 {
        digest32(tx, "spends_noncompact_digest", &s.spends_noncompact_digest)?
    } else if s.spends_noncompact_digest.is_empty() {
        zero
    } else {
        return Err(malformed(
            tx,
            "spends_noncompact_digest present without spends",
        ));
    };
    let (memos, nc) = if s.outputs > 0 {
        (
            digest32(tx, "outputs_memos_digest", &s.outputs_memos_digest)?,
            digest32(
                tx,
                "outputs_noncompact_digest",
                &s.outputs_noncompact_digest,
            )?,
        )
    } else if s.outputs_memos_digest.is_empty() && s.outputs_noncompact_digest.is_empty() {
        (zero, zero)
    } else {
        return Err(malformed(tx, "output digests present without outputs"));
    };
    let nfs: Vec<&[u8; 32]> = f.nullifiers.iter().collect();
    let outs: Vec<SaplingOutputCompact<'_>> = f
        .outputs
        .iter()
        .map(|(cmu, epk, ct)| SaplingOutputCompact {
            cmu,
            epk,
            ciphertext: ct,
        })
        .collect();
    Ok(digest::sapling_digest(
        &nfs,
        &spends_nc,
        &outs,
        &memos,
        &nc,
        s.value_balance,
    ))
}

fn actions_part(
    tx: usize,
    format: ActionsFormat,
    r: Option<&ActionsRemainder>,
    actions: &[[[u8; 32]; 3]],
    cts: &[[u8; 52]],
) -> Result<blake2b_simd::Hash, VerifyError> {
    let Some(r) = r else {
        if !actions.is_empty() {
            return Err(VerifyError::CompactCount {
                tx,
                what: format_actions(format),
                got: actions.len(),
                want: 0,
            });
        }
        return Ok(digest::actions_empty(format));
    };
    if r.actions == 0 {
        return Err(malformed(
            tx,
            format!("{} bundle with no actions", format.name),
        ));
    }
    check_count(tx, format_actions(format), actions.len(), r.actions)?;
    let flags = u8::try_from(r.flags).map_err(|_| malformed(tx, "flags out of range"))?;
    let memos = digest32(tx, "memos_digest", &r.memos_digest)?;
    let nc = digest32(tx, "noncompact_digest", &r.noncompact_digest)?;
    let anchor = if format.anchor_in_txid {
        Some(digest32(tx, "anchor", &r.anchor)?)
    } else if r.anchor.is_empty() {
        None
    } else {
        return Err(malformed(
            tx,
            format!("{} anchor is not part of this format's txid", format.name),
        ));
    };
    let compact: Vec<ActionCompact<'_>> = actions
        .iter()
        .zip(cts)
        .map(|(a, ct)| ActionCompact {
            nullifier: &a[0],
            cmx: &a[1],
            epk: &a[2],
            ciphertext: ct,
        })
        .collect();
    Ok(digest::actions_digest(
        format,
        &compact,
        &memos,
        &nc,
        flags,
        r.value_balance,
        anchor.as_ref(),
    ))
}

fn format_actions(f: ActionsFormat) -> &'static str {
    if f.name == "ironwood" {
        "ironwood actions"
    } else {
        "orchard actions"
    }
}

/// T.2 from the record's sequence digest and the compact vin/vout. The
/// coinbase (block position 0) gets its null outpoint back.
fn transparent_part(
    tx: usize,
    sequence: &[u8],
    f: &Fields,
) -> Result<blake2b_simd::Hash, VerifyError> {
    if sequence.is_empty() {
        if !f.vin.is_empty() || !f.vout.is_empty() {
            return Err(VerifyError::CompactCount {
                tx,
                what: "transparent inputs/outputs",
                got: f.vin.len() + f.vout.len(),
                want: 0,
            });
        }
        return Ok(digest::transparent_empty());
    }
    let sequence = digest32(tx, "sequence_digest", sequence)?;
    let mut prevouts = Vec::with_capacity(f.vin.len() + 1);
    if tx == 0 {
        prevouts.push(([0u8; 32], u32::MAX));
    }
    prevouts.extend_from_slice(&f.vin);
    let outputs: Vec<(u64, &[u8])> = f.vout.iter().map(|(v, s)| (*v, s.as_slice())).collect();
    Ok(digest::transparent_digest(&prevouts, &sequence, &outputs))
}

/// Txid of a pruned v5/v6 record given the compact fields.
fn pruned_txid(tx: usize, p: &PrunedTx, f: &Fields) -> Result<[u8; 32], VerifyError> {
    if p.header >> 31 != 1 {
        return Err(malformed(tx, "pruned form requires an overwintered header"));
    }
    let version = p.header & 0x7fff_ffff;
    let v6 = match (version, p.version_group_id) {
        (digest::V5_TX_VERSION, digest::V5_VERSION_GROUP_ID) => false,
        (digest::V6_TX_VERSION, digest::V6_VERSION_GROUP_ID) => true,
        _ => {
            return Err(malformed(
                tx,
                format!(
                    "pruned form supports v5 and v6 only, got version {version} group {:08x}",
                    p.version_group_id
                ),
            ))
        }
    };
    let transparent = transparent_part(tx, &p.sequence_digest, f)?;
    let header = digest::header_digest(
        p.header,
        p.version_group_id,
        p.consensus_branch_id,
        p.lock_time,
        p.expiry_height,
    );
    let sapling = sapling_part(tx, p.sapling.as_ref(), f)?;
    let orchard_fmt = if v6 {
        digest::ORCHARD_V6
    } else {
        digest::ORCHARD_V5
    };
    let orchard = actions_part(
        tx,
        orchard_fmt,
        p.orchard.as_ref(),
        &f.orchard,
        &f.orchard_ct,
    )?;
    let ironwood = if v6 {
        Some(actions_part(
            tx,
            digest::IRONWOOD_V6,
            p.ironwood.as_ref(),
            &f.ironwood,
            &f.ironwood_ct,
        )?)
    } else {
        if p.ironwood.is_some() || !f.ironwood.is_empty() {
            return Err(malformed(
                tx,
                "v5 transactions cannot carry Ironwood actions",
            ));
        }
        None
    };
    Ok(digest::txid(
        p.consensus_branch_id,
        &header,
        &transparent,
        &sapling,
        &orchard,
        ironwood.as_ref(),
    ))
}

/// Parses a raw transaction, rejecting trailing bytes.
pub fn parse_raw(tx: usize, raw: &[u8]) -> Result<Transaction, VerifyError> {
    let mut cur = Cursor::new(raw);
    // The branch id only matters for v4-and-earlier parsing bookkeeping;
    // v5/v6 encodings carry their own, and the txid of a v4 transaction is
    // a hash of its bytes.
    let parsed = Transaction::read(&mut cur, BranchId::Sapling)
        .map_err(|e| malformed(tx, format!("raw transaction does not parse: {e}")))?;
    if cur.position() as usize != raw.len() {
        return Err(malformed(
            tx,
            format!(
                "{} trailing bytes after raw transaction",
                raw.len() - cur.position() as usize
            ),
        ));
    }
    Ok(parsed)
}

fn compare_raw(tx: usize, expected: &Fields, got: &Fields) -> Result<(), VerifyError> {
    check_count(
        tx,
        "transparent inputs",
        got.vin.len(),
        expected.vin.len() as u32,
    )?;
    check_count(
        tx,
        "transparent outputs",
        got.vout.len(),
        expected.vout.len() as u32,
    )?;
    if got.vin != expected.vin {
        return Err(VerifyError::FieldMismatch {
            tx,
            what: "transparent input",
        });
    }
    if got.vout != expected.vout {
        return Err(VerifyError::FieldMismatch {
            tx,
            what: "transparent output",
        });
    }
    check_count(
        tx,
        "sapling spends",
        got.nullifiers.len(),
        expected.nullifiers.len() as u32,
    )?;
    check_count(
        tx,
        "sapling outputs",
        got.outputs.len(),
        expected.outputs.len() as u32,
    )?;
    check_count(
        tx,
        "orchard actions",
        got.orchard.len(),
        expected.orchard.len() as u32,
    )?;
    check_count(
        tx,
        "ironwood actions",
        got.ironwood.len(),
        expected.ironwood.len() as u32,
    )?;
    if got.nullifiers != expected.nullifiers {
        return Err(VerifyError::FieldMismatch {
            tx,
            what: "sapling nullifier",
        });
    }
    for (g, e) in got.outputs.iter().zip(&expected.outputs) {
        if g.0 != e.0 {
            return Err(VerifyError::FieldMismatch {
                tx,
                what: "sapling cmu",
            });
        }
        if g.1 != e.1 {
            return Err(VerifyError::FieldMismatch {
                tx,
                what: "sapling ephemeral key",
            });
        }
        if g.2 != e.2 {
            return Err(VerifyError::FieldMismatch {
                tx,
                what: "sapling ciphertext",
            });
        }
    }
    for (ga, gc, ea, ec) in [
        (
            &got.orchard,
            &got.orchard_ct,
            &expected.orchard,
            &expected.orchard_ct,
        ),
        (
            &got.ironwood,
            &got.ironwood_ct,
            &expected.ironwood,
            &expected.ironwood_ct,
        ),
    ] {
        for i in 0..ga.len() {
            for (k, what) in ["action nullifier", "action cmx", "action ephemeral key"]
                .iter()
                .enumerate()
            {
                if ga[i][k] != ea[i][k] {
                    return Err(VerifyError::FieldMismatch { tx, what });
                }
            }
            if gc[i] != ec[i] {
                return Err(VerifyError::FieldMismatch {
                    tx,
                    what: "action ciphertext",
                });
            }
        }
    }
    Ok(())
}

/// Verifies one block.
///
/// `record` and `compact` come from untrusted sources; `header` from the
/// trusted validator. On success every compact field in the returned block
/// is bound to a txid that is bound to `header.merkle_root`.
pub fn verify_block(
    header: &TrustedHeader,
    record: &BlockRecord,
    compact: &CompactBlock,
) -> Result<VerifiedBlock, VerifyError> {
    if record.height != header.height {
        return Err(VerifyError::RecordHeight {
            got: record.height,
            want: header.height,
        });
    }
    if record.block_hash.as_slice() != header.hash {
        return Err(VerifyError::RecordHash {
            got: display(&record.block_hash),
            want: display(&header.hash),
        });
    }
    if compact.height != u64::from(header.height) {
        return Err(VerifyError::CompactHeader { field: "height" });
    }
    if compact.hash.as_slice() != header.hash {
        return Err(VerifyError::CompactHeader { field: "hash" });
    }
    if !compact.prev_hash.is_empty() && compact.prev_hash.as_slice() != header.prev_hash {
        return Err(VerifyError::CompactHeader { field: "prev_hash" });
    }
    if record.txs.is_empty() {
        return Err(VerifyError::EmptyRecord);
    }

    // Compact transactions keyed by their claimed index, which must be
    // strictly increasing and inside the block.
    let mut by_index: BTreeMap<usize, &CompactTx> = BTreeMap::new();
    let mut last: Option<u64> = None;
    for c in &compact.vtx {
        if last.is_some_and(|l| c.index <= l) {
            return Err(VerifyError::CompactOrder { index: c.index });
        }
        last = Some(c.index);
        if c.index as usize >= record.txs.len() {
            return Err(VerifyError::CompactTxUnexpected { index: c.index });
        }
        by_index.insert(c.index as usize, c);
    }

    let mut txids = Vec::with_capacity(record.txs.len());
    let mut vtx = Vec::new();
    let (mut sapling_outputs, mut orchard_actions, mut ironwood_actions) = (0u32, 0u32, 0u32);

    for (i, tr) in record.txs.iter().enumerate() {
        let c = by_index.get(&i).copied();
        let fields = match c {
            Some(c) => Fields::from_compact(i, c)?,
            None => Fields::default(),
        };
        let txid = match &tr.body {
            Some(tx_record::Body::Pruned(p)) => {
                let shielded = p.sapling.is_some() || p.orchard.is_some() || p.ironwood.is_some();
                if shielded && c.is_none() {
                    return Err(VerifyError::CompactTxMissing { tx: i });
                }
                pruned_txid(i, p, &fields)?
            }
            Some(tx_record::Body::Raw(raw)) => {
                let parsed = parse_raw(i, raw)?;
                let expected = fields_of(&parsed).0;
                if expected.has_shielded() && c.is_none() {
                    return Err(VerifyError::CompactTxMissing { tx: i });
                }
                compare_raw(i, &expected, &fields)?;
                *parsed.txid().as_ref()
            }
            None => return Err(malformed(i, "empty transaction record")),
        };
        if let Some(c) = c {
            if c.txid.as_slice() != txid {
                return Err(VerifyError::TxidMismatch {
                    tx: i,
                    claimed: display(&c.txid),
                    computed: display(&txid),
                });
            }
            if !fields.is_empty() {
                sapling_outputs += fields.outputs.len() as u32;
                orchard_actions += fields.orchard.len() as u32;
                ironwood_actions += fields.ironwood.len() as u32;
                vtx.push(fields.to_compact(i, &txid));
            }
        }
        txids.push(txid);
    }

    let mut seen = HashSet::with_capacity(txids.len());
    for t in &txids {
        if !seen.insert(*t) {
            return Err(VerifyError::DuplicateTxid(display(t)));
        }
    }
    let root = merkle::root(&txids).map_err(|e| match e {
        MerkleError::Empty => VerifyError::EmptyRecord,
        MerkleError::Mutated { level, index } => VerifyError::MutatedTree { level, index },
    })?;
    if root != header.merkle_root {
        return Err(VerifyError::MerkleRoot {
            computed: display(&root),
            want: display(&header.merkle_root),
        });
    }

    Ok(VerifiedBlock {
        compact: CompactBlock {
            height: u64::from(header.height),
            hash: header.hash.to_vec(),
            prev_hash: header.prev_hash.to_vec(),
            time: header.time,
            header: vec![],
            vtx,
            chain_metadata: None,
        },
        txids,
        sapling_outputs,
        orchard_actions,
        ironwood_actions,
    })
}

/// Txid of one record entry given its compact transaction (if any).
/// Exposed for tests and tooling; [`verify_block`] does this per tx.
pub fn txid_from_record(
    tx: usize,
    record: &lwproto::record::TxRecord,
    compact: Option<&CompactTx>,
) -> Result<[u8; 32], VerifyError> {
    let fields = match compact {
        Some(c) => Fields::from_compact(tx, c)?,
        None => Fields::default(),
    };
    match &record.body {
        Some(tx_record::Body::Pruned(p)) => pruned_txid(tx, p, &fields),
        Some(tx_record::Body::Raw(raw)) => {
            let parsed = parse_raw(tx, raw)?;
            compare_raw(tx, &fields_of(&parsed).0, &fields)?;
            Ok(*parsed.txid().as_ref())
        }
        None => Err(malformed(tx, "empty transaction record")),
    }
}

/// The compact transaction an honest indexer would serve for `tx` at
/// position `index` with all pools requested, or `None` if it has no
/// compact data at all.
pub fn reference_compact(tx: &Transaction, index: usize) -> Option<CompactTx> {
    let f = fields_of(tx);
    (!f.is_empty()).then(|| f.to_compact(index, tx.txid().as_ref()))
}

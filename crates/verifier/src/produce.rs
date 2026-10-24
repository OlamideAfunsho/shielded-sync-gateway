//! Producer side: full block in, verification record out.
//!
//! This is the half that runs next to untrusted infrastructure. It parses
//! full transactions with `zcash_primitives` and computes the digest parts
//! the compact block lacks. Nothing here is trusted by the gateway; a
//! producer that lies is caught by [`crate::verify::verify_block`].

use std::io::{Cursor, Read};

use blake2b_simd::{Params, State};
use lwproto::record::{
    tx_record, ActionsRemainder, BlockRecord, PrunedTx, SaplingRemainder, TxRecord,
};
use lwproto::rpc::CompactBlock;
use sha2::{Digest, Sha256};
use zcash_primitives::transaction::txid::TxIdDigester;
use zcash_primitives::transaction::{Transaction, TxVersion};

use crate::verify::{fields_of, parse_raw, TrustedHeader};

#[derive(Debug, thiserror::Error)]
pub enum ProduceError {
    #[error("block is truncated or malformed: {0}")]
    Block(String),
    #[error("transaction {0}: {1}")]
    Tx(usize, String),
}

/// The 80-ish byte header fields we need, plus the hash.
#[derive(Clone, Debug)]
pub struct ParsedHeader {
    pub hash: [u8; 32],
    pub prev_hash: [u8; 32],
    pub merkle_root: [u8; 32],
    pub time: u32,
}

pub struct ParsedBlock {
    pub header: ParsedHeader,
    pub txs: Vec<(Vec<u8>, Transaction)>,
}

fn read_compact_size(r: &mut Cursor<&[u8]>) -> Result<u64, ProduceError> {
    let mut b = [0u8; 1];
    r.read_exact(&mut b)
        .map_err(|e| ProduceError::Block(e.to_string()))?;
    let n = match b[0] {
        0xfd => {
            let mut x = [0u8; 2];
            r.read_exact(&mut x)
                .map_err(|e| ProduceError::Block(e.to_string()))?;
            u64::from(u16::from_le_bytes(x))
        }
        0xfe => {
            let mut x = [0u8; 4];
            r.read_exact(&mut x)
                .map_err(|e| ProduceError::Block(e.to_string()))?;
            u64::from(u32::from_le_bytes(x))
        }
        0xff => {
            let mut x = [0u8; 8];
            r.read_exact(&mut x)
                .map_err(|e| ProduceError::Block(e.to_string()))?;
            u64::from_le_bytes(x)
        }
        v => u64::from(v),
    };
    Ok(n)
}

/// Splits a serialized Zcash block into header fields and transactions.
pub fn parse_block(bytes: &[u8]) -> Result<ParsedBlock, ProduceError> {
    // version(4) prev(32) merkle(32) commitments(32) time(4) bits(4)
    // nonce(32) then the Equihash solution with a CompactSize length.
    const FIXED: usize = 4 + 32 + 32 + 32 + 4 + 4 + 32;
    if bytes.len() < FIXED + 1 {
        return Err(ProduceError::Block("shorter than a header".into()));
    }
    let mut cur = Cursor::new(bytes);
    cur.set_position(FIXED as u64);
    let sol_len = read_compact_size(&mut cur)? as usize;
    let header_end = cur.position() as usize + sol_len;
    if header_end > bytes.len() {
        return Err(ProduceError::Block("solution runs past the end".into()));
    }
    let header_bytes = &bytes[..header_end];
    let hash: [u8; 32] = Sha256::digest(Sha256::digest(header_bytes)).into();
    let mut prev_hash = [0u8; 32];
    prev_hash.copy_from_slice(&bytes[4..36]);
    let mut merkle_root = [0u8; 32];
    merkle_root.copy_from_slice(&bytes[36..68]);
    let time = u32::from_le_bytes(bytes[100..104].try_into().expect("4 bytes"));

    cur.set_position(header_end as u64);
    let count = read_compact_size(&mut cur)? as usize;
    let mut txs = Vec::with_capacity(count.min(10_000));
    for i in 0..count {
        let start = cur.position() as usize;
        let rest = &bytes[start..];
        let mut sub = Cursor::new(rest);
        let tx = Transaction::read(&mut sub, zcash_protocol::consensus::BranchId::Sapling)
            .map_err(|e| ProduceError::Tx(i, e.to_string()))?;
        let len = sub.position() as usize;
        txs.push((rest[..len].to_vec(), tx));
        cur.set_position((start + len) as u64);
    }
    if cur.position() as usize != bytes.len() {
        return Err(ProduceError::Block(format!(
            "{} trailing bytes after {count} transactions",
            bytes.len() - cur.position() as usize
        )));
    }
    Ok(ParsedBlock {
        header: ParsedHeader {
            hash,
            prev_hash,
            merkle_root,
            time,
        },
        txs,
    })
}

fn hasher(p: &[u8; 16]) -> State {
    Params::new().hash_length(32).personal(p).to_state()
}

/// T.2b sequence digest, or empty without a transparent bundle.
fn sequence_digest(tx: &Transaction) -> Vec<u8> {
    match tx.digest(TxIdDigester).transparent_digests {
        Some(t) => t.sequence_digest.as_bytes().to_vec(),
        None => vec![],
    }
}

fn sapling_remainder(tx: &Transaction) -> Option<SaplingRemainder> {
    let b = tx.sapling_bundle()?;
    let v6 = matches!(tx.version(), TxVersion::V6);
    let spends = b.shielded_spends();
    let outputs = b.shielded_outputs();
    let spends_nc = if spends.is_empty() {
        vec![]
    } else {
        let mut h = hasher(if v6 {
            b"ZTxIdSSpendNH_v6"
        } else {
            b"ZTxIdSSpendNHash"
        });
        for s in spends {
            h.update(&s.cv().to_bytes());
            if !v6 {
                h.update(&ff::PrimeField::to_repr(s.anchor()));
            }
            h.update(&<[u8; 32]>::from(*s.rk()));
        }
        h.finalize().as_bytes().to_vec()
    };
    let (memos, nc) = if outputs.is_empty() {
        (vec![], vec![])
    } else {
        let mut m = hasher(b"ZTxIdSOutM__Hash");
        let mut n = hasher(b"ZTxIdSOutN__Hash");
        for o in outputs {
            m.update(&o.enc_ciphertext()[52..564]);
            n.update(&o.cv().to_bytes());
            n.update(&o.enc_ciphertext()[564..]);
            n.update(&o.out_ciphertext()[..]);
        }
        (
            m.finalize().as_bytes().to_vec(),
            n.finalize().as_bytes().to_vec(),
        )
    };
    Some(SaplingRemainder {
        spends: spends.len() as u32,
        outputs: outputs.len() as u32,
        spends_noncompact_digest: spends_nc,
        outputs_memos_digest: memos,
        outputs_noncompact_digest: nc,
        value_balance: i64::from(*b.value_balance()),
    })
}

fn actions_remainder(
    bundle: Option<
        &orchard::Bundle<orchard::bundle::Authorized, zcash_protocol::value::ZatBalance>,
    >,
    memos_p: &[u8; 16],
    nc_p: &[u8; 16],
    with_anchor: bool,
) -> Option<ActionsRemainder> {
    let b = bundle?;
    let mut m = hasher(memos_p);
    let mut n = hasher(nc_p);
    for a in b.actions().iter() {
        let note = a.encrypted_note();
        m.update(&note.enc_ciphertext[52..564]);
        n.update(&a.cv_net().to_bytes());
        n.update(&<[u8; 32]>::from(a.rk()));
        n.update(&note.enc_ciphertext[564..]);
        n.update(&note.out_ciphertext);
    }
    Some(ActionsRemainder {
        actions: b.actions().len() as u32,
        memos_digest: m.finalize().as_bytes().to_vec(),
        noncompact_digest: n.finalize().as_bytes().to_vec(),
        flags: u32::from(b.flag_byte()),
        value_balance: i64::from(*b.value_balance()),
        anchor: if with_anchor {
            b.anchor().to_bytes().to_vec()
        } else {
            vec![]
        },
    })
}

/// Record entry for one transaction: pruned for v5/v6, raw otherwise.
pub fn tx_record(raw: &[u8], tx: &Transaction) -> TxRecord {
    let body = match tx.version() {
        TxVersion::V5 | TxVersion::V6 => {
            let v6 = matches!(tx.version(), TxVersion::V6);
            tx_record::Body::Pruned(PrunedTx {
                header: tx.version().header(),
                version_group_id: tx.version().version_group_id(),
                consensus_branch_id: u32::from(tx.consensus_branch_id()),
                lock_time: tx.lock_time(),
                expiry_height: u32::from(tx.expiry_height()),
                sequence_digest: sequence_digest(tx),
                sapling: sapling_remainder(tx),
                // Orchard reuses the same action-level personalizations in
                // v5 and v6; only v5 puts the anchor in the txid.
                orchard: actions_remainder(
                    tx.orchard_bundle(),
                    b"ZTxIdOrcActMHash",
                    b"ZTxIdOrcActNHash",
                    !v6,
                ),
                ironwood: actions_remainder(
                    tx.ironwood_bundle(),
                    b"ZTxIdIrnActMH_v6",
                    b"ZTxIdIrnActNH_v6",
                    false,
                ),
            })
        }
        _ => tx_record::Body::Raw(raw.to_vec()),
    };
    TxRecord { body: Some(body) }
}

/// Builds the record for a block, the header as the block itself states it,
/// and the compact block an honest indexer would serve.
pub fn produce(
    height: u32,
    block: &[u8],
) -> Result<(BlockRecord, TrustedHeader, CompactBlock), ProduceError> {
    let parsed = parse_block(block)?;
    let mut txs = Vec::with_capacity(parsed.txs.len());
    let mut vtx = Vec::new();
    for (i, (raw, tx)) in parsed.txs.iter().enumerate() {
        txs.push(tx_record(raw, tx));
        let fields = fields_of(tx);
        if !fields.is_empty() {
            vtx.push(fields.to_compact(i, tx.txid().as_ref()));
        }
    }
    let h = &parsed.header;
    let record = BlockRecord {
        height,
        block_hash: h.hash.to_vec(),
        txs,
    };
    let header = TrustedHeader {
        height,
        hash: h.hash,
        prev_hash: h.prev_hash,
        merkle_root: h.merkle_root,
        time: h.time,
    };
    let compact = CompactBlock {
        height: u64::from(height),
        hash: h.hash.to_vec(),
        prev_hash: h.prev_hash.to_vec(),
        time: h.time,
        header: vec![],
        vtx,
        chain_metadata: None,
    };
    Ok((record, header, compact))
}

/// Re-parses a raw transaction from a record and returns its txid. Used by
/// tests and by `GetTransaction` authentication in the gateway.
pub fn raw_txid(raw: &[u8]) -> Result<[u8; 32], crate::verify::VerifyError> {
    Ok(*parse_raw(0, raw)?.txid().as_ref())
}

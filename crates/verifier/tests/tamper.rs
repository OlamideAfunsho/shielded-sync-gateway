//! Every way of altering scanning data that the plan names must be
//! rejected: changed ciphertexts, ephemeral keys, commitments and
//! nullifiers; omitted, extra, reordered and duplicated transactions;
//! malformed and lying records.

use lwproto::record::{tx_record, BlockRecord};
use lwproto::rpc::CompactBlock;
use ssg_verifier::produce::produce;
use ssg_verifier::{verify_block, TrustedHeader, VerifyError};

fn regtest_block(height: u32) -> Vec<u8> {
    let text = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/regtest-v6.jsonl"
    ))
    .unwrap();
    for line in text.lines() {
        let j: serde_json::Value = serde_json::from_str(line).unwrap();
        if j["height"] == height {
            return hex::decode(j["raw"].as_str().unwrap()).unwrap();
        }
    }
    panic!("no regtest block {height}");
}

fn mainnet_block(name: &str) -> Vec<u8> {
    let p = format!(
        "{}/tests/fixtures/mainnet/{name}.hex",
        env!("CARGO_MANIFEST_DIR")
    );
    hex::decode(std::fs::read_to_string(p).unwrap().trim()).unwrap()
}

/// Regtest height 114: coinbase plus a v6 payment with 2 Sapling outputs
/// and 2 Ironwood actions.
/// Position in `vtx` of the payment transaction (block index 1).
const PAY: usize = 1;

fn v6_case() -> (BlockRecord, TrustedHeader, CompactBlock) {
    let (r, h, c) = produce(114, &regtest_block(114)).unwrap();
    assert_eq!(
        c.vtx[0].index, 0,
        "coinbase carries transparent outputs, so it is in the compact block"
    );
    let tx = &c.vtx[PAY];
    assert_eq!((tx.outputs.len(), tx.ironwood_actions.len()), (2, 2));
    verify_block(&h, &r, &c).expect("honest block verifies");
    (r, h, c)
}

/// Mainnet 1,687,107: v5 transactions with Orchard actions.
fn v5_case() -> (BlockRecord, TrustedHeader, CompactBlock) {
    let (r, h, c) = produce(1_687_107, &mainnet_block("block-main-1-687-107")).unwrap();
    assert!(
        c.vtx.iter().any(|t| !t.actions.is_empty()),
        "fixture must contain Orchard actions"
    );
    verify_block(&h, &r, &c).expect("honest block verifies");
    (r, h, c)
}

/// Mainnet 419,201: v4 transactions with Sapling spends and outputs
/// (raw path).
fn v4_case() -> (BlockRecord, TrustedHeader, CompactBlock) {
    let (r, h, c) = produce(419_201, &mainnet_block("block-main-0-419-201")).unwrap();
    assert!(r
        .txs
        .iter()
        .all(|t| matches!(t.body, Some(tx_record::Body::Raw(_)))));
    assert!(c.vtx.iter().any(|t| !t.outputs.is_empty()));
    verify_block(&h, &r, &c).expect("honest block verifies");
    (r, h, c)
}

/// A v4 block with Sapling spends, found by scanning the fixtures.
fn v4_spends_case() -> (BlockRecord, TrustedHeader, CompactBlock) {
    for (name, height) in [
        ("block-main-0-419-202", 419_202),
        ("block-main-0-653-600", 653_600),
        ("block-main-0-903-000", 903_000),
        ("block-main-1-046-400", 1_046_400),
    ] {
        let (r, h, c) = produce(height, &mainnet_block(name)).unwrap();
        if c.vtx.iter().any(|t| !t.spends.is_empty())
            && r.txs
                .iter()
                .all(|t| matches!(t.body, Some(tx_record::Body::Raw(_))))
        {
            verify_block(&h, &r, &c).expect("honest block verifies");
            return (r, h, c);
        }
    }
    panic!("no v4 fixture with Sapling spends");
}

fn flip(b: &mut [u8], i: usize) {
    b[i] ^= 0x01;
}

/// Mutations that keep commitments intact must still be caught.
#[test]
fn altered_ciphertext_and_ephemeral_keys() {
    for (name, (r, h, c)) in [("v6", v6_case()), ("v5", v5_case()), ("v4", v4_case())] {
        let target = c
            .vtx
            .iter()
            .position(|t| {
                !t.outputs.is_empty() || !t.actions.is_empty() || !t.ironwood_actions.is_empty()
            })
            .unwrap();
        let muts: Vec<(&str, Mutation)> = vec![
            (
                "sapling ciphertext",
                Box::new(move |c: &mut CompactBlock| {
                    if let Some(o) = c.vtx[target].outputs.first_mut() {
                        flip(&mut o.ciphertext, 10)
                    } else {
                        flip(&mut first_action(c, target).ciphertext, 10)
                    }
                }),
            ),
            (
                "ephemeral key",
                Box::new(move |c: &mut CompactBlock| {
                    if let Some(o) = c.vtx[target].outputs.first_mut() {
                        flip(&mut o.ephemeral_key, 0)
                    } else {
                        flip(&mut first_action(c, target).ephemeral_key, 0)
                    }
                }),
            ),
            (
                "action ciphertext (last byte)",
                Box::new(move |c: &mut CompactBlock| {
                    if let Some(a) = any_action(c) {
                        flip(&mut a.ciphertext, 51)
                    } else {
                        flip(&mut c.vtx[target].outputs[0].ciphertext, 51)
                    }
                }),
            ),
            (
                "note commitment",
                Box::new(move |c: &mut CompactBlock| {
                    if let Some(o) = c.vtx[target].outputs.first_mut() {
                        flip(&mut o.cmu, 5)
                    } else {
                        flip(&mut first_action(c, target).cmx, 5)
                    }
                }),
            ),
        ];
        for (what, m) in muts {
            let mut bad = c.clone();
            m(&mut bad);
            let err =
                verify_block(&h, &r, &bad).expect_err(&format!("{name}: {what} must be rejected"));
            assert!(
                matches!(
                    err,
                    VerifyError::TxidMismatch { .. } | VerifyError::FieldMismatch { .. }
                ),
                "{name}: {what}: {err}"
            );
        }
    }
}

type Mutation = Box<dyn Fn(&mut CompactBlock)>;

fn first_action(c: &mut CompactBlock, target: usize) -> &mut lwproto::rpc::CompactOrchardAction {
    let tx = &mut c.vtx[target];
    if !tx.actions.is_empty() {
        &mut tx.actions[0]
    } else {
        &mut tx.ironwood_actions[0]
    }
}

fn any_action(c: &mut CompactBlock) -> Option<&mut lwproto::rpc::CompactOrchardAction> {
    c.vtx
        .iter_mut()
        .find_map(|t| t.ironwood_actions.first_mut().or(t.actions.first_mut()))
}

#[test]
fn altered_nullifiers() {
    // v6 Ironwood action nullifier.
    let (r, h, mut c) = v6_case();
    flip(&mut c.vtx[PAY].ironwood_actions[1].nullifier, 31);
    assert!(matches!(
        verify_block(&h, &r, &c),
        Err(VerifyError::TxidMismatch { tx: 1, .. })
    ));

    // v4 Sapling spend nullifier (raw path).
    let (r, h, mut c) = v4_spends_case();
    let t = c.vtx.iter().position(|t| !t.spends.is_empty()).unwrap();
    flip(&mut c.vtx[t].spends[0].nf, 0);
    assert!(matches!(
        verify_block(&h, &r, &c),
        Err(VerifyError::FieldMismatch {
            what: "sapling nullifier",
            ..
        })
    ));

    // v5 Orchard action nullifier.
    let (r, h, mut c) = v5_case();
    let t = c.vtx.iter().position(|t| !t.actions.is_empty()).unwrap();
    flip(&mut c.vtx[t].actions[0].nullifier, 7);
    assert!(matches!(
        verify_block(&h, &r, &c),
        Err(VerifyError::TxidMismatch { .. })
    ));
}

#[test]
fn indexer_that_also_fixes_up_the_claimed_txid_is_caught_by_the_root() {
    // A careful attacker recomputes the txid for the altered fields and
    // puts that in the compact block, so the per-tx check passes. The
    // block's Merkle root still exposes it.
    let (r, h, mut c) = v6_case();
    flip(&mut c.vtx[PAY].outputs[0].ciphertext, 3);
    let forged = ssg_verifier::verify::txid_from_record(1, &r.txs[1], Some(&c.vtx[PAY])).unwrap();
    c.vtx[PAY].txid = forged.to_vec();
    assert!(matches!(
        verify_block(&h, &r, &c),
        Err(VerifyError::MerkleRoot { .. })
    ));
}

#[test]
fn omitted_and_extra_compact_transactions() {
    let (r, h, c) = v6_case();
    let mut omitted = c.clone();
    omitted.vtx.clear();
    assert_eq!(
        verify_block(&h, &r, &omitted),
        Err(VerifyError::CompactTxMissing { tx: 1 })
    );

    // Dropping one output inside a transaction.
    let mut short = c.clone();
    short.vtx[PAY].outputs.pop();
    assert!(matches!(
        verify_block(&h, &r, &short),
        Err(VerifyError::CompactCount {
            what: "sapling outputs",
            ..
        })
    ));

    // A second entry claiming the coinbase position.
    let mut extra = c.clone();
    let mut fake = c.vtx[PAY].clone();
    fake.index = 0;
    extra.vtx.insert(0, fake);
    assert!(verify_block(&h, &r, &extra).is_err());

    // Index past the end of the block.
    let mut past = c.clone();
    let last = past.vtx.len() - 1;
    past.vtx[last].index = 9;
    assert_eq!(
        verify_block(&h, &r, &past),
        Err(VerifyError::CompactTxUnexpected { index: 9 })
    );
}

#[test]
fn reordered_compact_transactions() {
    let (r, h, c) = v5_case();
    assert!(c.vtx.len() >= 2);
    let mut swapped = c.clone();
    swapped.vtx.swap(0, 1);
    assert!(matches!(
        verify_block(&h, &r, &swapped),
        Err(VerifyError::CompactOrder { .. })
    ));

    // Same order, indices swapped: the fields now land on the wrong txs.
    let mut relabelled = c.clone();
    let (a, b) = (relabelled.vtx[0].index, relabelled.vtx[1].index);
    relabelled.vtx[0].index = b;
    relabelled.vtx[1].index = a;
    assert!(verify_block(&h, &r, &relabelled).is_err());
}

#[test]
fn record_omissions_reorderings_and_duplicates() {
    let (r, h, c) = v5_case();

    // A record (and compact block) that leave out the last transaction.
    // Every remaining transaction still checks out on its own; only the
    // Merkle root shows something is missing.
    let victim = r.txs.len() - 1;
    let mut dropped = r.clone();
    dropped.txs.remove(victim);
    let mut c2 = c.clone();
    c2.vtx.retain(|t| t.index as usize != victim);
    assert!(matches!(
        verify_block(&h, &dropped, &c2),
        Err(VerifyError::MerkleRoot { .. })
    ));

    // The last two transactions swapped in both the record and the compact
    // block, consistently re-indexed.
    let n = r.txs.len();
    let mut swapped = r.clone();
    swapped.txs.swap(n - 2, n - 1);
    let mut c3 = c.clone();
    for t in &mut c3.vtx {
        if t.index as usize == n - 2 {
            t.index = (n - 1) as u64;
        } else if t.index as usize == n - 1 {
            t.index = (n - 2) as u64;
        }
    }
    c3.vtx.sort_by_key(|t| t.index);
    assert!(matches!(
        verify_block(&h, &swapped, &c3),
        Err(VerifyError::MerkleRoot { .. })
    ));

    // The last transaction repeated: the CVE-2012-2459 shape when the
    // count is odd, a duplicate txid otherwise. Either way, rejected.
    let mut dup = r.clone();
    dup.txs.push(r.txs.last().unwrap().clone());
    let mut dup_c = c.clone();
    let last = r.txs.len() - 1;
    if let Some(ct) = c.vtx.iter().find(|t| t.index as usize == last) {
        let mut copy = ct.clone();
        copy.index = r.txs.len() as u64;
        dup_c.vtx.push(copy);
    }
    let err = verify_block(&h, &dup, &dup_c).unwrap_err();
    assert!(
        matches!(
            err,
            VerifyError::DuplicateTxid(_) | VerifyError::MutatedTree { .. }
        ),
        "{err}"
    );
}

fn pruned(r: &mut BlockRecord) -> &mut lwproto::record::PrunedTx {
    match r.txs[1].body.as_mut().unwrap() {
        tx_record::Body::Pruned(p) => p,
        _ => unreachable!(),
    }
}

#[test]
fn lying_or_malformed_records() {
    let (r, h, c) = v6_case();

    // A memo digest that is not the real one.
    let mut bad = r.clone();
    flip(
        &mut pruned(&mut bad)
            .sapling
            .as_mut()
            .unwrap()
            .outputs_memos_digest,
        0,
    );
    assert!(matches!(
        verify_block(&h, &bad, &c),
        Err(VerifyError::TxidMismatch { .. })
    ));

    // Hiding the Ironwood bundle.
    let mut hidden = r.clone();
    pruned(&mut hidden).ironwood = None;
    assert!(matches!(
        verify_block(&h, &hidden, &c),
        Err(VerifyError::CompactCount {
            what: "ironwood actions",
            ..
        })
    ));

    // Wrong digest length.
    let mut short = r.clone();
    pruned(&mut short).sequence_digest = vec![0; 31];
    assert!(matches!(
        verify_block(&h, &short, &c),
        Err(VerifyError::Malformed { tx: 1, .. })
    ));

    // Unsupported version in the pruned form.
    let mut v4 = r.clone();
    pruned(&mut v4).header = 0x8000_0004;
    assert!(matches!(
        verify_block(&h, &v4, &c),
        Err(VerifyError::Malformed { .. })
    ));

    // A raw record that does not parse, or has trailing bytes.
    let (r4, h4, c4) = v4_case();
    let mut trailing = r4.clone();
    if let Some(tx_record::Body::Raw(b)) = trailing.txs[0].body.as_mut() {
        b.push(0);
    }
    assert!(matches!(
        verify_block(&h4, &trailing, &c4),
        Err(VerifyError::Malformed { tx: 0, .. })
    ));

    // Wrong height or block hash.
    let mut wrong = r.clone();
    wrong.height += 1;
    assert!(matches!(
        verify_block(&h, &wrong, &c),
        Err(VerifyError::RecordHeight { .. })
    ));
    let mut wrong = r.clone();
    flip(&mut wrong.block_hash, 0);
    assert!(matches!(
        verify_block(&h, &wrong, &c),
        Err(VerifyError::RecordHash { .. })
    ));

    // A compact block from a different chain (stale fork).
    let mut fork = c.clone();
    flip(&mut fork.hash, 0);
    assert_eq!(
        verify_block(&h, &r, &fork),
        Err(VerifyError::CompactHeader { field: "hash" })
    );
}

#[test]
fn altered_transparent_data() {
    // v6 coinbase output value.
    let (r, h, mut c) = v6_case();
    c.vtx[0].vout[0].value += 1;
    assert!(matches!(
        verify_block(&h, &r, &c),
        Err(VerifyError::TxidMismatch { tx: 0, .. })
    ));

    // NU5-era block: a transparent input's prevout.
    let (r, h, mut c) = v5_case();
    let t = c
        .vtx
        .iter()
        .position(|t| !t.vin.is_empty())
        .expect("fixture has transparent inputs");
    c.vtx[t].vin[0].prevout_index ^= 1;
    // The block mixes v4 (raw path) and v5 (pruned path) transactions.
    let err = verify_block(&h, &r, &c).unwrap_err();
    assert!(
        matches!(
            err,
            VerifyError::TxidMismatch { .. }
                | VerifyError::FieldMismatch {
                    what: "transparent input",
                    ..
                }
        ),
        "{err}"
    );

    // v4 raw path: an output script.
    let (r, h, mut c) = v4_case();
    c.vtx[0].vout[0].script_pub_key.push(0x51);
    assert!(matches!(
        verify_block(&h, &r, &c),
        Err(VerifyError::FieldMismatch {
            what: "transparent output",
            ..
        })
    ));
}

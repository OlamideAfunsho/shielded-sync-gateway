//! Reconstructed txids and Merkle roots against real chain data.
//!
//! * Mainnet blocks (Zebra's test vectors): v1, v3, v4 through the raw
//!   path, v5 through the pruned path. Reference: the Merkle root in each
//!   block's header.
//! * A regtest chain mined by zebrad 6.3.0 after NU6.3: v6 transactions,
//!   Ironwood actions, and a v6 transaction with Sapling outputs.
//!   Reference: the header zebrad reports for each height.
//! * ZIP 244 test vectors (zcash-test-vectors, via zcash_primitives):
//!   v5 transactions with every combination of bundles.

use std::collections::BTreeMap;

use lwproto::record::tx_record;
use ssg_verifier::produce::{parse_block, produce};
use ssg_verifier::verify::{display, txid_from_record};
use ssg_verifier::{merkle, verify_block};

fn fixture(path: &str) -> String {
    std::fs::read_to_string(format!(
        "{}/tests/fixtures/{path}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

#[derive(Default, Debug)]
struct Census {
    raw_by_version: BTreeMap<u32, usize>,
    pruned_by_version: BTreeMap<u32, usize>,
    sapling_outputs: usize,
    sapling_spends: usize,
    orchard_actions: usize,
    ironwood_actions: usize,
    v6_with_sapling: usize,
    v5_with_orchard: usize,
}

fn check_block(height: u32, bytes: &[u8], census: &mut Census) -> [u8; 32] {
    let parsed = parse_block(bytes).unwrap();
    let (record, header, compact) = produce(height, bytes).unwrap();

    // Every txid we rebuild from record + compact fields must equal the
    // txid zcash_primitives computes from the full transaction.
    let compact_by_index: BTreeMap<u64, _> = compact.vtx.iter().map(|c| (c.index, c)).collect();
    for (i, ((_, tx), rec)) in parsed.txs.iter().zip(&record.txs).enumerate() {
        let rebuilt = txid_from_record(i, rec, compact_by_index.get(&(i as u64)).copied()).unwrap();
        assert_eq!(rebuilt, *tx.txid().as_ref(), "height {height} tx {i}");
        let version = tx.version().header() & 0x7fff_ffff;
        match &rec.body {
            Some(tx_record::Body::Raw(_)) => {
                *census.raw_by_version.entry(version).or_default() += 1
            }
            Some(tx_record::Body::Pruned(p)) => {
                *census.pruned_by_version.entry(version).or_default() += 1;
                if version == 6 && p.sapling.is_some() {
                    census.v6_with_sapling += 1;
                }
                if version == 5 && p.orchard.is_some() {
                    census.v5_with_orchard += 1;
                }
            }
            None => unreachable!(),
        }
    }
    for c in &compact.vtx {
        census.sapling_outputs += c.outputs.len();
        census.sapling_spends += c.spends.len();
        census.orchard_actions += c.actions.len();
        census.ironwood_actions += c.ironwood_actions.len();
    }

    let txids: Vec<[u8; 32]> = parsed.txs.iter().map(|(_, t)| *t.txid().as_ref()).collect();
    assert_eq!(
        merkle::root(&txids).unwrap(),
        parsed.header.merkle_root,
        "height {height}"
    );

    let verified = verify_block(&header, &record, &compact).unwrap();
    assert_eq!(verified.txids, txids);
    assert_eq!(verified.compact.vtx, compact.vtx);
    parsed.header.hash
}

#[test]
fn mainnet_blocks() {
    let dir = format!("{}/tests/fixtures/mainnet", env!("CARGO_MANIFEST_DIR"));
    let mut names: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| n.ends_with(".hex"))
        .collect();
    names.sort();
    assert!(names.len() >= 15);
    let mut census = Census::default();
    for n in &names {
        let height: u32 = n
            .trim_start_matches("block-main-")
            .trim_end_matches(".hex")
            .replace('-', "")
            .parse()
            .unwrap();
        let bytes = hex::decode(fixture(&format!("mainnet/{n}")).trim()).unwrap();
        check_block(height, &bytes, &mut census);
    }
    println!("{census:#?}");
    // The fixtures must exercise every path they claim to.
    assert!(census.raw_by_version.contains_key(&1));
    assert!(census.raw_by_version.contains_key(&3));
    assert!(census.raw_by_version.contains_key(&4));
    assert!(census.pruned_by_version.contains_key(&5));
    assert!(census.sapling_outputs > 0 && census.sapling_spends > 0);
}

#[test]
fn regtest_v6_chain_matches_zebrad_headers() {
    let mut census = Census::default();
    let mut blocks = 0;
    for line in fixture("regtest-v6.jsonl").lines() {
        let j: serde_json::Value = serde_json::from_str(line).unwrap();
        let height = j["height"].as_u64().unwrap() as u32;
        let bytes = hex::decode(j["raw"].as_str().unwrap()).unwrap();
        let hash = check_block(height, &bytes, &mut census);
        let parsed = parse_block(&bytes).unwrap();
        assert_eq!(display(&hash), j["hash"].as_str().unwrap());
        assert_eq!(
            display(&parsed.header.merkle_root),
            j["merkleroot"].as_str().unwrap()
        );
        assert_eq!(
            display(&parsed.header.prev_hash),
            j["previousblockhash"].as_str().unwrap()
        );
        assert_eq!(u64::from(parsed.header.time), j["time"].as_u64().unwrap());
        blocks += 1;
    }
    println!("{census:#?}");
    assert_eq!(blocks, 122);
    assert!(census.raw_by_version.is_empty(), "every regtest tx is v6");
    assert_eq!(census.pruned_by_version.get(&6), Some(&126));
    assert!(census.ironwood_actions > 0);
    // Corroborated non-empty v6 Sapling: the txid rebuilt from the
    // Sapling compact fields matches zcash_primitives, and the block's root
    // matches what zebrad reports.
    assert!(census.v6_with_sapling > 0);
    assert!(census.sapling_outputs > 0);
}

#[test]
fn zip244_test_vectors() {
    use zcash_primitives::transaction::tests::data::zip_0244;
    use zcash_primitives::transaction::Transaction;
    use zcash_protocol::consensus::BranchId;

    let mut with = [0usize; 3];
    for (n, tv) in zip_0244::make_test_vectors().iter().enumerate() {
        let tx = Transaction::read(&tv.tx[..], BranchId::Nu5).unwrap();
        assert_eq!(tx.txid().as_ref(), &tv.txid, "vector {n}");
        let rec = ssg_verifier::produce::tx_record(&tv.tx, &tx);
        // Block position matters only for coinbase-shaped transactions,
        // whose null input compact blocks omit; those must sit at 0.
        let pos = if tx.transparent_bundle().is_some_and(|t| t.is_coinbase()) {
            0
        } else {
            n + 1
        };
        let compact = ssg_verifier::verify::reference_compact(&tx, pos);
        let rebuilt = txid_from_record(pos, &rec, compact.as_ref()).unwrap();
        assert_eq!(rebuilt, tv.txid, "vector {n}");
        if let Some(tx_record::Body::Pruned(p)) = &rec.body {
            with[0] += p.sapling.as_ref().map_or(0, |s| (s.spends > 0) as usize);
            with[1] += p.sapling.as_ref().map_or(0, |s| (s.outputs > 0) as usize);
            with[2] += p.orchard.is_some() as usize;
        }
    }
    println!("vectors with sapling spends/outputs, orchard: {with:?}");
    assert!(with.iter().all(|&c| c > 0));
}

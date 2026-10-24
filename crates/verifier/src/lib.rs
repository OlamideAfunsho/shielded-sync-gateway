//! Verifier for Zcash light-wallet scanning data.
//!
//! A light wallet scans compact blocks: per transaction, Sapling nullifiers
//! and (cmu, ephemeral key, first 52 ciphertext bytes) per output, and
//! (nullifier, cmx, ephemeral key, first 52 ciphertext bytes) per Orchard or
//! Ironwood action. Nothing in that format lets the wallet check those
//! values. This crate does: given a verification record for the block and a
//! trusted header, it recomputes each transaction's ZIP 244 txid from the
//! compact fields plus the record, recomputes the transaction Merkle root
//! from every txid, and accepts the block only if that root equals the
//! header's.
//!
//! * [`verify::verify_block`] — the check.
//! * [`produce::produce`] — builds records from full blocks (producer side).
//! * [`digest`], [`merkle`] — the hashing rules.

pub mod digest;
pub mod merkle;
pub mod produce;
pub mod verify;

pub use verify::{verify_block, TrustedHeader, VerifiedBlock, VerifyError};

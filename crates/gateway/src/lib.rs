//! Shielded sync gateway: a local CompactTxStreamer endpoint that sits
//! between a Zcash light wallet and its indexer.
//!
//! Two modes:
//!
//! * `serve` — the verifying gateway. Compact blocks from an untrusted
//!   indexer and records from an untrusted producer are checked against
//!   headers from a trusted validator; the wallet is answered only from
//!   verified data ([`follower`], [`server`], [`store`]).
//! * `proxy` — a transparent recorder used to inventory a wallet's calls
//!   and, with a rules file, to corrupt compact blocks on purpose
//!   ([`proxy`], [`tamper`]).

pub mod call;
pub mod checkpoint;
pub mod describe;
pub mod follower;
pub mod grpc;
pub mod probe;
pub mod proxy;
pub mod server;
pub mod store;
pub mod summarize;
pub mod tamper;
pub mod trusted;

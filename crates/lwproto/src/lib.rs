//! Message types for the lightwallet gRPC protocol, generated from the
//! `walletrpc` definitions vendored in `proto/` (zcash/lightwallet-protocol,
//! as shipped with zaino 0.10.1).

// Generated code; doc comments are copied from the .proto files verbatim.
#[allow(clippy::doc_overindented_list_items, clippy::doc_lazy_continuation)]
pub mod rpc {
    include!(concat!(env!("OUT_DIR"), "/cash.z.wallet.sdk.rpc.rs"));
}

/// gRPC path prefix for every CompactTxStreamer method.
pub const SERVICE_PATH: &str = "/cash.z.wallet.sdk.rpc.CompactTxStreamer/";

/// Verification record types (`proto/ssg/record.proto`).
// `Pruned` is much larger than `Raw` (a Vec); records are short-lived, so
// boxing would only add indirection.
#[allow(clippy::large_enum_variant)]
pub mod record {
    include!(concat!(env!("OUT_DIR"), "/ssg.record.v1.rs"));
}

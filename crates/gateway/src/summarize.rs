//! Reduces a JSONL call log to the RPC inventory used in docs/step1.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use anyhow::Context as _;
use serde_json::Value;

use crate::call::CallRecord;

pub fn load(path: &Path) -> anyhow::Result<Vec<CallRecord>> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .enumerate()
        .map(|(i, l)| serde_json::from_str(l).with_context(|| format!("line {}", i + 1)))
        .collect()
}

#[derive(Default)]
struct PerMethod {
    calls: u64,
    ok: u64,
    /// Streams the wallet hung up on (no grpc-status, no error), e.g. the
    /// mempool stream when a session ends.
    cancelled: u64,
    req_bytes: u64,
    resp_bytes: u64,
    resp_msgs: u64,
    total_ms: f64,
}

pub fn markdown(records: &[CallRecord]) -> String {
    // The log is written in completion order; long-lived streams (the
    // mempool stream) finish last. Report in the order calls were made.
    let mut sorted = records.to_vec();
    sorted.sort_by_key(|r| r.id);
    let records = &sorted[..];
    let mut by_method: BTreeMap<&str, PerMethod> = BTreeMap::new();
    for r in records {
        let m = by_method.entry(&r.method).or_default();
        m.calls += 1;
        if r.grpc_status == Some(0) {
            m.ok += 1;
        } else if r.grpc_status.is_none() && r.transport_error.is_none() {
            m.cancelled += 1;
        }
        m.req_bytes += r.request_bytes;
        m.resp_bytes += r.response_bytes;
        m.resp_msgs += r.response_messages;
        m.total_ms += r.duration_ms;
    }

    let mut s = String::new();
    let total_resp: u64 = by_method.values().map(|m| m.resp_bytes).sum();
    let total_req: u64 = by_method.values().map(|m| m.req_bytes).sum();
    let _ = writeln!(s, "| method | calls | ok | cancelled by wallet | request bytes | response bytes | response msgs | time (ms) |");
    let _ = writeln!(s, "|---|---:|---:|---:|---:|---:|---:|---:|");
    for (name, m) in &by_method {
        let _ = writeln!(
            s,
            "| `{name}` | {} | {} | {} | {} | {} | {} | {:.1} |",
            m.calls, m.ok, m.cancelled, m.req_bytes, m.resp_bytes, m.resp_msgs, m.total_ms
        );
    }
    let _ = writeln!(
        s,
        "| **total** | {} | | | {total_req} | {total_resp} | | |",
        records.len()
    );

    // Block coverage and the fields the wallet was handed.
    let mut fields: BTreeMap<String, u64> = BTreeMap::new();
    let mut ranges = Vec::new();
    let mut out_of_order = 0u64;
    for r in records
        .iter()
        .filter(|r| r.method == "GetBlockRange" || r.method == "GetBlock")
    {
        let b = &r.response["blocks"];
        if let Some(obj) = b["fields_present"].as_object() {
            for (k, v) in obj {
                *fields.entry(k.clone()).or_default() += v.as_u64().unwrap_or(0);
            }
        }
        out_of_order += b["out_of_order"].as_u64().unwrap_or(0);
        ranges.push(format!(
            "{}..={} ({} blocks)",
            r.request["start"]
                .as_u64()
                .map(|v| v.to_string())
                .unwrap_or_else(|| r.request["block"].to_string()),
            r.request["end"]
                .as_u64()
                .map(|v| v.to_string())
                .unwrap_or_default(),
            r.response["messages"]
        ));
    }
    if !ranges.is_empty() {
        let _ = writeln!(s, "\nBlock requests: {}", ranges.join(", "));
        let _ = writeln!(s, "Non-consecutive heights inside a range: {out_of_order}");
        let _ = writeln!(s, "\n| compact field | times present |\n|---|---:|");
        for (k, v) in &fields {
            let _ = writeln!(s, "| `{k}` | {v} |");
        }
    }

    // Consecutive calls with the same method and the same response shape
    // (e.g. one GetTaddressTxids per transparent address) are folded into
    // one row so the sequence stays readable.
    let _ = writeln!(s, "\nCall sequence:\n");
    let _ = writeln!(s, "| # | method | request | response |");
    let _ = writeln!(s, "|---|---|---|---|");
    let mut i = 0;
    while i < records.len() {
        let r = &records[i];
        let resp = compact_response(&r.response);
        let mut j = i + 1;
        while j < records.len()
            && records[j].method == r.method
            && compact_response(&records[j].response) == resp
            && records[j].grpc_status == r.grpc_status
            && r.method != "GetBlockRange"
        {
            j += 1;
        }
        let ids = if j - i > 1 {
            format!("{}–{} (×{})", r.id, records[j - 1].id, j - i)
        } else {
            r.id.to_string()
        };
        let request = if j - i > 1 {
            format!("{} … (first shown)", r.request)
        } else {
            r.request.to_string()
        };
        let _ = writeln!(
            s,
            "| {ids} | `{}` | `{request}` | {}{} |",
            r.method,
            resp.replace('|', "\\|"),
            status_note(r)
        );
        i = j;
    }
    s
}

fn status_note(r: &CallRecord) -> String {
    match (r.grpc_status, &r.grpc_message, &r.transport_error) {
        (Some(0), _, _) => String::new(),
        (Some(c), Some(m), _) => format!(" (status {c}: {m})"),
        (Some(c), None, _) => format!(" (status {c})"),
        (None, _, Some(e)) => format!(" (error: {e})"),
        (None, _, None) => " (stream closed by wallet)".into(),
    }
}

fn compact_response(v: &Value) -> String {
    if let Some(b) = v.get("blocks") {
        return format!(
            "{} blocks {}..={}, {} txs; sapling {} spends / {} outputs; orchard {} actions; ironwood {} actions",
            v["messages"], b["first_height"], b["last_height"], b["txs"], b["sapling_spends"],
            b["sapling_outputs"], b["orchard_actions"], b["ironwood_actions"]
        );
    }
    if let Some(st) = v.get("subtree_roots") {
        return format!("{} subtree roots", st["count"]);
    }
    match v.get("value") {
        Some(x) => format!("`{x}`"),
        None => format!("{} messages", v["messages"]),
    }
}

//! ssg-producer: the untrusted half of the verification pipeline.
//!
//! Reads full blocks from a Zcash node's JSON-RPC (`getblock <h> 0`) and
//! serves a verification record per block over HTTP:
//!
//!   GET /v1/record/<height>[?hash=<display hex>]   -> BlockRecord (protobuf)
//!
//! The gateway trusts nothing it gets from here; a wrong record makes
//! verification fail. `--tamper-file` exists so that failure can be
//! demonstrated: it makes this producer lie on purpose.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{anyhow, Context};
use bytes::Bytes;
use clap::{Parser, Subcommand};
use http::{Request, Response, StatusCode};
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use lwproto::record::{tx_record, BlockRecord};
use prost::Message;
use serde_json::json;
use tokio::net::TcpListener;
use tracing::{info, warn};

#[derive(Parser)]
#[command(
    name = "ssg-producer",
    version,
    about = "Serves verification records built from full Zcash blocks"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Serve records over HTTP.
    Serve {
        /// JSON-RPC endpoint of the node to read full blocks from.
        #[arg(long, default_value = "http://127.0.0.1:18232")]
        source: String,
        #[arg(long, default_value = "127.0.0.1:9180")]
        listen: SocketAddr,
        /// Lines of `<height> <kind>` to falsify on purpose (testing only).
        /// Kinds: memo-digest, drop-tx, swap-txs. Re-read on every request.
        #[arg(long)]
        tamper_file: Option<PathBuf>,
    },
    /// Print a record summary for one height as JSON.
    Show {
        #[arg(long, default_value = "http://127.0.0.1:18232")]
        source: String,
        height: u32,
    },
}

struct Source {
    url: String,
    http: reqwest::Client,
}

impl Source {
    async fn call(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
        let v: serde_json::Value = self
            .http
            .post(&self.url)
            .json(&body)
            .send()
            .await?
            .json()
            .await?;
        if let Some(e) = v.get("error").filter(|e| !e.is_null()) {
            return Err(anyhow!("{method}: {e}"));
        }
        Ok(v["result"].clone())
    }

    async fn record(&self, height: u32, hash: Option<&str>) -> anyhow::Result<BlockRecord> {
        let id = hash
            .map(str::to_string)
            .unwrap_or_else(|| height.to_string());
        let hexblock = self.call("getblock", json!([id, 0])).await?;
        let bytes = hex::decode(
            hexblock
                .as_str()
                .ok_or_else(|| anyhow!("getblock returned no hex"))?,
        )?;
        let (record, _, _) =
            ssg_verifier::produce::produce(height, &bytes).map_err(|e| anyhow!("{e}"))?;
        Ok(record)
    }
}

fn tamper(record: &mut BlockRecord, file: &Option<PathBuf>) -> Option<String> {
    let text = std::fs::read_to_string(file.as_ref()?).ok()?;
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        let (Some(h), Some(kind)) = (parts.next(), parts.next()) else {
            continue;
        };
        if h.parse::<u32>().ok() != Some(record.height) {
            continue;
        }
        match kind {
            "memo-digest" => {
                for t in &mut record.txs {
                    if let Some(tx_record::Body::Pruned(p)) = t.body.as_mut() {
                        if let Some(r) = p.ironwood.as_mut().or(p.orchard.as_mut()) {
                            r.memos_digest[0] ^= 1;
                            return Some(kind.into());
                        }
                        if let Some(s) = p.sapling.as_mut().filter(|s| s.outputs > 0) {
                            s.outputs_memos_digest[0] ^= 1;
                            return Some(kind.into());
                        }
                    }
                }
            }
            "drop-tx" if record.txs.len() > 1 => {
                record.txs.pop();
                return Some(kind.into());
            }
            "swap-txs" if record.txs.len() > 1 => {
                let n = record.txs.len();
                record.txs.swap(n - 2, n - 1);
                return Some(kind.into());
            }
            _ => {}
        }
    }
    None
}

async fn handle(
    src: Arc<Source>,
    tamper_file: Arc<Option<PathBuf>>,
    req: Request<Incoming>,
) -> Result<Response<Full<Bytes>>, std::convert::Infallible> {
    let path = req.uri().path().to_string();
    let hash = req
        .uri()
        .query()
        .and_then(|q| q.split('&').find_map(|kv| kv.strip_prefix("hash=")))
        .map(str::to_string);
    let reply = |status: StatusCode, body: Bytes| {
        Ok(Response::builder()
            .status(status)
            .header("content-type", "application/x-protobuf")
            .body(Full::new(body))
            .expect("static response parts"))
    };
    let Some(h) = path
        .strip_prefix("/v1/record/")
        .and_then(|h| h.parse::<u32>().ok())
    else {
        return reply(
            StatusCode::NOT_FOUND,
            Bytes::from_static(b"use /v1/record/<height>"),
        );
    };
    match src.record(h, hash.as_deref()).await {
        Ok(mut r) => {
            if let Some(kind) = tamper(&mut r, &tamper_file) {
                warn!(height = h, kind, "serving a deliberately falsified record");
            }
            reply(StatusCode::OK, Bytes::from(r.encode_to_vec()))
        }
        Err(e) => {
            warn!(height = h, "no record: {e:#}");
            reply(
                StatusCode::SERVICE_UNAVAILABLE,
                Bytes::from(format!("{e:#}")),
            )
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_writer(std::io::stderr)
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stderr()))
        .with_target(false)
        .init();
    let http = reqwest::Client::builder().no_proxy().build()?;
    match Cli::parse().cmd {
        Cmd::Show { source, height } => {
            let src = Source { url: source, http };
            let r = src.record(height, None).await?;
            let txs: Vec<_> = r
                .txs
                .iter()
                .map(|t| match &t.body {
                    Some(tx_record::Body::Raw(b)) => json!({"raw_bytes": b.len()}),
                    Some(tx_record::Body::Pruned(p)) => json!({
                        "version": p.header & 0x7fff_ffff,
                        "sapling": p.sapling.as_ref().map(|s| json!({"spends": s.spends, "outputs": s.outputs})),
                        "orchard_actions": p.orchard.as_ref().map(|o| o.actions),
                        "ironwood_actions": p.ironwood.as_ref().map(|o| o.actions),
                    }),
                    None => json!(null),
                })
                .collect();
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "height": r.height,
                    "hash": ssg_verifier::verify::display(&r.block_hash),
                    "record_bytes": r.encoded_len(),
                    "txs": txs,
                }))?
            );
            Ok(())
        }
        Cmd::Serve {
            source,
            listen,
            tamper_file,
        } => {
            let src = Arc::new(Source {
                url: source.clone(),
                http,
            });
            let tamper_file = Arc::new(tamper_file);
            let listener = TcpListener::bind(listen)
                .await
                .with_context(|| format!("binding {listen}"))?;
            info!(%listen, %source, "producer serving records");
            loop {
                let (stream, _) = listener.accept().await?;
                let (src, tf) = (src.clone(), tamper_file.clone());
                tokio::spawn(async move {
                    let svc = service_fn(move |req| handle(src.clone(), tf.clone(), req));
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(TokioIo::new(stream), svc)
                        .await;
                });
            }
        }
    }
}

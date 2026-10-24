use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use clap::{Parser, Subcommand};
use lwproto::rpc::compact_tx_streamer_client::CompactTxStreamerClient;
use lwproto::rpc::compact_tx_streamer_server::CompactTxStreamerServer;
use ssg_gateway::checkpoint::Checkpoint;
use ssg_gateway::follower::Shared;
use ssg_gateway::server::Gateway;
use ssg_gateway::store::Store;
use ssg_gateway::trusted::Trusted;
use ssg_gateway::{probe, proxy, summarize};
use tracing::info;

#[derive(Parser)]
#[command(
    name = "ssg-gateway",
    version,
    about = "Shielded sync gateway for Zcash light wallets"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the verifying gateway.
    Serve {
        /// Address the wallet connects to.
        #[arg(long, default_value = "127.0.0.1:9168")]
        listen: SocketAddr,
        /// Untrusted indexer serving compact blocks (gRPC, h2c).
        #[arg(long, default_value = "http://127.0.0.1:8137")]
        indexer: String,
        /// Untrusted producer serving verification records (HTTP).
        #[arg(long, default_value = "http://127.0.0.1:9180")]
        producer: String,
        /// Trusted validator (zebrad JSON-RPC).
        #[arg(long, default_value = "http://127.0.0.1:18232")]
        trusted: String,
        /// Pinned checkpoint (see `checkpoint`).
        #[arg(long)]
        checkpoint: PathBuf,
        /// Verified-chain database. Reused across restarts.
        #[arg(long)]
        state: PathBuf,
        /// Rewritten with the follower's status after every change.
        #[arg(long)]
        status_file: Option<PathBuf>,
        /// How long a wallet request waits for a block to be verified.
        #[arg(long, default_value_t = 30)]
        wait_secs: u64,
        /// Blocks verified per follower pass.
        #[arg(long, default_value_t = 200)]
        batch: u32,
    },
    /// Write a checkpoint for `height` from the trusted validator.
    Checkpoint {
        #[arg(long, default_value = "http://127.0.0.1:18232")]
        trusted: String,
        #[arg(long)]
        height: u32,
        #[arg(long)]
        out: PathBuf,
    },
    /// Forward wallet traffic to an indexer unchanged and log every call.
    Proxy {
        /// Address the wallet connects to.
        #[arg(long, default_value = "127.0.0.1:9168")]
        listen: SocketAddr,
        /// Indexer gRPC endpoint (zainod or lightwalletd), plaintext h2c.
        #[arg(long, default_value = "http://127.0.0.1:8137")]
        upstream: http::Uri,
        /// Append one JSON object per completed call to this file.
        #[arg(long)]
        record: Option<PathBuf>,
        /// Corrupt compact blocks per this rules file (testing only; see
        /// the tamper module docs).
        #[arg(long)]
        tamper_file: Option<PathBuf>,
    },
    /// Print a markdown RPC inventory from a call log.
    Summarize { log: PathBuf },
    /// Print the compact block an indexer serves for `height` (all pools),
    /// next to the one built from the full block, as JSON.
    Inspect {
        #[arg(long, default_value = "http://127.0.0.1:8137")]
        indexer: String,
        #[arg(long, default_value = "http://127.0.0.1:18232")]
        node: String,
        height: u32,
    },
    /// Print the tip height reported by an indexer (GetLatestBlock).
    Tip {
        #[arg(default_value = "http://127.0.0.1:8137")]
        server: http::Uri,
    },
}

#[allow(clippy::too_many_arguments)]
async fn serve(
    listen: SocketAddr,
    indexer: String,
    producer: String,
    trusted: String,
    checkpoint: PathBuf,
    state: PathBuf,
    status_file: Option<PathBuf>,
    wait: Duration,
    batch: u32,
) -> anyhow::Result<()> {
    let trusted = Trusted::new(trusted)?;
    let cp = Checkpoint::load(&checkpoint)?;
    cp.check_against(&trusted)
        .await
        .context("checkpoint check")?;
    let store = Store::open(&state, cp.clone())?;
    let resumed = store.tip()?;
    info!(
        checkpoint = cp.height,
        resumed_at = resumed.height,
        "verified chain loaded{}",
        if resumed.height > cp.height {
            " (resuming after restart)"
        } else {
            ""
        }
    );
    let channel = tonic::transport::Endpoint::from_shared(indexer.clone())?.connect_lazy();
    let upstream = CompactTxStreamerClient::new(channel)
        .max_decoding_message_size(64 << 20)
        .max_encoding_message_size(64 << 20);
    let shared = Shared::new(
        store,
        trusted,
        upstream,
        producer.trim_end_matches('/').to_string(),
    )?;

    tokio::spawn(shared.clone().run(Duration::from_millis(500), batch));
    if let Some(path) = status_file {
        let s = shared.clone();
        tokio::spawn(async move {
            let mut rx = s.changed.subscribe();
            loop {
                let json = serde_json::to_string_pretty(&s.status()).unwrap_or_default();
                let tmp = path.with_extension("tmp");
                if std::fs::write(&tmp, json).is_ok() {
                    let _ = std::fs::rename(&tmp, &path);
                }
                if rx.changed().await.is_err() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        });
    }

    let svc = CompactTxStreamerServer::new(Gateway { shared, wait })
        .max_decoding_message_size(64 << 20)
        .max_encoding_message_size(64 << 20);
    info!(%listen, %indexer, %producer, "verifying gateway listening");
    tonic::transport::Server::builder()
        .add_service(svc)
        .serve_with_shutdown(listen, async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_writer(std::io::stderr)
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stderr()))
        .with_target(false)
        .init();

    let rt = || {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
    };
    match Cli::parse().cmd {
        Cmd::Serve { listen, indexer, producer, trusted, checkpoint, state, status_file, wait_secs, batch } => rt()?
            .block_on(serve(
                listen,
                indexer,
                producer,
                trusted,
                checkpoint,
                state,
                status_file,
                Duration::from_secs(wait_secs),
                batch,
            )),
        Cmd::Checkpoint { trusted, height, out } => rt()?.block_on(async {
            let cp = Checkpoint::from_trusted(&Trusted::new(trusted)?, height).await?;
            std::fs::write(&out, serde_json::to_string_pretty(&cp)?)?;
            println!("checkpoint at {} ({}) written to {}", cp.height, cp.hash, out.display());
            Ok(())
        }),
        Cmd::Proxy { listen, upstream, record, tamper_file } => {
            rt()?.block_on(proxy::run(proxy::Config { listen, upstream, record, tamper_file }))
        }
        Cmd::Inspect { indexer, node, height } => rt()?.block_on(async move {
            use lwproto::rpc::{BlockId, BlockRange, PoolType};
            let mut c = CompactTxStreamerClient::connect(indexer).await?;
            let req = BlockRange {
                start: Some(BlockId { height: u64::from(height), hash: vec![] }),
                end: Some(BlockId { height: u64::from(height), hash: vec![] }),
                pool_types: vec![PoolType::Transparent as i32, PoolType::Sapling as i32, PoolType::Orchard as i32, PoolType::Ironwood as i32],
            };
            let mut s = c.get_block_range(req).await?.into_inner();
            let served = s.message().await?.context("no block")?;
            let raw = reqwest::Client::builder().no_proxy().build()?
                .post(&node)
                .json(&serde_json::json!({"jsonrpc":"2.0","id":1,"method":"getblock","params":[height.to_string(), 0]}))
                .send().await?.json::<serde_json::Value>().await?;
            let bytes = hex::decode(raw["result"].as_str().context("no block hex")?)?;
            let (_, _, reference) = ssg_verifier::produce::produce(height, &bytes).map_err(|e| anyhow::anyhow!("{e}"))?;
            let show = |b: &lwproto::rpc::CompactBlock| serde_json::json!(b.vtx.iter().map(|t| serde_json::json!({
                "index": t.index,
                "txid": ssg_verifier::verify::display(&t.txid),
                "vin": t.vin.iter().map(|i| format!("{}:{}", ssg_verifier::verify::display(&i.prevout_txid), i.prevout_index)).collect::<Vec<_>>(),
                "vout": t.vout.iter().map(|o| format!("{} {}", o.value, hex::encode(&o.script_pub_key))).collect::<Vec<_>>(),
                "spends": t.spends.len(), "outputs": t.outputs.len(), "actions": t.actions.len(), "ironwood_actions": t.ironwood_actions.len(),
            })).collect::<Vec<_>>());
            println!("{}", serde_json::to_string_pretty(&serde_json::json!({"indexer": show(&served), "from_full_block": show(&reference)}))?);
            Ok(())
        }),
        Cmd::Tip { server } => {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
            let id = rt.block_on(probe::latest_block(&server))?;
            let mut hash = id.hash.clone();
            hash.reverse();
            println!("{} {}", id.height, hex::encode(hash));
            Ok(())
        }
        Cmd::Summarize { log } => {
            let records = summarize::load(&log)?;
            print!("{}", summarize::markdown(&records));
            Ok(())
        }
    }
}

//! Transparent CompactTxStreamer proxy.
//!
//! Step 1 of the plan: sit between the wallet and the indexer, change
//! nothing, and record exactly which calls a real sync makes. The proxy is
//! method-agnostic at the transport level, so calls we have no decoder for
//! still pass and still show up in the log by name and size.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc};

use anyhow::Context as _;
use bytes::Bytes;
use http::{HeaderValue, Request, Response, StatusCode, Uri};
use http_body_util::{combinators::BoxBody, BodyExt, Empty};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper_util::client::legacy::{connect::HttpConnector, Client};
use hyper_util::rt::{TokioExecutor, TokioIo};
use tokio::net::TcpListener;
use tracing::{debug, info, warn};

use crate::call::{Call, CallRecord, Side, Tap};

type Upstream = Client<HttpConnector, Tap<Incoming>>;
type OutBody = BoxBody<Bytes, hyper::Error>;

pub struct Config {
    pub listen: SocketAddr,
    pub upstream: Uri,
    pub record: Option<PathBuf>,
    /// See [`crate::tamper`]. Testing only.
    pub tamper_file: Option<PathBuf>,
}

struct State {
    client: Upstream,
    upstream: Uri,
    next_id: AtomicU64,
    sink: mpsc::Sender<CallRecord>,
    tamper_file: Option<PathBuf>,
}

pub async fn run(cfg: Config) -> anyhow::Result<()> {
    let listener = TcpListener::bind(cfg.listen)
        .await
        .with_context(|| format!("binding {}", cfg.listen))?;
    match &cfg.tamper_file {
        Some(f) => {
            warn!(listen = %cfg.listen, upstream = %cfg.upstream, rules = %f.display(), "proxy up; TAMPERING with compact blocks per rules file")
        }
        None => info!(listen = %cfg.listen, upstream = %cfg.upstream, "gateway up (pass-through)"),
    }
    serve_with(listener, cfg.upstream, cfg.record, cfg.tamper_file).await
}

/// Serves wallet connections on an already-bound listener until ctrl-c.
pub async fn serve(
    listener: TcpListener,
    upstream: Uri,
    record: Option<PathBuf>,
) -> anyhow::Result<()> {
    serve_with(listener, upstream, record, None).await
}

async fn serve_with(
    listener: TcpListener,
    upstream: Uri,
    record: Option<PathBuf>,
    tamper_file: Option<PathBuf>,
) -> anyhow::Result<()> {
    let sink = spawn_recorder(record)?;

    let mut connector = HttpConnector::new();
    connector.set_nodelay(true);
    let client = Client::builder(TokioExecutor::new())
        .http2_only(true)
        .build(connector);

    let state = Arc::new(State {
        client,
        upstream,
        next_id: AtomicU64::new(1),
        sink,
        tamper_file,
    });

    loop {
        let (stream, peer) = tokio::select! {
            r = listener.accept() => r?,
            _ = tokio::signal::ctrl_c() => {
                info!("shutting down");
                return Ok(());
            }
        };
        let _ = stream.set_nodelay(true);
        let state = state.clone();
        tokio::spawn(async move {
            let svc = service_fn(move |req| handle(state.clone(), req));
            // Wallets speak h2c with prior knowledge (tonic over http://).
            let conn = hyper::server::conn::http2::Builder::new(TokioExecutor::new())
                .max_frame_size(1 << 20)
                .serve_connection(TokioIo::new(stream), svc);
            if let Err(e) = conn.await {
                debug!(%peer, "connection closed: {e}");
            }
        });
    }
}

async fn handle(
    state: Arc<State>,
    req: Request<Incoming>,
) -> Result<Response<OutBody>, Infallible> {
    let path = req.uri().path().to_string();
    let method = path
        .strip_prefix(lwproto::SERVICE_PATH)
        .unwrap_or(&path)
        .to_string();
    let id = state.next_id.fetch_add(1, Ordering::Relaxed);
    let rules = match &state.tamper_file {
        Some(f) if method == "GetBlockRange" || method == "GetBlock" => crate::tamper::load(f),
        _ => vec![],
    };
    let call = Call::new(id, method, state.sink.clone());

    let (mut parts, body) = req.into_parts();
    parts.uri = match upstream_uri(&state.upstream, &parts.uri) {
        Ok(u) => u,
        Err(e) => return Ok(grpc_error(&call, 13, &format!("bad upstream uri: {e}"))),
    };
    if let Some(auth) = state.upstream.authority() {
        if let Ok(v) = HeaderValue::from_str(auth.as_str()) {
            parts.headers.insert(http::header::HOST, v);
        }
    }
    let upstream_req = Request::from_parts(parts, Tap::new(body, Side::Request, call.clone()));

    match state.client.request(upstream_req).await {
        Ok(resp) => {
            call.on_response_head(resp.status().as_u16(), resp.headers());
            let (parts, body) = resp.into_parts();
            if rules.is_empty() {
                let body = Tap::new(body, Side::Response, call).boxed();
                return Ok(Response::from_parts(parts, body));
            }
            let body = match rewrite(body, &rules).await {
                Ok(b) => b,
                Err(e) => {
                    return Ok(grpc_error(
                        &call,
                        13,
                        &format!("tamper rewrite failed: {e}"),
                    ))
                }
            };
            Ok(Response::from_parts(
                parts,
                Tap::new(body, Side::Response, call).boxed(),
            ))
        }
        Err(e) => {
            warn!(call = id, "upstream request failed: {e}");
            // 14 = UNAVAILABLE, which wallets treat as retryable.
            Ok(grpc_error(
                &call,
                14,
                &format!("gateway could not reach indexer: {e}"),
            ))
        }
    }
}

/// Buffers a compact-block response, applies tamper rules to each block and
/// re-frames it, keeping the upstream trailers.
async fn rewrite(body: Incoming, rules: &[crate::tamper::Rule]) -> anyhow::Result<OutBody> {
    use prost::Message;
    let collected = body.collect().await?;
    let trailers = collected.trailers().cloned();
    let data = collected.to_bytes();
    let mut d = crate::grpc::Deframer::default();
    d.push(&data);
    let mut out = bytes::BytesMut::new();
    while let Some(rec) = d.next_record().map_err(|e| anyhow::anyhow!("{e:?}"))? {
        let crate::grpc::Record::Message(m) = rec else {
            anyhow::bail!("compressed message")
        };
        let mut block = lwproto::rpc::CompactBlock::decode(m)?;
        let n = crate::tamper::apply(rules, &mut block);
        if n > 0 {
            warn!(
                height = block.height,
                fields = n,
                "tampered with compact block"
            );
        }
        out.extend_from_slice(&crate::grpc::encode(&block.encode_to_vec()));
    }
    let mut frames = vec![Ok::<_, hyper::Error>(hyper::body::Frame::data(
        out.freeze(),
    ))];
    if let Some(t) = trailers {
        frames.push(Ok(hyper::body::Frame::trailers(t)));
    }
    Ok(http_body_util::StreamBody::new(futures_util::stream::iter(frames)).boxed())
}

fn upstream_uri(base: &Uri, incoming: &Uri) -> Result<Uri, http::Error> {
    let pq = incoming.path_and_query().map(|p| p.as_str()).unwrap_or("/");
    Uri::builder()
        .scheme(base.scheme_str().unwrap_or("http"))
        .authority(base.authority().map(|a| a.as_str()).unwrap_or_default())
        .path_and_query(pq)
        .build()
}

/// A trailers-only gRPC error response.
fn grpc_error(call: &Arc<Call>, code: i32, msg: &str) -> Response<OutBody> {
    call.on_transport_error(msg.to_string());
    let mut resp = Response::new(
        Empty::<Bytes>::new()
            .map_err(|never| match never {})
            .boxed(),
    );
    *resp.status_mut() = StatusCode::OK;
    let h = resp.headers_mut();
    h.insert(
        http::header::CONTENT_TYPE,
        HeaderValue::from_static("application/grpc"),
    );
    h.insert("grpc-status", HeaderValue::from(code));
    if let Ok(v) = HeaderValue::from_str(&msg.replace(['\r', '\n'], " ")) {
        h.insert("grpc-message", v);
    }
    call.on_response_head(200, resp.headers());
    resp
}

fn spawn_recorder(path: Option<PathBuf>) -> anyhow::Result<mpsc::Sender<CallRecord>> {
    use std::io::Write;
    let mut out = match &path {
        Some(p) => {
            if let Some(dir) = p.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(p)
                .with_context(|| format!("opening {}", p.display()))?;
            Some(std::io::BufWriter::new(f))
        }
        None => None,
    };
    let (tx, rx) = mpsc::channel::<CallRecord>();
    std::thread::Builder::new()
        .name("recorder".into())
        .spawn(move || {
            for r in rx {
                info!(
                    "#{:<4} {:<22} status={:<4} req={}B resp={}B/{}msg {:.1}ms {}",
                    r.id,
                    r.method,
                    r.grpc_status
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| "-".into()),
                    r.request_bytes,
                    r.response_bytes,
                    r.response_messages,
                    r.duration_ms,
                    r.request,
                );
                if let Some(w) = out.as_mut() {
                    let line = serde_json::to_string(&r).expect("record serializes");
                    if writeln!(w, "{line}").and_then(|_| w.flush()).is_err() {
                        warn!("could not write call log");
                    }
                }
            }
        })?;
    Ok(tx)
}

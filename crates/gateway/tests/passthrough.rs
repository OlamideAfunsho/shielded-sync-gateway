//! The pass-through gateway must hand the wallet exactly what the indexer
//! sent: same bytes, same trailers, same status. These tests stand up a
//! fake indexer, put the gateway in front of it, and compare.

use std::convert::Infallible;
use std::time::Duration;

use bytes::Bytes;
use http::{HeaderMap, HeaderValue, Request, Response};
use http_body_util::{BodyExt, Full, StreamBody};
use hyper::body::Frame;
use hyper::service::service_fn;
use hyper_util::client::legacy::Client;
use hyper_util::rt::{TokioExecutor, TokioIo};
use lwproto::rpc;
use prost::Message;
use ssg_gateway::{call::CallRecord, grpc, proxy};
use tokio::net::TcpListener;

fn block(height: u64) -> rpc::CompactBlock {
    rpc::CompactBlock {
        height,
        hash: vec![height as u8; 32],
        prev_hash: vec![(height - 1) as u8; 32],
        time: 1_700_000_000,
        vtx: vec![rpc::CompactTx {
            index: 1,
            txid: vec![0xab; 32],
            outputs: vec![rpc::CompactSaplingOutput {
                cmu: vec![1; 32],
                ephemeral_key: vec![2; 32],
                ciphertext: vec![3; 52],
            }],
            ..Default::default()
        }],
        chain_metadata: Some(rpc::ChainMetadata {
            sapling_commitment_tree_size: height as u32,
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// The exact bytes the fake indexer streams for any GetBlockRange.
fn indexer_body() -> Vec<u8> {
    let mut out = Vec::new();
    for h in 100..103 {
        out.extend_from_slice(&grpc::encode(&block(h).encode_to_vec()));
    }
    out
}

/// A fake indexer: streams three blocks in awkward chunk sizes, then
/// trailers. `fail` makes it answer with a trailers-only error instead.
async fn fake_indexer(fail: bool) -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            tokio::spawn(async move {
                let svc = service_fn(move |_req: Request<hyper::body::Incoming>| async move {
                    let mut trailers = HeaderMap::new();
                    if fail {
                        let resp = Response::builder()
                            .header("content-type", "application/grpc")
                            .header("grpc-status", "5")
                            .header("grpc-message", "block not found")
                            .body(StreamBody::new(futures_stream(vec![])))
                            .unwrap();
                        return Ok::<_, Infallible>(resp);
                    }
                    trailers.insert("grpc-status", HeaderValue::from_static("0"));
                    let body = indexer_body();
                    // 7-byte chunks split every record header and payload.
                    let mut frames: Vec<Frame<Bytes>> = body
                        .chunks(7)
                        .map(|c| Frame::data(Bytes::copy_from_slice(c)))
                        .collect();
                    frames.push(Frame::trailers(trailers));
                    let resp = Response::builder()
                        .header("content-type", "application/grpc")
                        .body(StreamBody::new(futures_stream(frames)))
                        .unwrap();
                    Ok(resp)
                });
                let _ = hyper::server::conn::http2::Builder::new(TokioExecutor::new())
                    .serve_connection(TokioIo::new(stream), svc)
                    .await;
            });
        }
    });
    addr
}

type FrameStream =
    std::pin::Pin<Box<dyn futures_core::Stream<Item = Result<Frame<Bytes>, Infallible>> + Send>>;

fn futures_stream(frames: Vec<Frame<Bytes>>) -> FrameStream {
    Box::pin(tokio_stream::iter(frames.into_iter().map(Ok)))
}

async fn start_gateway(
    upstream: std::net::SocketAddr,
    log: &std::path::Path,
) -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let up: http::Uri = format!("http://{upstream}").parse().unwrap();
    let log = log.to_path_buf();
    tokio::spawn(async move { proxy::serve(listener, up, Some(log)).await.unwrap() });
    addr
}

async fn get_block_range(server: std::net::SocketAddr) -> (Vec<u8>, Option<HeaderMap>, HeaderMap) {
    let client = Client::builder(TokioExecutor::new())
        .http2_only(true)
        .build_http::<Full<Bytes>>();
    let req = rpc::BlockRange {
        start: Some(rpc::BlockId {
            height: 100,
            hash: vec![],
        }),
        end: Some(rpc::BlockId {
            height: 102,
            hash: vec![],
        }),
        pool_types: vec![],
    };
    let req = Request::post(format!(
        "http://{server}{}GetBlockRange",
        lwproto::SERVICE_PATH
    ))
    .header("content-type", "application/grpc")
    .header("te", "trailers")
    .body(Full::new(grpc::encode(&req.encode_to_vec())))
    .unwrap();
    let resp = client.request(req).await.unwrap();
    let headers = resp.headers().clone();
    let collected = resp.into_body().collect().await.unwrap();
    let trailers = collected.trailers().cloned();
    (collected.to_bytes().to_vec(), trailers, headers)
}

async fn read_log(path: &std::path::Path, want: usize) -> Vec<CallRecord> {
    // The record is written when hyper drops the call's bodies, which can
    // be a moment after the client has its response.
    for _ in 0..100 {
        if let Ok(text) = std::fs::read_to_string(path) {
            let recs: Vec<CallRecord> = text
                .lines()
                .map(|l| serde_json::from_str(l).unwrap())
                .collect();
            if recs.len() >= want {
                return recs;
            }
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("call log never reached {want} records");
}

#[tokio::test]
async fn stream_passes_through_byte_for_byte() {
    let dir = tempdir();
    let log = dir.join("calls.jsonl");
    let indexer = fake_indexer(false).await;
    let gateway = start_gateway(indexer, &log).await;

    let (direct, direct_trailers, _) = get_block_range(indexer).await;
    let (proxied, proxied_trailers, _) = get_block_range(gateway).await;

    assert_eq!(direct, indexer_body());
    assert_eq!(proxied, direct, "gateway altered the response body");
    assert_eq!(
        proxied_trailers.unwrap().get("grpc-status"),
        direct_trailers.unwrap().get("grpc-status")
    );

    let recs = read_log(&log, 1).await;
    let r = &recs[0];
    assert_eq!(r.method, "GetBlockRange");
    assert_eq!(r.grpc_status, Some(0));
    assert_eq!(r.response_messages, 3);
    assert_eq!(r.response_bytes as usize, indexer_body().len());
    assert_eq!(r.request["start"], 100);
    assert_eq!(r.response["blocks"]["first_height"], 100);
    assert_eq!(r.response["blocks"]["last_height"], 102);
    assert_eq!(
        r.response["blocks"]["fields_present"]["sapling_output.ciphertext"],
        3
    );
}

#[tokio::test]
async fn indexer_errors_reach_the_wallet_unchanged() {
    let dir = tempdir();
    let log = dir.join("calls.jsonl");
    let indexer = fake_indexer(true).await;
    let gateway = start_gateway(indexer, &log).await;

    let (body, _, headers) = get_block_range(gateway).await;
    assert!(body.is_empty());
    assert_eq!(headers.get("grpc-status").unwrap(), "5");
    assert_eq!(headers.get("grpc-message").unwrap(), "block not found");

    let recs = read_log(&log, 1).await;
    assert_eq!(recs[0].grpc_status, Some(5));
    assert_eq!(recs[0].grpc_message.as_deref(), Some("block not found"));
}

#[tokio::test]
async fn unreachable_indexer_is_reported_as_unavailable() {
    let dir = tempdir();
    let log = dir.join("calls.jsonl");
    // Bind and drop to get a port nothing listens on.
    let dead = TcpListener::bind("127.0.0.1:0")
        .await
        .unwrap()
        .local_addr()
        .unwrap();
    let gateway = start_gateway(dead, &log).await;

    let (_, _, headers) = get_block_range(gateway).await;
    assert_eq!(headers.get("grpc-status").unwrap(), "14");

    let recs = read_log(&log, 1).await;
    assert_eq!(recs[0].grpc_status, Some(14));
    assert!(recs[0].transport_error.is_some());
}

fn tempdir() -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!(
        "ssg-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

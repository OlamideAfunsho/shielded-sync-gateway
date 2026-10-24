//! Tiny unary gRPC client used by the scripts to ask an indexer (or the
//! gateway) for its tip, without pulling in grpcurl.

use bytes::Bytes;
use http::{Request, Uri};
use http_body_util::{BodyExt, Full};
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;
use prost::Message;

use crate::grpc::{Deframer, Record};

pub async fn latest_block(server: &Uri) -> anyhow::Result<lwproto::rpc::BlockId> {
    let client = Client::builder(TokioExecutor::new())
        .http2_only(true)
        .build_http::<Full<Bytes>>();
    let uri: Uri = format!(
        "{}://{}{}GetLatestBlock",
        server.scheme_str().unwrap_or("http"),
        server
            .authority()
            .map(|a| a.as_str())
            .unwrap_or("127.0.0.1:8137"),
        lwproto::SERVICE_PATH
    )
    .parse()?;
    let body = crate::grpc::encode(&lwproto::rpc::ChainSpec {}.encode_to_vec());
    let req = Request::post(uri)
        .header("content-type", "application/grpc")
        .header("te", "trailers")
        .body(Full::new(body))?;
    let resp = client.request(req).await?;
    let status = resp.headers().get("grpc-status").cloned();
    let collected = resp.into_body().collect().await?;
    let trailer_status = collected
        .trailers()
        .and_then(|t| t.get("grpc-status").cloned())
        .or(status);
    if let Some(s) = trailer_status {
        if s != "0" {
            anyhow::bail!("grpc-status {}", s.to_str().unwrap_or("?"));
        }
    }
    let mut d = Deframer::default();
    d.push(&collected.to_bytes());
    match d.next_record() {
        Ok(Some(Record::Message(m))) => Ok(lwproto::rpc::BlockId::decode(m)?),
        other => anyhow::bail!("unexpected response: {other:?}"),
    }
}

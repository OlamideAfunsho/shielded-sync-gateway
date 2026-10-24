//! Per-call bookkeeping for proxied gRPC requests.
//!
//! Both bodies of a call (request and response) are wrapped in [`Tap`],
//! which sees every DATA and trailer frame on its way through. The call is
//! written to the log when the last reference to its [`Call`] is dropped,
//! i.e. after hyper has finished with both bodies. Relying on Drop rather
//! than end-of-stream matters: hyper may stop polling a body once
//! `is_end_stream` reports true, so a final `None` is not guaranteed.

use std::pin::Pin;
use std::sync::{mpsc, Arc, Mutex};
use std::task::{ready, Context, Poll};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use http::HeaderMap;
use http_body::{Body, Frame, SizeHint};
use serde::Serialize;
use serde_json::Value;

use crate::describe::{self, Responses};
use crate::grpc::{Deframer, Record};

#[derive(Clone, Copy, Debug)]
pub enum Side {
    Request,
    Response,
}

#[derive(Default)]
struct Direction {
    wire_bytes: u64,
    messages: u64,
    deframer: Deframer,
    framing_error: Option<String>,
}

struct Inner {
    request_summary: Option<Value>,
    responses: Responses,
    req: Direction,
    resp: Direction,
    http_status: Option<u16>,
    grpc_status: Option<i32>,
    grpc_message: Option<String>,
    transport_error: Option<String>,
    first_response_at: Option<Instant>,
}

pub struct Call {
    pub id: u64,
    pub method: String,
    started: Instant,
    started_unix_ms: u128,
    inner: Mutex<Inner>,
    sink: mpsc::Sender<CallRecord>,
}

/// One line of the JSONL call log.
#[derive(Serialize, Debug, Clone, serde::Deserialize)]
pub struct CallRecord {
    pub id: u64,
    pub method: String,
    pub started_unix_ms: u128,
    pub duration_ms: f64,
    pub time_to_first_response_ms: Option<f64>,
    pub http_status: Option<u16>,
    pub grpc_status: Option<i32>,
    pub grpc_message: Option<String>,
    pub transport_error: Option<String>,
    pub request_messages: u64,
    pub request_bytes: u64,
    pub response_messages: u64,
    pub response_bytes: u64,
    pub request: Value,
    pub response: Value,
}

impl Call {
    pub fn new(id: u64, method: String, sink: mpsc::Sender<CallRecord>) -> Arc<Self> {
        Arc::new(Call {
            id,
            method,
            started: Instant::now(),
            started_unix_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or_default(),
            inner: Mutex::new(Inner {
                request_summary: None,
                responses: Responses::default(),
                req: Direction::default(),
                resp: Direction::default(),
                http_status: None,
                grpc_status: None,
                grpc_message: None,
                transport_error: None,
                first_response_at: None,
            }),
            sink,
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        // A panic while holding the lock would only ever come from our own
        // bookkeeping; keep forwarding traffic regardless.
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn on_response_head(&self, status: u16, headers: &HeaderMap) {
        let mut g = self.lock();
        g.http_status = Some(status);
        read_grpc_status(&mut g, headers);
    }

    pub fn on_transport_error(&self, err: String) {
        self.lock().transport_error = Some(err);
    }

    fn on_data(&self, side: Side, data: &[u8]) {
        let mut g = self.lock();
        let g = &mut *g;
        if matches!(side, Side::Response) && g.first_response_at.is_none() {
            g.first_response_at = Some(Instant::now());
        }
        let dir = match side {
            Side::Request => &mut g.req,
            Side::Response => &mut g.resp,
        };
        dir.wire_bytes += data.len() as u64;
        if dir.framing_error.is_some() {
            return;
        }
        dir.deframer.push(data);
        loop {
            match dir.deframer.next_record() {
                Ok(None) => break,
                Ok(Some(Record::Message(payload))) => {
                    dir.messages += 1;
                    match side {
                        Side::Request => {
                            // Server-streaming only in this protocol except
                            // GetTaddressBalanceStream; keep the first.
                            if g.request_summary.is_none() {
                                g.request_summary = Some(describe::request(&self.method, &payload));
                            }
                        }
                        Side::Response => g.responses.observe(&self.method, &payload),
                    }
                }
                Ok(Some(Record::Compressed(n))) => {
                    dir.messages += 1;
                    dir.framing_error =
                        Some(format!("compressed message of {n} bytes not inspected"));
                }
                Err(e) => {
                    dir.framing_error = Some(format!("{e:?}"));
                    break;
                }
            }
        }
    }

    fn on_trailers(&self, headers: &HeaderMap) {
        read_grpc_status(&mut self.lock(), headers);
    }

    fn on_body_error(&self, side: Side, err: String) {
        let mut g = self.lock();
        g.transport_error = Some(format!("{side:?} body: {err}"));
    }
}

fn read_grpc_status(g: &mut Inner, headers: &HeaderMap) {
    if let Some(s) = headers.get("grpc-status").and_then(|v| v.to_str().ok()) {
        g.grpc_status = s.parse().ok();
    }
    if let Some(m) = headers.get("grpc-message").and_then(|v| v.to_str().ok()) {
        if !m.is_empty() {
            g.grpc_message = Some(m.to_string());
        }
    }
}

impl Drop for Call {
    fn drop(&mut self) {
        let g = self.inner.get_mut().unwrap_or_else(|p| p.into_inner());
        let mut response = g.responses.to_json();
        if let Some(e) = &g.resp.framing_error {
            response["framing_error"] = Value::String(e.clone());
        }
        if g.resp.deframer.pending() > 0 {
            response["truncated_bytes"] = g.resp.deframer.pending().into();
        }
        let record = CallRecord {
            id: self.id,
            method: self.method.clone(),
            started_unix_ms: self.started_unix_ms,
            duration_ms: self.started.elapsed().as_secs_f64() * 1e3,
            time_to_first_response_ms: g
                .first_response_at
                .map(|t| (t - self.started).as_secs_f64() * 1e3),
            http_status: g.http_status,
            grpc_status: g.grpc_status,
            grpc_message: g.grpc_message.take(),
            transport_error: g.transport_error.take(),
            request_messages: g.req.messages,
            request_bytes: g.req.wire_bytes,
            response_messages: g.resp.messages,
            response_bytes: g.resp.wire_bytes,
            request: g.request_summary.take().unwrap_or(Value::Null),
            response,
        };
        // The recorder thread only goes away at shutdown.
        let _ = self.sink.send(record);
    }
}

pin_project_lite::pin_project! {
    /// A body that forwards frames untouched while reporting them to a
    /// [`Call`].
    pub struct Tap<B> {
        #[pin]
        inner: B,
        side: Side,
        call: Arc<Call>,
    }
}

impl<B> Tap<B> {
    pub fn new(inner: B, side: Side, call: Arc<Call>) -> Self {
        Tap { inner, side, call }
    }
}

impl<B> Body for Tap<B>
where
    B: Body<Data = Bytes>,
    B::Error: std::fmt::Display,
{
    type Data = Bytes;
    type Error = B::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
        let this = self.project();
        let frame = ready!(this.inner.poll_frame(cx));
        match &frame {
            Some(Ok(f)) => {
                if let Some(d) = f.data_ref() {
                    this.call.on_data(*this.side, d);
                } else if let Some(t) = f.trailers_ref() {
                    this.call.on_trailers(t);
                }
            }
            Some(Err(e)) => this.call.on_body_error(*this.side, e.to_string()),
            None => {}
        }
        Poll::Ready(frame)
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

//! Server-sent event streams (RFC-MACP-0006 §4.3): the five `Watch*` RPCs
//! and the passive-subscribe form of `StreamSession`.
//!
//! **Before the stream** (any failure is a request-level error and no stream
//! is opened; `dispatch` runs the first three):
//!
//! 1. `Accept`, if present, must admit `text/event-stream`
//!    ([`accepts_event_stream`]), else the transport `406`.
//! 2. Authentication from the headers (rows whose gRPC handler
//!    authenticates), then the body under the cap and the canonical decode.
//! 3. `StreamSession`: no `envelope`, a non-empty `subscribe_session_id`, and
//!    a `Last-Event-ID` that is a non-negative decimal agreeing with a
//!    non-zero `after_sequence` ([`after_sequence`]), else `400
//!    INVALID_REQUEST`; then `MacpServer::open_subscription` (`404
//!    SESSION_NOT_FOUND`, `403 FORBIDDEN`, `409 HISTORY_UNAVAILABLE` with its
//!    details). A `Watch*` RPC's handler status is request-level.
//!
//! **Framing.** One event per message: `data:` is the message's canonical
//! JSON on one line, with an `id:` (the Envelope's ordinal, RFC-MACP-0006
//! §3.2) on subscribe envelope events only. `: keepalive` after
//! [`KEEPALIVE_INTERVAL`] without an event. A fatal error is one
//! `event: error` whose data is the §4.5 body, then the end of the response.
//! Headers `Content-Type: text/event-stream` and `Cache-Control: no-store`;
//! no `retry:` field is ever sent.
//!
//! **How a subscribe stream ends.**
//!
//! - **Normally after the terminal Envelope.** When the session is resolved,
//!   its last ordinal is fixed (a terminal session accepts nothing more and
//!   the log append precedes the state change, `Runtime::process_message`),
//!   so the stream ends normally once it has delivered that ordinal, the
//!   Envelope that resolved the session. The `Resolved` lifecycle event is
//!   published before the Envelope reaches the bus, and the two channels are
//!   independent, so the event only fixes the target: it never closes the
//!   stream itself. A subscribe to a session already resolved replays through
//!   that Envelope and ends the same way.
//! - **Fatally on cancel or expiry.** These write runtime bookkeeping entries
//!   that consume no ordinal and are never delivered (RFC-MACP-0006 §3.2), so
//!   no terminal Envelope exists; a plain close would read as a lost
//!   connection and drive reconnect loops. The stream drains the envelopes
//!   already on its bus receiver (published under the session lock before
//!   the terminal entry), then sends `event: error` `SESSION_NOT_OPEN` naming
//!   the cause. A subscribe to a session already cancelled or expired
//!   replays and ends the same way.
//! - **Fatally on lag** (`STREAM_LAGGED`): the client stopped reading, hyper
//!   stopped polling, and the bus dropped envelopes for this receiver.
//! - **As a lost connection on shutdown**: when the listener starts draining,
//!   every stream ends with no `event: error`, so the client resumes with
//!   `Last-Event-ID` (elsewhere, or after the restart) instead of the drain
//!   waiting for streams that have no natural end until the deadline.
//!
//! Lifecycle events are a global bus shared by every session; a subscriber
//! that lags on it re-reads its session's state instead.
//!
//! **Resume numbering.** The cursor numbers the first live envelope
//! `high_water + 1`, exact whenever `after_sequence` is at most the high
//! water at the bus subscribe (see `SubscriptionCursor`). A client's
//! `after_sequence` above that (one it cannot have learned from this
//! runtime) would leave an envelope accepted in the subscribe window
//! unreplayed and mislabelled, so the subscription is opened at
//! `min(after_sequence, high water read before the bus subscribe)` and every
//! item at or below the client's `after_sequence` is dropped: ids stay exact
//! and nothing the client holds is delivered again.
//!
//! **Lifetime.** A stream is not bounded by the request timeout (only the
//! pre-stream checks are), and ends when the client goes away: hyper drops
//! the body, which drops the bus and lifecycle receivers (the session's bus
//! channel is then reclaimable). Keepalive writes surface a dead peer.
//! Observability seam (P9): the HTTP SERVER span must be moved into the body
//! stream (`tracing::Instrument`) so it stays open for the stream's life, as
//! the gRPC span does; nothing here creates a span yet.

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures_core::Stream;
use http::header::{HeaderMap, HeaderValue, CACHE_CONTROL, CONTENT_TYPE};
use http::{Response, StatusCode};
use http_body_util::{BodyExt, StreamBody};
use hyper::body::Frame;
use macp_pb::pb::{Envelope, StreamSessionRequest, StreamSessionResponse};
use prost_reflect::{DynamicMessage, MethodDescriptor};
use tokio::sync::{broadcast, watch};
use tokio::time::Instant;
use tokio_stream::StreamExt;
use tonic::metadata::MetadataMap;
use tonic::{Extensions, Status};

use super::dispatch::to_dynamic;
use super::errors::{empty, status_response, RequestError};
use super::json;
use super::server::{split_params, Body};
use crate::pb::macp_runtime_service_server::MacpRuntimeService;
use crate::runtime::{Runtime, SessionLifecycleEvent};
use crate::security::AuthIdentity;
use crate::server::{MacpServer, StreamLagged, SubscribeError, SubscriptionCursor};
use crate::session::SessionState;

/// RFC-MACP-0006 §4.3: a comment after this long without an event.
pub(crate) const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(30);

const KEEPALIVE: &[u8] = b": keepalive\n\n";
const EVENT_STREAM: HeaderValue = HeaderValue::from_static("text/event-stream");
const NO_STORE: HeaderValue = HeaderValue::from_static("no-store");

/// What a stream needs from its listener.
#[derive(Debug, Clone)]
pub(crate) struct Context {
    keepalive: Duration,
    shutdown: watch::Receiver<bool>,
}

impl Context {
    pub(crate) fn new(keepalive: Duration, shutdown: watch::Receiver<bool>) -> Self {
        Self {
            keepalive,
            shutdown,
        }
    }
}

/// Resolves once the listener starts draining (at once if it already has).
/// Cancel-safe.
async fn shutdown_requested(rx: &mut watch::Receiver<bool>) {
    if rx.wait_for(|draining| *draining).await.is_err() {
        // The listener state outlives every connection, so the sender is
        // never dropped under a live stream; never end a stream on it.
        std::future::pending::<()>().await;
    }
}

// ───────────────────────── pre-stream checks ─────────────────────────

/// RFC-MACP-0006 §4.3: a request's `Accept`, if present, must admit
/// `text/event-stream`. Every `Accept` header's media ranges are read
/// (RFC 9110 §12.5.1); the most specific range matching `text/event-stream`
/// (the type itself, then `text/*`, then `*/*`) decides, and admits it unless
/// its `q` is `0`. No `Accept` header admits anything; a present header with
/// no matching range (including an empty one) does not. A range with a
/// malformed `q` is ignored.
pub(crate) fn accepts_event_stream<'a>(values: impl Iterator<Item = &'a HeaderValue>) -> bool {
    let mut present = false;
    // (specificity, admitted) of the most specific matching range so far.
    let mut best: Option<(u8, bool)> = None;
    for value in values {
        present = true;
        let Ok(value) = value.to_str() else {
            continue;
        };
        for range in split_list(value) {
            let Some(parts) = split_params(range) else {
                continue;
            };
            let mut parts = parts.into_iter();
            let media = parts.next().unwrap_or_default().trim();
            let specificity = if media.eq_ignore_ascii_case("text/event-stream") {
                3
            } else if media.eq_ignore_ascii_case("text/*") {
                2
            } else if media == "*/*" {
                1
            } else {
                continue;
            };
            let mut q = Some(1.0f32);
            for param in parts {
                if let Some((name, value)) = param.split_once('=') {
                    if name.trim().eq_ignore_ascii_case("q") {
                        q = parse_qvalue(value.trim());
                        break;
                    }
                }
            }
            let Some(q) = q else {
                continue;
            };
            if best.is_none_or(|(s, _)| specificity > s) {
                best = Some((specificity, q > 0.0));
            }
        }
    }
    !present || best.is_some_and(|(_, admitted)| admitted)
}

/// Split a header list on `,` outside quoted strings.
fn split_list(value: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let (mut start, mut quoted, mut escaped) = (0, false, false);
    for (i, c) in value.char_indices() {
        match c {
            _ if escaped => escaped = false,
            '\\' if quoted => escaped = true,
            '"' => quoted = !quoted,
            ',' if !quoted => {
                parts.push(&value[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&value[start..]);
    parts
}

/// RFC 9110 §12.4.2 `qvalue`: `0`..`1` with at most three decimals.
fn parse_qvalue(raw: &str) -> Option<f32> {
    let (int, frac) = raw.split_once('.').unwrap_or((raw, ""));
    let valid = matches!(int, "0" | "1")
        && frac.len() <= 3
        && frac.bytes().all(|b| b.is_ascii_digit())
        && (int == "0" || frac.bytes().all(|b| b == b'0'));
    valid.then(|| raw.parse().ok()).flatten()
}

/// The subscribe position: the `Last-Event-ID` header when present (it must
/// be one non-negative decimal integer and agree with a non-zero
/// `after_sequence`), else the body's `after_sequence` (§4.3). `Err` is the
/// `400 INVALID_REQUEST` message.
pub(crate) fn after_sequence(headers: &HeaderMap, body_after: u64) -> Result<u64, String> {
    let mut values = headers.get_all("last-event-id").iter();
    let Some(value) = values.next() else {
        return Ok(body_after);
    };
    if values.next().is_some() {
        return Err("more than one Last-Event-ID header".into());
    }
    let shown = String::from_utf8_lossy(value.as_bytes()).into_owned();
    let text = value.as_bytes();
    if text.is_empty() || !text.iter().all(u8::is_ascii_digit) {
        return Err(format!(
            "Last-Event-ID {shown:?} is not a non-negative decimal integer"
        ));
    }
    let id: u64 = shown
        .parse()
        .map_err(|_| format!("Last-Event-ID {shown} is out of range"))?;
    if body_after != 0 && body_after != id {
        return Err(format!(
            "Last-Event-ID {id} conflicts with after_sequence {body_after}"
        ));
    }
    Ok(id)
}

// ───────────────────────── serving ─────────────────────────

/// Serve a streaming RPC whose `Accept`, credential and body cap `dispatch`
/// has checked; `body` is the request body.
pub(crate) async fn serve(
    server: &MacpServer,
    method: &MethodDescriptor,
    headers: &HeaderMap,
    metadata: MetadataMap,
    identity: Option<AuthIdentity>,
    body: &[u8],
    ctx: Context,
) -> Response<Body> {
    let request = match json::decode_request(method.name(), body) {
        Ok(request) => request,
        Err(e) => return RequestError::new("INVALID_REQUEST", e.reason()).into_response(),
    };
    if method.name() == "StreamSession" {
        let Some(identity) = identity else {
            // `StreamSession` is an authenticated row (pinned by a test).
            return RequestError::new("UNAUTHENTICATED", "missing bearer token").into_response();
        };
        return subscribe(server, method, headers, &identity, request, ctx).await;
    }
    watch(server, method, metadata, request, ctx).await
}

/// A `200` event stream.
fn sse_response(events: impl Stream<Item = Bytes> + Send + 'static) -> Response<Body> {
    let body = StreamBody::new(events.map(|b| Ok::<_, Infallible>(Frame::data(b))));
    let mut resp = Response::new(body.boxed_unsync());
    let headers = resp.headers_mut();
    headers.insert(CONTENT_TYPE, EVENT_STREAM);
    headers.insert(CACHE_CONTROL, NO_STORE);
    resp
}

/// One `data:` event (with `id:` when given). The canonical encoder writes
/// compact JSON, so the data is one line.
fn data_event(json: &[u8], id: Option<u64>) -> Bytes {
    let mut out = Vec::with_capacity(json.len() + 32);
    if let Some(id) = id {
        out.extend_from_slice(format!("id: {id}\n").as_bytes());
    }
    out.extend_from_slice(b"data: ");
    out.extend_from_slice(json);
    out.extend_from_slice(b"\n\n");
    Bytes::from(out)
}

/// The fatal `event: error` (§4.3): data is the §4.5 body.
fn error_event(err: &RequestError) -> Bytes {
    let body = err.body();
    let mut out = Vec::with_capacity(body.len() + 24);
    out.extend_from_slice(b"event: error\ndata: ");
    out.extend_from_slice(&body);
    out.extend_from_slice(b"\n\n");
    Bytes::from(out)
}

// ───────────────────────── watch streams ─────────────────────────

/// A `Watch*` RPC: the same trait method gRPC serves, its stream as events.
async fn watch(
    server: &MacpServer,
    method: &MethodDescriptor,
    metadata: MetadataMap,
    request: DynamicMessage,
    ctx: Context,
) -> Response<Body> {
    macro_rules! call {
        ($call:ident, $req:ty) => {{
            let request: $req = match request.transcode_to() {
                Ok(r) => r,
                Err(e) => {
                    return RequestError::new(
                        "INVALID_REQUEST",
                        format!("request does not transcode: {e}"),
                    )
                    .into_response()
                }
            };
            let request = tonic::Request::from_parts(metadata, Extensions::new(), request);
            match MacpRuntimeService::$call(server, request).await {
                Ok(resp) => sse_response(watch_events(method.clone(), resp.into_inner(), ctx)),
                Err(status) => status_response(&status, ""),
            }
        }};
    }
    match method.name() {
        "WatchModeRegistry" => call!(watch_mode_registry, crate::pb::WatchModeRegistryRequest),
        "WatchRoots" => call!(watch_roots, crate::pb::WatchRootsRequest),
        "WatchSignals" => call!(watch_signals, crate::pb::WatchSignalsRequest),
        "WatchSessions" => call!(watch_sessions, crate::pb::WatchSessionsRequest),
        "WatchPolicies" => call!(watch_policies, crate::pb::WatchPoliciesRequest),
        // The table is pinned to the descriptor: no other streaming row.
        _ => empty(StatusCode::NOT_FOUND),
    }
}

enum WatchStep<T> {
    Shutdown,
    Item(Option<Result<T, Status>>),
    Idle,
}

/// A watch stream's messages as events: no `id:` (watch streams are not
/// resumable, §4.3); a handler status mid-stream is the fatal error event.
/// `WatchSignalsResponse.sender_authenticated` is a sibling of the Envelope
/// and always emitted (the encoder writes no-presence defaults, §10.10).
fn watch_events<S, T>(
    method: MethodDescriptor,
    mut stream: S,
    mut ctx: Context,
) -> impl Stream<Item = Bytes> + Send + 'static
where
    S: Stream<Item = Result<T, Status>> + Send + Unpin + 'static,
    T: prost::Message + Send + 'static,
{
    async_stream::stream! {
        let mut idle = Box::pin(tokio::time::sleep(ctx.keepalive));
        loop {
            let step = tokio::select! {
                biased;
                _ = shutdown_requested(&mut ctx.shutdown) => WatchStep::Shutdown,
                item = stream.next() => WatchStep::Item(item),
                _ = &mut idle => WatchStep::Idle,
            };
            match step {
                WatchStep::Shutdown | WatchStep::Item(None) => break,
                WatchStep::Item(Some(Ok(message))) => match to_dynamic(&method, &message) {
                    Ok(dynamic) => yield data_event(&json::encode_response(&dynamic), None),
                    Err(status) => {
                        yield error_event(&RequestError::from_coded_status(&status, ""));
                        break;
                    }
                },
                WatchStep::Item(Some(Err(status))) => {
                    yield error_event(&RequestError::from_coded_status(&status, ""));
                    break;
                }
                WatchStep::Idle => yield Bytes::from_static(KEEPALIVE),
            }
            idle.as_mut().reset(Instant::now() + ctx.keepalive);
        }
    }
}

// ───────────────────────── subscribe stream ─────────────────────────

/// `StreamSession` passive subscribe: the body checks, then the stream.
async fn subscribe(
    server: &MacpServer,
    method: &MethodDescriptor,
    headers: &HeaderMap,
    identity: &AuthIdentity,
    request: DynamicMessage,
    ctx: Context,
) -> Response<Body> {
    let req: StreamSessionRequest = match request.transcode_to() {
        Ok(r) => r,
        Err(e) => {
            return RequestError::new(
                "INVALID_REQUEST",
                format!("request does not transcode: {e}"),
            )
            .into_response()
        }
    };
    let session_id = req.subscribe_session_id;
    let invalid = |message: String| {
        RequestError::new("INVALID_REQUEST", message)
            .with_session(&session_id)
            .into_response()
    };
    if req.envelope.is_some() {
        return invalid(
            "a StreamSession request must not carry an envelope: this binding has no \
             bidirectional stream; submit Envelopes with Send"
                .into(),
        );
    }
    if session_id.is_empty() {
        return invalid("subscribe_session_id must not be empty".into());
    }
    let after = match after_sequence(headers, req.after_sequence) {
        Ok(after) => after,
        Err(message) => return invalid(message),
    };

    let runtime = Arc::clone(server.runtime());
    // Before the cursor, so no terminal transition can fall between the
    // state the stream first reads and the events it then receives.
    let lifecycle = runtime.subscribe_session_lifecycle();
    let cursor = match open(server, identity, &session_id, after).await {
        Ok(cursor) => cursor,
        Err(e) => return status_response(&e.status(), &session_id),
    };
    let subscription = Subscription {
        runtime,
        method: method.clone(),
        session_id,
        cursor,
        lifecycle,
    };
    sse_response(subscribe_events(subscription, after, ctx, Hooks::default()))
}

/// Open the cursor at `min(after, high water read before the bus subscribe)`
/// (see the module docs, "Resume numbering"). If that floor fell below a
/// compaction made in between (the session ended meanwhile, so nothing more
/// is published), the client's own position is tried.
async fn open(
    server: &MacpServer,
    identity: &AuthIdentity,
    session_id: &str,
    after: u64,
) -> Result<SubscriptionCursor, SubscribeError> {
    let floor = match server
        .runtime()
        .subscribe_snapshot(session_id, u64::MAX)
        .await
    {
        Ok((_, high_water)) => after.min(high_water),
        Err(_) => after,
    };
    match server.open_subscription(identity, session_id, floor).await {
        Err(SubscribeError::HistoryUnavailable {
            lowest_available_sequence,
        }) if floor < after && after >= lowest_available_sequence => {
            server.open_subscription(identity, session_id, after).await
        }
        other => other,
    }
}

/// One subscriber's open stream.
struct Subscription {
    runtime: Arc<Runtime>,
    /// `StreamSession`, whose output type frames each Envelope.
    method: MethodDescriptor,
    session_id: String,
    cursor: SubscriptionCursor,
    lifecycle: broadcast::Receiver<SessionLifecycleEvent>,
}

/// How far the session is from ending, as last observed.
#[derive(Debug, Clone, PartialEq)]
enum Ending {
    /// Not terminal (open or suspended).
    Open,
    /// Resolved: the stream ends normally once it delivered `last_ordinal`.
    Resolved { last_ordinal: u64 },
    /// Ended without a terminal Envelope: drain, then this error event.
    Fatal(RequestError),
}

impl Ending {
    /// A terminal observation is final; only `Open` is replaced.
    fn merge(self, newer: Ending) -> Ending {
        match self {
            Ending::Open => newer,
            terminal => terminal,
        }
    }
}

impl Subscription {
    /// The session's state now.
    async fn observe(&self) -> Ending {
        let session = match self.runtime.session_for_read(&self.session_id).await {
            Ok(session) => session,
            Err(_) => {
                return Ending::Fatal(
                    RequestError::new("SESSION_NOT_FOUND", "the session no longer exists")
                        .with_session(&self.session_id),
                )
            }
        };
        match session.state {
            SessionState::Resolved => Ending::Resolved {
                // Never below the compaction base, so `u64::MAX` cannot fail.
                last_ordinal: self
                    .runtime
                    .subscribe_snapshot(&self.session_id, u64::MAX)
                    .await
                    .map(|(_, high_water)| high_water)
                    .unwrap_or(0),
            },
            SessionState::Cancelled => Ending::Fatal(self.not_open(
                "session was cancelled; it delivers no further envelopes and has no terminal \
                 envelope",
            )),
            SessionState::Expired => Ending::Fatal(self.not_open(
                "session expired (TTL); it delivers no further envelopes and has no terminal \
                 envelope",
            )),
            SessionState::Open | SessionState::Suspended => Ending::Open,
        }
    }

    fn not_open(&self, message: &str) -> RequestError {
        RequestError::new("SESSION_NOT_OPEN", message).with_session(&self.session_id)
    }

    fn lagged(&self, lagged: StreamLagged) -> RequestError {
        RequestError::new(
            "STREAM_LAGGED",
            format!(
                "subscriber fell behind by {} envelopes; events were dropped",
                lagged.skipped
            ),
        )
        .with_session(&self.session_id)
    }

    /// Whether a lifecycle event may change how this stream ends.
    fn concerns(&self, event: &SessionLifecycleEvent) -> bool {
        match event {
            SessionLifecycleEvent::Resolved { session_id }
            | SessionLifecycleEvent::Cancelled { session_id }
            | SessionLifecycleEvent::Expired { session_id } => *session_id == self.session_id,
            _ => false,
        }
    }

    /// An Envelope as its event: `id:` = ordinal, data = the
    /// `StreamSessionResponse` JSON.
    fn envelope_event(&self, ordinal: u64, envelope: Envelope) -> Result<Bytes, RequestError> {
        let response = StreamSessionResponse {
            response: Some(crate::pb::stream_session_response::Response::Envelope(
                envelope,
            )),
        };
        to_dynamic(&self.method, &response)
            .map(|message| data_event(&json::encode_response(&message), Some(ordinal)))
            .map_err(|status| RequestError::from_coded_status(&status, &self.session_id))
    }
}

/// Test seams; nothing in production builds.
#[derive(Debug, Clone, Default)]
struct Hooks {
    /// Read nothing from the bus (replay included) until a `Resolved`
    /// lifecycle event has been observed: the lifecycle event then always
    /// wins the race with the Envelope that caused it, the worst case of the
    /// runtime's order (it signals `Resolved` before it publishes).
    #[cfg(test)]
    hold_bus_until_resolved: bool,
    /// Notified each time the stream is about to wait while the session is
    /// still open, so a test can act only once the stream is parked having
    /// observed `Open`.
    #[cfg(test)]
    parked: Option<Arc<tokio::sync::Notify>>,
}

impl Hooks {
    fn bus_open(&self, ending: &Ending) -> bool {
        #[cfg(test)]
        if self.hold_bus_until_resolved {
            return matches!(ending, Ending::Resolved { .. });
        }
        let _ = ending;
        true
    }

    fn parking(&self, ending: &Ending) {
        #[cfg(test)]
        if let (Some(parked), Ending::Open) = (&self.parked, ending) {
            parked.notify_one();
        }
        let _ = ending;
    }
}

enum SubscribeStep {
    Shutdown,
    Lifecycle(Result<SessionLifecycleEvent, broadcast::error::RecvError>),
    Item(Result<Option<(u64, Envelope)>, StreamLagged>),
    Idle,
}

/// The subscribe stream's events. `after` is the client's position: no item
/// at or below it is delivered.
fn subscribe_events(
    mut sub: Subscription,
    after: u64,
    mut ctx: Context,
    hooks: Hooks,
) -> impl Stream<Item = Bytes> + Send + 'static {
    async_stream::stream! {
        let mut ending = sub.observe().await;
        // The highest ordinal the client holds.
        let mut delivered = after;
        let mut lifecycle_open = true;
        let mut idle = Box::pin(tokio::time::sleep(ctx.keepalive));
        'stream: loop {
            match &ending {
                Ending::Fatal(err) => {
                    // Everything accepted before the terminal entry is
                    // already on this receiver; deliver it, then the error.
                    loop {
                        match sub.cursor.try_next() {
                            Ok(Some((ordinal, envelope))) if ordinal > delivered => {
                                match sub.envelope_event(ordinal, envelope) {
                                    Ok(event) => yield event,
                                    Err(e) => {
                                        yield error_event(&e);
                                        break 'stream;
                                    }
                                }
                                delivered = ordinal;
                            }
                            Ok(Some(_)) => {}
                            Ok(None) => break,
                            Err(lagged) => {
                                yield error_event(&sub.lagged(lagged));
                                break 'stream;
                            }
                        }
                    }
                    yield error_event(err);
                    break;
                }
                Ending::Resolved { last_ordinal } if delivered >= *last_ordinal => break,
                _ => {}
            }
            let bus_open = hooks.bus_open(&ending);
            hooks.parking(&ending);
            let step = tokio::select! {
                biased;
                _ = shutdown_requested(&mut ctx.shutdown) => SubscribeStep::Shutdown,
                event = sub.lifecycle.recv(), if lifecycle_open => SubscribeStep::Lifecycle(event),
                item = sub.cursor.next(), if bus_open => SubscribeStep::Item(item),
                _ = &mut idle => SubscribeStep::Idle,
            };
            match step {
                // A lost connection to the client: it resumes elsewhere.
                SubscribeStep::Shutdown => break,
                SubscribeStep::Lifecycle(Ok(event)) => {
                    if sub.concerns(&event) {
                        ending = ending.merge(sub.observe().await);
                    }
                }
                // Events for this session may be among those dropped.
                SubscribeStep::Lifecycle(Err(broadcast::error::RecvError::Lagged(_))) => {
                    ending = ending.merge(sub.observe().await);
                }
                SubscribeStep::Lifecycle(Err(broadcast::error::RecvError::Closed)) => {
                    lifecycle_open = false;
                }
                SubscribeStep::Item(Ok(Some((ordinal, envelope)))) => {
                    if ordinal > delivered {
                        match sub.envelope_event(ordinal, envelope) {
                            Ok(event) => yield event,
                            Err(e) => {
                                yield error_event(&e);
                                break;
                            }
                        }
                        delivered = ordinal;
                        idle.as_mut().reset(Instant::now() + ctx.keepalive);
                    }
                }
                SubscribeStep::Item(Ok(None)) => {
                    // The bus channel closed under a live receiver, which
                    // the runtime never does; the stream cannot continue.
                    yield error_event(
                        &RequestError::new("INTERNAL_ERROR", "the session stream closed")
                            .with_session(&sub.session_id),
                    );
                    break;
                }
                SubscribeStep::Item(Err(lagged)) => {
                    yield error_event(&sub.lagged(lagged));
                    break;
                }
                SubscribeStep::Idle => {
                    yield Bytes::from_static(KEEPALIVE);
                    idle.as_mut().reset(Instant::now() + ctx.keepalive);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn accept(values: &[&str]) -> bool {
        let values: Vec<HeaderValue> = values
            .iter()
            .map(|v| HeaderValue::from_str(v).unwrap())
            .collect();
        accepts_event_stream(values.iter())
    }

    #[test]
    fn accept_parsing() {
        assert!(accept(&[]), "missing Accept admits anything");
        for ok in [
            "text/event-stream",
            "TEXT/Event-Stream",
            "*/*",
            "text/*",
            "text/event-stream;q=0.5, application/json",
            "application/json, text/event-stream",
            "application/json;q=1, */*;q=0.1",
            "text/event-stream; charset=utf-8",
            "text/*;q=0, text/event-stream;q=0.001",
            "*/*;q=0, text/event-stream",
            "application/json;foo=\"a,b\", text/event-stream",
        ] {
            assert!(accept(&[ok]), "{ok}");
        }
        assert!(
            accept(&["application/json", "text/event-stream"]),
            "two headers"
        );
        for refused in [
            "application/json",
            "",
            "text/html",
            "text/event-stream;q=0",
            "text/event-stream;q=0.000, */*",
            "text/*;q=0",
            "*/*;q=0",
            "text/event-stream;q=2",
            "text/event-stream;q=abc",
            "application/json;foo=\"text/event-stream,\"",
        ] {
            assert!(!accept(&[refused]), "{refused}");
        }
        assert!(!accept(&["application/json", "text/html"]));
    }

    #[test]
    fn qvalues() {
        for (raw, want) in [
            ("0", Some(0.0)),
            ("1", Some(1.0)),
            ("0.5", Some(0.5)),
            ("0.001", Some(0.001)),
            ("1.000", Some(1.0)),
            ("1.", Some(1.0)),
        ] {
            assert_eq!(parse_qvalue(raw), want, "{raw}");
        }
        for bad in ["1.5", "0.0001", "2", "-0", ".5", "", "x", "1.001"] {
            assert_eq!(parse_qvalue(bad), None, "{bad}");
        }
    }

    fn headers(ids: &[&[u8]]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for id in ids {
            h.append("last-event-id", HeaderValue::from_bytes(id).unwrap());
        }
        h
    }

    #[test]
    fn last_event_id_rules() {
        assert_eq!(after_sequence(&headers(&[]), 0), Ok(0));
        assert_eq!(after_sequence(&headers(&[]), 7), Ok(7));
        assert_eq!(after_sequence(&headers(&[b"41"]), 0), Ok(41));
        assert_eq!(after_sequence(&headers(&[b"41"]), 41), Ok(41));
        assert_eq!(after_sequence(&headers(&[b"0"]), 0), Ok(0));
        assert_eq!(
            after_sequence(&headers(&[b"43"]), 41),
            Err("Last-Event-ID 43 conflicts with after_sequence 41".into())
        );
        for bad in [
            &b""[..],
            b"-1",
            b"+1",
            b"1.0",
            b" 1",
            b"abc",
            b"1e3",
            b"18446744073709551616",
        ] {
            assert!(after_sequence(&headers(&[bad]), 0).is_err(), "{bad:?}");
        }
        assert!(after_sequence(&headers(&[b"1", b"1"]), 0).is_err());
        assert_eq!(
            after_sequence(&headers(&[b"18446744073709551615"]), 0),
            Ok(u64::MAX)
        );
    }

    #[test]
    fn framing() {
        assert_eq!(
            data_event(b"{\"a\":1}", Some(42)),
            Bytes::from_static(b"id: 42\ndata: {\"a\":1}\n\n")
        );
        assert_eq!(data_event(b"{}", None), Bytes::from_static(b"data: {}\n\n"));
        let e = error_event(&RequestError::new("STREAM_LAGGED", "x").with_session("s"));
        let text = std::str::from_utf8(&e).unwrap();
        let data = text
            .strip_prefix("event: error\ndata: ")
            .and_then(|r| r.strip_suffix("\n\n"))
            .unwrap();
        assert!(!data.contains('\n'), "one line");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(data).unwrap(),
            serde_json::json!({"error": {"code": "STREAM_LAGGED", "message": "x",
                "session_id": "s", "message_id": "", "details": ""}})
        );
    }

    // ───── stream-level tests over an in-process runtime (no socket) ─────

    use crate::log_store::LogStore;
    use crate::registry::SessionRegistry;
    use crate::security::SecurityLayer;
    use prost::Message as _;

    const DECISION: &str = "macp.mode.decision.v1";
    const ORCH: &str = "agent://orchestrator";

    fn server() -> MacpServer {
        let runtime = Arc::new(
            Runtime::new(
                Arc::new(crate::storage::MemoryBackend),
                Arc::new(SessionRegistry::new()),
                Arc::new(LogStore::new()),
            )
            .with_compact_terminal_logs(false),
        );
        MacpServer::new(runtime, SecurityLayer::dev_mode())
    }

    fn identity() -> AuthIdentity {
        AuthIdentity {
            sender: ORCH.into(),
            allowed_modes: None,
            can_start_sessions: true,
            max_open_sessions: None,
            can_manage_mode_registry: false,
            is_observer: false,
        }
    }

    fn env(kind: &str, id: &str, sid: &str, payload: Vec<u8>) -> Envelope {
        Envelope {
            macp_version: "1.0".into(),
            mode: DECISION.into(),
            message_type: kind.into(),
            message_id: id.into(),
            session_id: sid.into(),
            sender: ORCH.into(),
            timestamp_unix_ms: chrono::Utc::now().timestamp_millis(),
            payload,
        }
    }

    async fn accept_env(server: &MacpServer, e: Envelope) {
        server.runtime().process(&e, None).await.expect("accepted");
    }

    async fn start(server: &MacpServer, sid: &str) {
        let payload = crate::pb::SessionStartPayload {
            intent: "sse".into(),
            participants: vec![ORCH.into()],
            mode_version: "1.0.0".into(),
            configuration_version: "config.default".into(),
            policy_version: "policy.default".into(),
            ttl_ms: 600_000,
            ..Default::default()
        };
        accept_env(
            server,
            env("SessionStart", "start", sid, payload.encode_to_vec()),
        )
        .await;
    }

    async fn propose(server: &MacpServer, sid: &str, id: &str) {
        let payload = crate::decision_pb::ProposalPayload {
            proposal_id: id.into(),
            option: "a".into(),
            rationale: "r".into(),
            ..Default::default()
        };
        accept_env(server, env("Proposal", id, sid, payload.encode_to_vec())).await;
    }

    async fn commit(server: &MacpServer, sid: &str) {
        let vote = crate::decision_pb::VotePayload {
            proposal_id: "p1".into(),
            vote: "APPROVE".into(),
            reason: "ok".into(),
        };
        accept_env(server, env("Vote", "vote", sid, vote.encode_to_vec())).await;
        let payload = crate::pb::CommitmentPayload {
            commitment_id: "c1".into(),
            action: "decision.selected".into(),
            authority_scope: "s".into(),
            reason: "r".into(),
            mode_version: "1.0.0".into(),
            policy_version: "policy.default".into(),
            configuration_version: "config.default".into(),
            outcome_positive: true,
            supersedes: None,
        };
        accept_env(
            server,
            env("Commitment", "commit", sid, payload.encode_to_vec()),
        )
        .await;
    }

    /// Open a subscribe stream as `subscribe` does, with `hooks`.
    async fn open_stream(
        server: &MacpServer,
        sid: &str,
        after: u64,
        hooks: Hooks,
        keepalive: Duration,
    ) -> (
        std::pin::Pin<Box<dyn Stream<Item = Bytes> + Send>>,
        watch::Sender<bool>,
    ) {
        let runtime = Arc::clone(server.runtime());
        let lifecycle = runtime.subscribe_session_lifecycle();
        let cursor = open(server, &identity(), sid, after).await.unwrap();
        let (tx, rx) = watch::channel(false);
        let sub = Subscription {
            runtime,
            method: json::method_descriptor("StreamSession").unwrap(),
            session_id: sid.into(),
            cursor,
            lifecycle,
        };
        let stream = subscribe_events(sub, after, Context::new(keepalive, rx), hooks);
        (Box::pin(stream), tx)
    }

    /// `(id, data or event name)` per event, until the stream ends.
    async fn collect(stream: std::pin::Pin<Box<dyn Stream<Item = Bytes> + Send>>) -> Vec<String> {
        let chunks: Vec<Bytes> = stream.collect().await;
        chunks
            .iter()
            .map(|c| String::from_utf8(c.to_vec()).unwrap())
            .collect()
    }

    fn id_of(event: &str) -> Option<u64> {
        event
            .strip_prefix("id: ")
            .and_then(|r| r.split('\n').next())
            .and_then(|n| n.parse().ok())
    }

    const LONG: Duration = Duration::from_secs(3600);

    /// The Resolved lifecycle event is observed before any Envelope is read
    /// (the bus is held until then, and the stream is parked before the
    /// commit): the stream still yields everything through the Commitment
    /// with its id and then ends normally. 200 times. Closing on `Resolved`
    /// without waiting for the last ordinal fails this test with no ids.
    #[tokio::test]
    async fn resolved_event_first_never_closes_before_the_commitment() {
        for round in 0..200 {
            let parked = Arc::new(tokio::sync::Notify::new());
            let hooks = Hooks {
                hold_bus_until_resolved: true,
                parked: Some(Arc::clone(&parked)),
            };
            let server = server();
            let sid = uuid::Uuid::new_v4().to_string();
            start(&server, &sid).await;
            propose(&server, &sid, "p1").await;
            let (stream, _tx) = open_stream(&server, &sid, 0, hooks, LONG).await;
            // The reader runs first and parks having observed `Open`, with
            // the bus held: nothing (not even the replay) is read before
            // `Resolved` is observed, after the Commitment is in the log
            // but, at the earliest, before it reaches the bus.
            let reader = tokio::spawn(collect(stream));
            parked.notified().await;
            commit(&server, &sid).await;
            let events = reader.await.unwrap();
            let ids: Vec<u64> = events.iter().filter_map(|e| id_of(e)).collect();
            assert_eq!(ids, vec![1, 2, 3, 4], "round {round}: {events:?}");
            let last = events.last().unwrap();
            assert!(last.contains("\"message_type\":\"Commitment\""), "{last}");
            assert!(
                !events.iter().any(|e| e.starts_with("event: error")),
                "round {round}: {events:?}"
            );
        }
    }

    /// Without the hook, live: replay, then the live Commitment, then a
    /// normal end.
    #[tokio::test]
    async fn live_commitment_ends_normally() {
        let server = server();
        let sid = uuid::Uuid::new_v4().to_string();
        start(&server, &sid).await;
        let (stream, _tx) = open_stream(&server, &sid, 0, Hooks::default(), LONG).await;
        let reader = tokio::spawn(collect(stream));
        propose(&server, &sid, "p1").await;
        commit(&server, &sid).await;
        let events = reader.await.unwrap();
        let ids: Vec<u64> = events.iter().filter_map(|e| id_of(e)).collect();
        assert_eq!(ids, vec![1, 2, 3, 4], "{events:?}");
    }

    /// A consumer that stops reading lags the bus (capacity 256): the stream
    /// ends with `event: error` `STREAM_LAGGED`, framed as §4.3 requires.
    #[tokio::test]
    async fn lag_is_a_fatal_error_event() {
        let server = server();
        let sid = uuid::Uuid::new_v4().to_string();
        start(&server, &sid).await;
        let (stream, _tx) = open_stream(&server, &sid, 0, Hooks::default(), LONG).await;
        for i in 0..300 {
            propose(&server, &sid, &format!("p{i}")).await;
        }
        let events = collect(stream).await;
        let last = events.last().unwrap();
        assert!(
            last.starts_with("event: error\ndata: {\"error\":{\"code\":\"STREAM_LAGGED\""),
            "{last}"
        );
        assert!(last.ends_with("\n\n"));
        // The replay (SessionStart) came first, with its id.
        assert_eq!(id_of(&events[0]), Some(1));
    }

    /// Shutdown ends a stream at once, as a lost connection (no error).
    #[tokio::test]
    async fn shutdown_ends_the_stream_without_an_error() {
        let server = server();
        let sid = uuid::Uuid::new_v4().to_string();
        start(&server, &sid).await;
        let (mut stream, tx) = open_stream(&server, &sid, 0, Hooks::default(), LONG).await;
        assert_eq!(
            id_of(std::str::from_utf8(&stream.next().await.unwrap()).unwrap()),
            Some(1)
        );
        tx.send_replace(true);
        assert!(stream.next().await.is_none());
    }

    /// A keepalive comment after the idle interval, and only then; the
    /// production interval is §4.3's 30 s.
    #[tokio::test]
    async fn keepalive_after_idle() {
        assert_eq!(KEEPALIVE_INTERVAL, Duration::from_secs(30));
        let interval = Duration::from_millis(40);
        let server = server();
        let sid = uuid::Uuid::new_v4().to_string();
        start(&server, &sid).await;
        let (mut stream, _tx) = open_stream(&server, &sid, 0, Hooks::default(), interval).await;
        let first = stream.next().await.unwrap();
        assert!(first.starts_with(b"id: 1\n"));
        let before = Instant::now();
        let next = stream.next().await.unwrap();
        assert_eq!(next, Bytes::from_static(b": keepalive\n\n"));
        assert!(before.elapsed() >= interval / 2);
    }

    /// An `after_sequence` above the high water opens at the high water read
    /// before the bus subscribe: ids are never below or at the client's
    /// position, and each id is the true ordinal.
    #[tokio::test]
    async fn after_sequence_above_the_high_water_keeps_ids_exact() {
        let server = server();
        let sid = uuid::Uuid::new_v4().to_string();
        start(&server, &sid).await; // 1
        propose(&server, &sid, "p1").await; // 2
        let (stream, _tx) = open_stream(&server, &sid, 4, Hooks::default(), LONG).await;
        let reader = tokio::spawn(collect(stream));
        propose(&server, &sid, "p2").await; // 3: dropped (<= 4)
        propose(&server, &sid, "p3").await; // 4: dropped
        propose(&server, &sid, "p4").await; // 5
        commit(&server, &sid).await; // 6 vote, 7 commitment
        let events = reader.await.unwrap();
        let ids: Vec<u64> = events.iter().filter_map(|e| id_of(e)).collect();
        assert_eq!(ids, vec![5, 6, 7], "{events:?}");
        assert!(events[0].contains("\"message_id\":\"p4\""), "{events:?}");
    }
}

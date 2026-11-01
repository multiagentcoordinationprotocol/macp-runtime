//! Request-level errors (RFC-MACP-0006 §4.5) and the JSON responses of the
//! binding.
//!
//! A request-level error answers with the HTTP status the error-codes
//! registry gives its code (`macp_core::error::registry_entry`), the body
//! `{"error": <MACPError>}` in the canonical JSON mapping (RFC-MACP-0001 §10,
//! so `details` is standard padded base64), and the code again in a
//! `MACP-Error-Code` header; a `401` also carries `WWW-Authenticate: Bearer`.
//!
//! A gRPC [`Status`] from a `MacpServer` handler maps through its
//! `macp-error-code` metadata (every status the server builds carries one)
//! and its `macp-error-details-bin` bytes. `UNIMPLEMENTED` is the §4.1
//! transport `404` with no body. A status with no registry code (none is
//! expected) falls back by its gRPC code, else `500 INTERNAL_ERROR`.
//!
//! Every body the binding writes is `Content-Type: application/json`, never
//! `application/macp-envelope+json` (§4.2), and no response ever carries
//! `Set-Cookie` (§4.6).

use bytes::Bytes;
use http::header::{HeaderName, HeaderValue, CONTENT_TYPE, WWW_AUTHENTICATE};
use http::{Response, StatusCode};
use http_body_util::{BodyExt, Empty, Full};
use macp_pb::pb::MacpError as PbMacpError;
use prost_reflect::DynamicMessage;
use tonic::{Code, Status};

use super::json;
use super::server::Body;

/// The `MACP-Error-Code` response header (§4.5).
pub(crate) const MACP_ERROR_CODE: HeaderName = HeaderName::from_static("macp-error-code");

const APPLICATION_JSON: HeaderValue = HeaderValue::from_static("application/json");

/// A request-level error, before it becomes a response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RequestError {
    pub(crate) code: &'static str,
    pub(crate) message: String,
    pub(crate) session_id: String,
    pub(crate) details: Vec<u8>,
}

impl RequestError {
    pub(crate) fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            session_id: String::new(),
            details: Vec::new(),
        }
    }

    /// `401 UNAUTHENTICATED` from the request headers.
    pub(crate) fn unauthenticated(err: &macp_core::error::MacpError) -> Self {
        Self::new(err.error_code(), err.to_string())
    }

    /// The registry's HTTP status for this code; `500` for a code the
    /// registry does not list (never built here).
    pub(crate) fn http_status(&self) -> StatusCode {
        macp_core::error::registry_entry(self.code)
            .and_then(|e| StatusCode::from_u16(e.http).ok())
            .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR)
    }

    /// The `{"error": <MACPError>}` body, canonical JSON (§4.5).
    pub(crate) fn body(&self) -> Vec<u8> {
        let error = PbMacpError {
            code: self.code.to_string(),
            message: self.message.clone(),
            session_id: self.session_id.clone(),
            message_id: String::new(),
            details: self.details.clone(),
        };
        let mut message = DynamicMessage::new(
            json::descriptor_pool()
                .get_message_by_name("macp.v1.MACPError")
                .expect("macp.v1.MACPError is in the descriptor pool"),
        );
        let encoded = match message.transcode_from(&error) {
            Ok(()) => json::encode_message(&message),
            // A generated MACPError always transcodes to its own descriptor.
            Err(_) => serde_json::Value::Null,
        };
        serde_json::to_vec(&serde_json::json!({ "error": encoded })).unwrap_or_default()
    }

    pub(crate) fn into_response(self) -> Response<Body> {
        let status = self.http_status();
        let mut resp = json_response(status, self.body());
        let headers = resp.headers_mut();
        headers.insert(MACP_ERROR_CODE, HeaderValue::from_static(self.code));
        if status == StatusCode::UNAUTHORIZED {
            headers.insert(WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        }
        resp
    }
}

/// A `MacpServer` status as the binding answers it.
pub(crate) fn status_response(status: &Status, session_id: &str) -> Response<Body> {
    match RequestError::from_status(status, session_id) {
        Some(err) => err.into_response(),
        None => empty(StatusCode::NOT_FOUND),
    }
}

impl RequestError {
    /// Fill the error's `session_id`.
    pub(crate) fn with_session(mut self, session_id: &str) -> Self {
        self.session_id = session_id.to_string();
        self
    }

    /// A `MacpServer` status as a request-level error: its registry code
    /// (`macp-error-code`, else a fallback by gRPC code) and its details
    /// bytes. `None` for `UNIMPLEMENTED`, which is the §4.1 transport `404`.
    pub(crate) fn from_status(status: &Status, session_id: &str) -> Option<Self> {
        if status.code() == Code::Unimplemented {
            return None;
        }
        Some(Self::from_coded_status(status, session_id))
    }

    /// [`Self::from_status`] for a status that must become an error whatever
    /// its gRPC code (a fatal SSE `event: error`, §4.3): `UNIMPLEMENTED`
    /// falls back to `INTERNAL_ERROR` like any other uncoded status.
    pub(crate) fn from_coded_status(status: &Status, session_id: &str) -> Self {
        let code = status
            .metadata()
            .get("macp-error-code")
            .and_then(|v| v.to_str().ok())
            .and_then(macp_core::error::registry_entry)
            .map(|e| e.code)
            .unwrap_or_else(|| fallback_code(status.code()));
        let details = status
            .metadata()
            .get_bin("macp-error-details-bin")
            .and_then(|v| v.to_bytes().ok())
            .map(|b| b.to_vec())
            .unwrap_or_default();
        RequestError {
            code,
            message: status.message().to_string(),
            session_id: session_id.to_string(),
            details,
        }
    }
}

/// The registry code for a status that carries none, by its gRPC code.
fn fallback_code(code: Code) -> &'static str {
    match code {
        Code::Unauthenticated => "UNAUTHENTICATED",
        Code::PermissionDenied => "FORBIDDEN",
        Code::InvalidArgument => "INVALID_REQUEST",
        _ => "INTERNAL_ERROR",
    }
}

/// A `200`-style JSON response.
pub(crate) fn json_response(status: StatusCode, body: Vec<u8>) -> Response<Body> {
    let mut resp = Response::new(Full::new(Bytes::from(body)).boxed_unsync());
    *resp.status_mut() = status;
    resp.headers_mut().insert(CONTENT_TYPE, APPLICATION_JSON);
    resp
}

/// A transport status with no body (§4.5: 404, 405, 406, 415).
pub(crate) fn empty(status: StatusCode) -> Response<Body> {
    let mut resp = Response::new(Empty::new().boxed_unsync());
    *resp.status_mut() = status;
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parts(resp: Response<Body>) -> (StatusCode, http::HeaderMap) {
        let (p, _) = resp.into_parts();
        (p.status, p.headers)
    }

    #[test]
    fn every_registry_code_maps_to_its_http_status() {
        for entry in macp_core::error::ERROR_CODE_REGISTRY {
            let err = RequestError::new(entry.code, "x");
            assert_eq!(err.http_status().as_u16(), entry.http, "{}", entry.code);
            let (s, h) = parts(err.into_response());
            assert_eq!(s.as_u16(), entry.http);
            assert_eq!(h.get(MACP_ERROR_CODE).unwrap(), entry.code);
            assert_eq!(h.get(CONTENT_TYPE).unwrap(), "application/json");
            assert_eq!(h.contains_key(WWW_AUTHENTICATE), entry.http == 401);
        }
    }

    #[test]
    fn statuses_map_through_their_code_and_details() {
        let mut status = Status::new(Code::FailedPrecondition, "compacted");
        status.metadata_mut().insert(
            "macp-error-code",
            tonic::metadata::MetadataValue::from_static("HISTORY_UNAVAILABLE"),
        );
        status.metadata_mut().insert_bin(
            "macp-error-details-bin",
            tonic::metadata::MetadataValue::from_bytes(b"{\"lowest_available_sequence\":4}"),
        );
        let (p, body) = status_response(&status, "s1").into_parts();
        assert_eq!(p.status, StatusCode::CONFLICT);
        let body = futures_executor_block(body);
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["error"]["code"], "HISTORY_UNAVAILABLE");
        assert_eq!(v["error"]["session_id"], "s1");
        assert_eq!(v["error"]["message_id"], "");
        assert_eq!(
            v["error"]["details"],
            "eyJsb3dlc3RfYXZhaWxhYmxlX3NlcXVlbmNlIjo0fQ=="
        );

        let (s, _) = parts(status_response(&Status::unimplemented("no"), ""));
        assert_eq!(s, StatusCode::NOT_FOUND);
        for (code, want) in [
            (Code::Unauthenticated, 401),
            (Code::PermissionDenied, 403),
            (Code::InvalidArgument, 400),
            (Code::Unavailable, 500),
            (Code::Internal, 500),
        ] {
            let (s, h) = parts(status_response(&Status::new(code, "uncoded"), ""));
            assert_eq!(s.as_u16(), want, "{code:?}");
            assert!(h.contains_key(MACP_ERROR_CODE));
        }
        // An unknown code string falls back by gRPC code too.
        let mut odd = Status::new(Code::NotFound, "x");
        odd.metadata_mut().insert(
            "macp-error-code",
            tonic::metadata::MetadataValue::from_static("NOT_A_CODE"),
        );
        let (s, h) = parts(status_response(&odd, ""));
        assert_eq!(s, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(h.get(MACP_ERROR_CODE).unwrap(), "INTERNAL_ERROR");
    }

    fn futures_executor_block(body: Body) -> Bytes {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(body.collect())
            .unwrap()
            .to_bytes()
    }
}

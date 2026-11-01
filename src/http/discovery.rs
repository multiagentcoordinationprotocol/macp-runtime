//! The well-known manifest (RFC-MACP-0005 §7.1, RFC-MACP-0006 §4.11).
//!
//! `GET /.well-known/macp.json` (and `HEAD`, the same head with no body)
//! answers `200` with the runtime's `AgentManifest` in the canonical JSON
//! mapping (RFC-MACP-0001 §10), labelled `application/macp-manifest+json`.
//! It is served at the origin root whatever path prefix `{base}` has, and is
//! unauthenticated, like `GetManifest` on gRPC: the document is built by the
//! same `MacpServer::get_manifest` both bindings serve, so its
//! `transport_endpoints` (the HTTP entry when `MACP_HTTP_PUBLIC_URL` is set)
//! are the ones `GetManifest` returns. Any other method is `405` with
//! `Allow: GET, HEAD`.

use bytes::Bytes;
use http::header::{HeaderValue, ALLOW, CONTENT_LENGTH, CONTENT_TYPE};
use http::{Method, Response, StatusCode};
use http_body_util::{BodyExt, Empty, Full};
use prost_reflect::DynamicMessage;

use super::errors::{empty, status_response};
use super::json;
use super::server::Body;
use crate::pb::macp_runtime_service_server::MacpRuntimeService;
use crate::pb::GetManifestRequest;
use crate::server::MacpServer;

/// The media type of the well-known document (`registries/media-types.md`).
pub(crate) const MANIFEST_MEDIA_TYPE: &str = "application/macp-manifest+json";

/// Answer a request for the well-known path.
pub(crate) async fn well_known(server: &MacpServer, method: &Method) -> Response<Body> {
    let head = match *method {
        Method::GET => false,
        Method::HEAD => true,
        _ => {
            let mut resp = empty(StatusCode::METHOD_NOT_ALLOWED);
            resp.headers_mut()
                .insert(ALLOW, HeaderValue::from_static("GET, HEAD"));
            return resp;
        }
    };
    let manifest = match MacpRuntimeService::get_manifest(
        server,
        tonic::Request::new(GetManifestRequest::default()),
    )
    .await
    {
        Ok(resp) => resp.into_inner().manifest.unwrap_or_default(),
        // The runtime's own manifest is never refused; mapped like any status.
        Err(status) => return status_response(&status, ""),
    };
    let body = manifest_json(&manifest);
    let len = body.len();
    let body: Body = if head {
        Empty::new().boxed_unsync()
    } else {
        Full::new(Bytes::from(body)).boxed_unsync()
    };
    let mut resp = Response::new(body);
    let headers = resp.headers_mut();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static(MANIFEST_MEDIA_TYPE));
    headers.insert(CONTENT_LENGTH, HeaderValue::from(len));
    resp
}

/// The canonical JSON text of `manifest`.
pub(crate) fn manifest_json(manifest: &crate::pb::AgentManifest) -> Vec<u8> {
    let mut message = DynamicMessage::new(
        json::descriptor_pool()
            .get_message_by_name("macp.v1.AgentManifest")
            .expect("macp.v1.AgentManifest is in the descriptor pool"),
    );
    // A generated AgentManifest always transcodes to its own descriptor.
    if message.transcode_from(manifest).is_err() {
        return b"null".to_vec();
    }
    json::encode_response(&message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn server() -> MacpServer {
        let runtime = Arc::new(crate::runtime::Runtime::new(
            Arc::new(crate::storage::MemoryBackend),
            Arc::new(crate::registry::SessionRegistry::new()),
            Arc::new(crate::log_store::LogStore::new()),
        ));
        MacpServer::new(runtime, crate::security::SecurityLayer::dev_mode())
    }

    async fn get(server: &MacpServer, method: Method) -> (StatusCode, http::HeaderMap, Bytes) {
        let (p, b) = well_known(server, &method).await.into_parts();
        (p.status, p.headers, b.collect().await.unwrap().to_bytes())
    }

    #[tokio::test]
    async fn serves_the_get_manifest_document() {
        for (srv, endpoints) in [
            (server(), serde_json::json!([])),
            (
                server().with_http_endpoint("https://h.example/api"),
                serde_json::json!([{
                    "transport": "macp.transport.http.v1",
                    "uri": "https://h.example/api",
                    "content_types": ["application/json"],
                    "metadata": {},
                }]),
            ),
        ] {
            let (s, h, body) = get(&srv, Method::GET).await;
            assert_eq!(s, StatusCode::OK);
            assert_eq!(h[CONTENT_TYPE], MANIFEST_MEDIA_TYPE);
            assert_eq!(h[CONTENT_LENGTH], body.len().to_string());
            let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(v["agent_id"], "macp-runtime");
            assert_eq!(v["transport_endpoints"], endpoints);

            // Byte-identical to the canonical JSON of GetManifest's manifest.
            let grpc = MacpRuntimeService::get_manifest(
                &srv,
                tonic::Request::new(GetManifestRequest::default()),
            )
            .await
            .unwrap()
            .into_inner()
            .manifest
            .unwrap();
            assert_eq!(body.as_ref(), manifest_json(&grpc).as_slice());

            let (s, h, head) = get(&srv, Method::HEAD).await;
            assert_eq!(s, StatusCode::OK);
            assert_eq!(h[CONTENT_TYPE], MANIFEST_MEDIA_TYPE);
            assert_eq!(h[CONTENT_LENGTH], body.len().to_string());
            assert!(head.is_empty());
        }
    }

    #[tokio::test]
    async fn other_methods_are_405() {
        for m in [Method::POST, Method::PUT, Method::DELETE, Method::PATCH] {
            let (s, h, body) = get(&server(), m.clone()).await;
            assert_eq!(s, StatusCode::METHOD_NOT_ALLOWED, "{m}");
            assert_eq!(h[ALLOW], "GET, HEAD");
            assert!(body.is_empty());
        }
    }
}

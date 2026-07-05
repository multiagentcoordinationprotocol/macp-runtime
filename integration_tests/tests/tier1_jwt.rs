//! Tier 1 — JWT auth integration tests through the real gRPC boundary.
//!
//! Spins up a dedicated runtime subprocess with `MACP_AUTH_ISSUER` +
//! `MACP_AUTH_JWKS_JSON` set (inline symmetric HS256 JWKS) and verifies:
//!   * Valid signed JWT → accepted, sender derived from `sub` claim
//!   * `macp_scopes` claim is honored (observer flag grants read-any-session)
//!   * Expired JWT → UNAUTHENTICATED
//!   * Wrong issuer → UNAUTHENTICATED
//!   * Wrong signature → UNAUTHENTICATED
//!   * No bearer header → UNAUTHENTICATED
//!   * Opaque (non-JWT) bearer → UNAUTHENTICATED (JWT is the only resolver)

use base64::Engine;
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use macp_integration_tests::server_manager::ServerManager;
use macp_runtime::pb::macp_runtime_service_client::MacpRuntimeServiceClient;
use macp_runtime::pb::{Envelope, GetSessionRequest, SendRequest, SessionStartPayload};
use prost::Message;
use serde::Serialize;
use std::sync::OnceLock;
use tokio::sync::Mutex;
use tonic::transport::Channel;
use tonic::Request;

const ISSUER: &str = "https://issuer.test";
const AUDIENCE: &str = "macp-runtime";
const SECRET: &[u8] = b"integration-test-hmac-secret-32b";

const MODE_DECISION: &str = "macp.mode.decision.v1";
const MODE_VERSION: &str = "1.0.0";
const CONFIG_VERSION: &str = "config.default";

static JWT_SERVER: OnceLock<Mutex<Option<ServerManager>>> = OnceLock::new();
static JWT_ENDPOINT: OnceLock<String> = OnceLock::new();

fn jwks_json() -> String {
    let k = base64::engine::general_purpose::STANDARD.encode(SECRET);
    serde_json::json!({ "keys": [ { "kty": "oct", "alg": "HS256", "k": k } ] }).to_string()
}

async fn endpoint() -> &'static str {
    if let Some(ep) = JWT_ENDPOINT.get() {
        return ep.as_str();
    }
    let binary = std::env::var("MACP_TEST_BINARY").unwrap_or_else(|_| {
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        format!("{manifest_dir}/../target/debug/macp-runtime")
    });
    let jwks = jwks_json();
    let manager = ServerManager::start_with_env(
        &binary,
        &[
            ("MACP_AUTH_ISSUER", ISSUER),
            ("MACP_AUTH_AUDIENCE", AUDIENCE),
            ("MACP_AUTH_JWKS_JSON", jwks.as_str()),
            // This harness signs with a symmetric key; HS256 is no longer in
            // the default allowlist (RS256/ES256 only) and must be opted into
            // explicitly — which also exercises the env knob end-to-end.
            ("MACP_AUTH_JWT_ALGS", "HS256"),
        ],
    )
    .await
    .expect("failed to start JWT-configured MACP runtime");
    let ep = manager.endpoint.clone();
    let _ = JWT_ENDPOINT.set(ep);
    let _ = JWT_SERVER.set(Mutex::new(Some(manager)));
    JWT_ENDPOINT.get().unwrap().as_str()
}

async fn client() -> MacpRuntimeServiceClient<Channel> {
    let ep = endpoint().await;
    MacpRuntimeServiceClient::connect(ep.to_string())
        .await
        .expect("failed to connect")
}

#[derive(Serialize)]
struct Claims<'a> {
    sub: &'a str,
    iss: &'a str,
    aud: &'a str,
    exp: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    macp_scopes: Option<serde_json::Value>,
}

fn sign_with_secret(secret: &[u8], claims: &Claims) -> String {
    encode(
        &Header::new(Algorithm::HS256),
        claims,
        &EncodingKey::from_secret(secret),
    )
    .unwrap()
}

fn sign(claims: &Claims) -> String {
    sign_with_secret(SECRET, claims)
}

fn with_bearer<T>(token: &str, inner: T) -> Request<T> {
    let mut req = Request::new(inner);
    req.metadata_mut()
        .insert("authorization", format!("Bearer {token}").parse().unwrap());
    req
}

fn new_session_id() -> String {
    uuid::Uuid::new_v4().as_hyphenated().to_string()
}

fn new_message_id() -> String {
    uuid::Uuid::new_v4().as_hyphenated().to_string()
}

fn session_start_envelope(sid: &str, participants: &[&str]) -> Envelope {
    let payload = SessionStartPayload {
        intent: "JWT auth test".into(),
        participants: participants.iter().map(|s| s.to_string()).collect(),
        mode_version: MODE_VERSION.into(),
        configuration_version: CONFIG_VERSION.into(),
        policy_version: String::new(),
        ttl_ms: 60_000,
        context_id: String::new(),
        extensions: std::collections::HashMap::new(),
        roots: vec![],
    }
    .encode_to_vec();
    Envelope {
        macp_version: "1.0".into(),
        mode: MODE_DECISION.into(),
        message_type: "SessionStart".into(),
        message_id: new_message_id(),
        session_id: sid.into(),
        sender: String::new(),
        timestamp_unix_ms: chrono::Utc::now().timestamp_millis(),
        payload,
    }
}

fn now_plus(secs: i64) -> i64 {
    chrono::Utc::now().timestamp() + secs
}

// ── Success paths ────────────────────────────────────────────────

#[tokio::test]
async fn valid_jwt_authenticates_and_derives_sender_from_sub() {
    let mut c = client().await;
    let token = sign(&Claims {
        sub: "agent://jwt-alice",
        iss: ISSUER,
        aud: AUDIENCE,
        exp: now_plus(300),
        macp_scopes: None,
    });

    let sid = new_session_id();
    let env = session_start_envelope(&sid, &["agent://jwt-alice", "agent://jwt-bob"]);
    let resp = c
        .send(with_bearer(
            &token,
            SendRequest {
                envelope: Some(env),
            },
        ))
        .await
        .expect("send succeeded");
    let ack = resp.into_inner().ack.unwrap();
    assert!(ack.ok, "ack error: {:?}", ack.error);

    // Verify the runtime used `sub` as the authenticated sender.
    let get = c
        .get_session(with_bearer(
            &token,
            GetSessionRequest {
                session_id: sid.clone(),
            },
        ))
        .await
        .unwrap();
    let meta = get.into_inner().metadata.unwrap();
    assert_eq!(meta.initiator, "agent://jwt-alice");
    assert_eq!(meta.session_id, sid);
}

#[tokio::test]
async fn observer_scope_can_get_session_for_non_participant() {
    let mut c = client().await;

    // Owner starts a session excluding the observer.
    let owner_token = sign(&Claims {
        sub: "agent://jwt-owner",
        iss: ISSUER,
        aud: AUDIENCE,
        exp: now_plus(300),
        macp_scopes: None,
    });
    let sid = new_session_id();
    let env = session_start_envelope(&sid, &["agent://jwt-owner", "agent://jwt-peer"]);
    let resp = c
        .send(with_bearer(
            &owner_token,
            SendRequest {
                envelope: Some(env),
            },
        ))
        .await
        .unwrap();
    assert!(resp.into_inner().ack.unwrap().ok);

    // Observer (not a participant) should still be able to read via is_observer scope.
    let observer_token = sign(&Claims {
        sub: "agent://jwt-observer",
        iss: ISSUER,
        aud: AUDIENCE,
        exp: now_plus(300),
        macp_scopes: Some(serde_json::json!({ "is_observer": true })),
    });
    let get = c
        .get_session(with_bearer(
            &observer_token,
            GetSessionRequest {
                session_id: sid.clone(),
            },
        ))
        .await
        .expect("observer can read any session");
    assert_eq!(get.into_inner().metadata.unwrap().session_id, sid);

    // A non-observer non-participant should be denied.
    let outsider_token = sign(&Claims {
        sub: "agent://jwt-outsider",
        iss: ISSUER,
        aud: AUDIENCE,
        exp: now_plus(300),
        macp_scopes: None,
    });
    let err = c
        .get_session(with_bearer(
            &outsider_token,
            GetSessionRequest { session_id: sid },
        ))
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::PermissionDenied);
}

// ── Rejection paths ──────────────────────────────────────────────

fn assert_unauthenticated_ack(ack: macp_runtime::pb::Ack) {
    assert!(!ack.ok, "expected auth failure, got ok ack");
    let err = ack.error.expect("auth failure must carry an error");
    assert_eq!(
        err.code, "UNAUTHENTICATED",
        "expected UNAUTHENTICATED, got {err:?}"
    );
}

#[tokio::test]
async fn expired_jwt_is_unauthenticated() {
    let mut c = client().await;
    let token = sign(&Claims {
        sub: "agent://jwt-alice",
        iss: ISSUER,
        aud: AUDIENCE,
        exp: now_plus(-600),
        macp_scopes: None,
    });
    let sid = new_session_id();
    let env = session_start_envelope(&sid, &["agent://jwt-alice"]);
    let resp = c
        .send(with_bearer(
            &token,
            SendRequest {
                envelope: Some(env),
            },
        ))
        .await
        .expect("RPC returned ack");
    assert_unauthenticated_ack(resp.into_inner().ack.unwrap());
}

#[tokio::test]
async fn wrong_issuer_is_unauthenticated() {
    let mut c = client().await;
    let token = sign(&Claims {
        sub: "agent://jwt-alice",
        iss: "https://attacker.example",
        aud: AUDIENCE,
        exp: now_plus(300),
        macp_scopes: None,
    });
    let sid = new_session_id();
    let env = session_start_envelope(&sid, &["agent://jwt-alice"]);
    let resp = c
        .send(with_bearer(
            &token,
            SendRequest {
                envelope: Some(env),
            },
        ))
        .await
        .expect("RPC returned ack");
    assert_unauthenticated_ack(resp.into_inner().ack.unwrap());
}

#[tokio::test]
async fn forged_signature_is_unauthenticated() {
    let mut c = client().await;
    let forged = sign_with_secret(
        b"not-the-server-secret-0123456789",
        &Claims {
            sub: "agent://jwt-alice",
            iss: ISSUER,
            aud: AUDIENCE,
            exp: now_plus(300),
            macp_scopes: None,
        },
    );
    let sid = new_session_id();
    let env = session_start_envelope(&sid, &["agent://jwt-alice"]);
    let resp = c
        .send(with_bearer(
            &forged,
            SendRequest {
                envelope: Some(env),
            },
        ))
        .await
        .expect("RPC returned ack");
    assert_unauthenticated_ack(resp.into_inner().ack.unwrap());
}

#[tokio::test]
async fn missing_authorization_is_unauthenticated() {
    let mut c = client().await;
    let sid = new_session_id();
    let env = session_start_envelope(&sid, &["agent://jwt-alice"]);
    let resp = c
        .send(Request::new(SendRequest {
            envelope: Some(env),
        }))
        .await
        .expect("RPC returned ack");
    assert_unauthenticated_ack(resp.into_inner().ack.unwrap());
}

#[tokio::test]
async fn opaque_bearer_is_unauthenticated_when_only_jwt_is_configured() {
    let mut c = client().await;
    let sid = new_session_id();
    let env = session_start_envelope(&sid, &["agent://jwt-alice"]);
    let resp = c
        .send(with_bearer(
            "static-opaque-token-with-no-dots",
            SendRequest {
                envelope: Some(env),
            },
        ))
        .await
        .expect("RPC returned ack");
    assert_unauthenticated_ack(resp.into_inner().ack.unwrap());
}

/// Security property (master plan §1.6): HS256 must NOT verify under the
/// default algorithm allowlist. A runtime configured with the same symmetric
/// JWKS but WITHOUT the explicit `MACP_AUTH_JWT_ALGS=HS256` opt-in rejects
/// otherwise-valid HS256 tokens.
#[tokio::test]
async fn hs256_rejected_without_explicit_algorithm_opt_in() {
    let binary = std::env::var("MACP_TEST_BINARY").unwrap_or_else(|_| {
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        format!("{manifest_dir}/../target/debug/macp-runtime")
    });
    let jwks = jwks_json();
    let manager = ServerManager::start_with_env(
        &binary,
        &[
            ("MACP_AUTH_ISSUER", ISSUER),
            ("MACP_AUTH_AUDIENCE", AUDIENCE),
            ("MACP_AUTH_JWKS_JSON", jwks.as_str()),
            // No MACP_AUTH_JWT_ALGS: default allowlist (RS256/ES256).
        ],
    )
    .await
    .expect("failed to start default-allowlist MACP runtime");

    let mut client = MacpRuntimeServiceClient::connect(manager.endpoint.clone())
        .await
        .expect("failed to connect");

    let token = sign(&Claims {
        sub: "agent://hs256-default",
        iss: ISSUER,
        aud: AUDIENCE,
        exp: chrono::Utc::now().timestamp() + 300,
        macp_scopes: None,
    });
    // ListSessions requires authentication (GetManifest is open discovery).
    let mut request = tonic::Request::new(macp_runtime::pb::ListSessionsRequest {});
    request.metadata_mut().insert(
        "authorization",
        format!("Bearer {token}").parse().expect("valid header"),
    );
    let err = client
        .list_sessions(request)
        .await
        .expect_err("HS256 token must be rejected under the default allowlist");
    assert_eq!(err.code(), tonic::Code::Unauthenticated);
}

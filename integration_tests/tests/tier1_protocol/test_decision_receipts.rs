//! Decision receipts (RFC-MACP-0013 §13) through the real gRPC boundary:
//! `GetDecisionReceipt`, `GetReceiptKeys`, the `Initialize` capability, and
//! `macp verify --keys-from` against the live runtime.

use base64::Engine as _;
use macp_integration_tests::helpers::*;
use macp_integration_tests::server_manager::ServerManager;
use macp_runtime::pb::macp_runtime_service_client::MacpRuntimeServiceClient;
use macp_runtime::pb::{GetDecisionReceiptRequest, GetReceiptKeysRequest};
use tonic::transport::Channel;

const BUYER: &str = "agent://buyer";
const SELLER: &str = "agent://seller";
const AUDIT: &str = "agent://audit";
const STRANGER: &str = "agent://stranger";
const FIXTURES: &str = "../crates/macp-core/tests/fixtures";

fn test_binary() -> String {
    std::env::var("MACP_TEST_BINARY").unwrap_or_else(|_| "../target/debug/macp-runtime".into())
}

/// The `macp` CLI: `cargo build -p macp-cli` builds it.
fn cli_binary() -> String {
    let cli = std::env::var("MACP_TEST_CLI").unwrap_or_else(|_| "../target/debug/macp".into());
    assert!(
        std::path::Path::new(&cli).exists(),
        "{cli} is missing: build it with `cargo build -p macp-cli` or set MACP_TEST_CLI"
    );
    cli
}

/// Each token is its sender, so `with_sender` authenticates; the audit
/// identity is an observer, the stranger a member of no session.
fn tokens() -> String {
    let id = |s: &str, observer: bool| serde_json::json!({ "token": s, "sender": s, "is_observer": observer });
    serde_json::json!([
        id(BUYER, false),
        id(SELLER, false),
        id(AUDIT, true),
        id(STRANGER, false)
    ])
    .to_string()
}

struct Runtime {
    manager: ServerManager,
    client: MacpRuntimeServiceClient<Channel>,
}

async fn start(data_dir: &str, extra: &[(&str, &str)]) -> Runtime {
    let tokens = tokens();
    let mut env = vec![
        ("MACP_ALLOW_INSECURE", "1"),
        ("MACP_MEMORY_ONLY", "0"),
        ("MACP_DATA_DIR", data_dir),
        ("MACP_AUTH_TOKENS_JSON", tokens.as_str()),
    ];
    env.extend_from_slice(extra);
    let manager = ServerManager::start_with_env(&test_binary(), &env)
        .await
        .expect("server must start");
    let client = MacpRuntimeServiceClient::connect(manager.endpoint.clone())
        .await
        .expect("connect");
    Runtime { manager, client }
}

async fn send_all(
    client: &mut MacpRuntimeServiceClient<Channel>,
    sid: &str,
    steps: Vec<(&str, &str, Vec<u8>)>,
) {
    for (sender, message_type, payload) in steps {
        let ack = send_as(
            client,
            sender,
            envelope(
                MODE_PROPOSAL,
                message_type,
                &new_message_id(),
                sid,
                sender,
                payload,
            ),
        )
        .await
        .unwrap();
        assert!(ack.ok, "{message_type}: {:?}", ack.error);
    }
}

async fn open_session(client: &mut MacpRuntimeServiceClient<Channel>) -> String {
    let sid = new_session_id();
    let start = session_start_payload("buy", &[BUYER, SELLER], 300_000);
    send_all(client, &sid, vec![(BUYER, "SessionStart", start)]).await;
    sid
}

async fn resolved_session(client: &mut MacpRuntimeServiceClient<Channel>) -> String {
    let sid = open_session(client).await;
    let steps = vec![
        (
            SELLER,
            "Proposal",
            proposal_mode_payload("p1", "offer", "terms"),
        ),
        (SELLER, "Accept", accept_proposal_payload("p1", "ok")),
        (BUYER, "Accept", accept_proposal_payload("p1", "ok")),
        (
            BUYER,
            "Commitment",
            commitment_payload("c1", "proposal.accepted", "commercial", "bound", true),
        ),
    ];
    send_all(client, &sid, steps).await;
    sid
}

async fn receipt(
    client: &mut MacpRuntimeServiceClient<Channel>,
    sender: &str,
    sid: &str,
) -> Result<String, tonic::Status> {
    client
        .get_decision_receipt(with_sender(
            sender,
            GetDecisionReceiptRequest {
                session_id: sid.into(),
            },
        ))
        .await
        .map(|r| r.into_inner().receipt_json)
}

async fn keys(client: &mut MacpRuntimeServiceClient<Channel>) -> serde_json::Value {
    let jwks = client
        .get_receipt_keys(GetReceiptKeysRequest {})
        .await
        .unwrap()
        .into_inner()
        .jwks_json;
    serde_json::from_str(&jwks).unwrap()
}

async fn signed_capability(client: &mut MacpRuntimeServiceClient<Channel>) -> bool {
    let caps = initialize(client).await.unwrap().capabilities.unwrap();
    let cap = caps.decision_receipt.expect("decision_receipt advertised");
    assert!(cap.get_decision_receipt);
    cap.signed
}

fn error_code(status: &tonic::Status) -> &str {
    status
        .metadata()
        .get("macp-error-code")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
}

/// The `kid` in a receipt's first attestation header.
fn signing_kid(receipt: &str) -> String {
    let receipt: serde_json::Value = serde_json::from_str(receipt).unwrap();
    let jws = receipt["attestations"][0]["jws"].as_str().unwrap();
    let header = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(jws.split('.').next().unwrap())
        .unwrap();
    let header: serde_json::Value = serde_json::from_slice(&header).unwrap();
    header["kid"].as_str().unwrap().to_string()
}

/// `macp verify <receipt> <args>`: (exit code, stdout, stderr).
fn verify(dir: &std::path::Path, receipt: &str, args: &[&str]) -> (i32, String, String) {
    let path = dir.join(format!("receipt-{}.json", uuid::Uuid::new_v4()));
    std::fs::write(&path, receipt).unwrap();
    let out = std::process::Command::new(cli_binary())
        .arg("verify")
        .arg(&path)
        .args(args)
        .env_remove("MACP_TOKEN")
        .output()
        .expect("macp must run");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn pem_env() -> [(&'static str, String); 2] {
    [
        (
            "MACP_RECEIPT_SIGNING_KEY_PATH",
            format!("{FIXTURES}/test-receipt-key.pem"),
        ),
        ("MACP_RECEIPT_SIGNING_KID", "key-a".to_string()),
    ]
}

#[tokio::test]
async fn signed_receipts_attest_and_are_byte_stable() {
    let data = tempfile::tempdir().unwrap();
    let data_dir = data.path().to_string_lossy().to_string();
    let pem = pem_env();
    let env = [
        (pem[0].0, pem[0].1.as_str()),
        (pem[1].0, pem[1].1.as_str()),
        ("MACP_RUNTIME_ID", "it-signed"),
    ];
    let mut rt = start(&data_dir, &env).await;
    assert!(signed_capability(&mut rt.client).await);

    let sid = resolved_session(&mut rt.client).await;
    let first = receipt(&mut rt.client, BUYER, &sid).await.unwrap();
    assert_eq!(receipt(&mut rt.client, SELLER, &sid).await.unwrap(), first);
    assert_eq!(receipt(&mut rt.client, AUDIT, &sid).await.unwrap(), first);
    let body: serde_json::Value = serde_json::from_str(&first).unwrap();
    assert_eq!(body["issuer"], "runtime:it-signed");
    assert_eq!(signing_kid(&first), "key-a");

    let endpoint = rt.manager.endpoint.clone();
    let (code, stdout, stderr) = verify(
        data.path(),
        &first,
        &["--keys-from", &endpoint, "--token", AUDIT],
    );
    assert_eq!(code, 0, "stdout: {stdout} stderr: {stderr}");
    assert!(stdout.contains("verdict       attested"), "{stdout}");

    // Refusals.
    let err = receipt(&mut rt.client, STRANGER, &sid).await.unwrap_err();
    assert_eq!(
        (err.code(), error_code(&err)),
        (tonic::Code::PermissionDenied, "FORBIDDEN")
    );
    let err = receipt(&mut rt.client, BUYER, &new_session_id())
        .await
        .unwrap_err();
    assert_eq!(
        (err.code(), error_code(&err)),
        (tonic::Code::NotFound, "SESSION_NOT_FOUND")
    );
    // A path-like id never reaches storage.
    let alias = format!("{data_dir}/sessions/{sid}");
    for id in [alias.as_str(), &format!("../sessions/{sid}")] {
        let err = receipt(&mut rt.client, BUYER, id).await.unwrap_err();
        assert_eq!(error_code(&err), "SESSION_NOT_FOUND", "{id}");
    }
    let open = open_session(&mut rt.client).await;
    let err = receipt(&mut rt.client, BUYER, &open).await.unwrap_err();
    assert_eq!(
        (err.code(), error_code(&err)),
        (tonic::Code::FailedPrecondition, "RECEIPT_UNAVAILABLE")
    );

    // Concurrent first fetches agree.
    let concurrent = resolved_session(&mut rt.client).await;
    let mut calls = tokio::task::JoinSet::new();
    for _ in 0..8 {
        let (mut client, sid) = (rt.client.clone(), concurrent.clone());
        calls.spawn(async move { receipt(&mut client, BUYER, &sid).await.unwrap() });
    }
    let fetched = calls.join_all().await;
    assert!(fetched.windows(2).all(|w| w[0] == w[1]));

    // A restart on the same data dir serves the same bytes.
    rt.manager.stop();
    let mut rt = start(&data_dir, &env).await;
    assert_eq!(receipt(&mut rt.client, BUYER, &sid).await.unwrap(), first);
    assert_eq!(
        receipt(&mut rt.client, BUYER, &concurrent).await.unwrap(),
        fetched[0]
    );
}

#[tokio::test]
async fn unsigned_receipts_are_integrity_only() {
    let data = tempfile::tempdir().unwrap();
    let data_dir = data.path().to_string_lossy().to_string();
    let mut rt = start(&data_dir, &[("MACP_MESSAGE_LIMIT_PER_MINUTE", "5")]).await;
    assert!(!signed_capability(&mut rt.client).await);
    assert_eq!(
        keys(&mut rt.client).await,
        serde_json::json!({ "keys": [] })
    );

    let sid = resolved_session(&mut rt.client).await;
    let unsigned = receipt(&mut rt.client, AUDIT, &sid).await.unwrap();
    let body: serde_json::Value = serde_json::from_str(&unsigned).unwrap();
    assert_eq!(body["attestations"], serde_json::json!([]));
    assert_eq!(body["issuer"], "runtime:macp-runtime");

    let (code, stdout, _) = verify(data.path(), &unsigned, &["--require", "integrity"]);
    assert_eq!(code, 0, "{stdout}");
    assert!(stdout.contains("verdict       integrity_only"), "{stdout}");
    let endpoint = rt.manager.endpoint.clone();
    let (code, _, stderr) = verify(data.path(), &unsigned, &["--keys-from", &endpoint]);
    assert_eq!(code, 2, "{stderr}");
    assert!(stderr.contains("no Ed25519 key"), "{stderr}");

    // The read path is rate limited like WhatIfReplay.
    let mut limited = None;
    for _ in 0..10 {
        if let Err(e) = receipt(&mut rt.client, AUDIT, &sid).await {
            limited = Some(e);
            break;
        }
    }
    let err = limited.expect("the reader must be rate limited");
    assert_eq!(err.code(), tonic::Code::ResourceExhausted, "{err:?}");
}

#[tokio::test]
async fn rotated_keys_keep_old_receipts_verifiable() {
    let data = tempfile::tempdir().unwrap();
    let data_dir = data.path().to_string_lossy().to_string();
    let pem = pem_env();

    // Key A issues.
    let mut rt = start(
        &data_dir,
        &[
            (pem[0].0, pem[0].1.as_str()),
            (pem[1].0, pem[1].1.as_str()),
            ("MACP_RUNTIME_ID", "it-rotated"),
        ],
    )
    .await;
    let old_sid = resolved_session(&mut rt.client).await;
    let old = receipt(&mut rt.client, BUYER, &old_sid).await.unwrap();
    let key_a = keys(&mut rt.client).await;
    rt.manager.stop();
    let key_a_path = data.path().join("key-a.json");
    std::fs::write(&key_a_path, key_a.to_string()).unwrap();

    // Key B (a private JWK, inline), with A's public key published.
    let seed = [9u8; 32];
    let mut key_b = macp_runtime::macp_core::receipt::ReceiptSigner::from_seed(&seed, "key-b")
        .unwrap()
        .public_jwk();
    key_b["d"] = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(seed)
        .into();
    let key_b = key_b.to_string();
    let key_a_path = key_a_path.to_string_lossy().to_string();
    let mut rt = start(
        &data_dir,
        &[
            ("MACP_RECEIPT_SIGNING_KEY", key_b.as_str()),
            ("MACP_RECEIPT_PUBLIC_KEYS_PATH", key_a_path.as_str()),
            ("MACP_RUNTIME_ID", "it-rotated"),
        ],
    )
    .await;
    let published = keys(&mut rt.client).await;
    let kids: Vec<_> = published["keys"]
        .as_array()
        .unwrap()
        .iter()
        .map(|k| k["kid"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(kids, ["key-b", "key-a"]);
    assert!(!published.to_string().contains("\"d\""));

    assert_eq!(receipt(&mut rt.client, BUYER, &old_sid).await.unwrap(), old);
    let endpoint = rt.manager.endpoint.clone();
    let (code, stdout, stderr) = verify(data.path(), &old, &["--keys-from", &endpoint]);
    assert_eq!(code, 0, "stdout: {stdout} stderr: {stderr}");
    assert!(stdout.contains("verdict       attested"), "{stdout}");

    let new_sid = resolved_session(&mut rt.client).await;
    let new = receipt(&mut rt.client, BUYER, &new_sid).await.unwrap();
    assert_eq!(signing_kid(&new), "key-b");
    rt.manager.stop();

    // Key B alone: --keys-from no longer trusts A; --key merges it back in.
    let rt = start(
        &data_dir,
        &[
            ("MACP_RECEIPT_SIGNING_KEY", key_b.as_str()),
            ("MACP_RUNTIME_ID", "it-rotated"),
        ],
    )
    .await;
    let endpoint = rt.manager.endpoint.clone();
    let (code, stdout, _) = verify(data.path(), &old, &["--keys-from", &endpoint]);
    assert_eq!(code, 1, "{stdout}");
    let (code, stdout, stderr) = verify(
        data.path(),
        &old,
        &["--keys-from", &endpoint, "--key", &key_a_path],
    );
    assert_eq!(code, 0, "stdout: {stdout} stderr: {stderr}");
}

#[test]
fn keys_from_an_unreachable_runtime_is_a_usage_error() {
    let dir = tempfile::tempdir().unwrap();
    let port = macp_integration_tests::server_manager::find_free_port().unwrap();
    let (code, _, stderr) = verify(
        dir.path(),
        "{\"receipt_format\":\"macp-receipt/1\"}",
        &["--keys-from", &format!("http://127.0.0.1:{port}")],
    );
    assert_eq!(code, 2, "{stderr}");
    assert!(stderr.contains("--keys-from"), "{stderr}");
}

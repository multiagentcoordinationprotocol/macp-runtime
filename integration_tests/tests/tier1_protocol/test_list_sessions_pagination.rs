//! Server-side pagination of `ListSessions` through the real gRPC boundary.
//!
//! Every test spawns its **own** runtime process via
//! [`ServerManager::start_with_env`]. This is not a stylistic choice: the
//! shared server in `tests/common/mod.rs` accumulates sessions from every
//! other test in this binary, so any page-count or exact-traversal assertion
//! against it would be inherently flaky. `start_with_env` forces
//! `MACP_MEMORY_ONLY=1` and a free `MACP_BIND_ADDR`, giving each test a clean,
//! empty session registry.
//!
//! These tests are also the **only** end-to-end proof that
//! `MACP_LIST_SESSIONS_DEFAULT_PAGE_SIZE` and `MACP_LIST_SESSIONS_MAX_PAGE_SIZE`
//! are wired to the fields they name. The unit tests below the handler drive a
//! name-agnostic pure resolver, and the one `from_env` test runs with both vars
//! unset, so a transposition of the two variable names at the `from_env` call
//! site is invisible to them. See the note on
//! [`page_size_above_max_is_clamped`].

use std::collections::BTreeSet;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use macp_integration_tests::helpers::*;
use macp_integration_tests::server_manager::ServerManager;
use macp_runtime::pb::macp_runtime_service_client::MacpRuntimeServiceClient;
use tonic::transport::Channel;
use tonic::Code;

fn test_binary() -> String {
    std::env::var("MACP_TEST_BINARY").unwrap_or_else(|_| "../target/debug/macp-runtime".into())
}

/// Pin the session-start rate limit far above every fixture here. The default
/// is 60 per sender per minute, which today's largest fixture (25 sessions from
/// one sender) clears — but a lowered default or a grown fixture would turn
/// into a rate-limit failure dressed up as a pagination bug.
const RATE_LIMIT_HEADROOM: (&str, &str) = ("MACP_SESSION_START_LIMIT_PER_MINUTE", "1000");

/// Start a dedicated runtime with the page-size limits under test, and return a
/// connected client alongside the manager (which must stay alive for the test).
async fn start(extra_env: &[(&str, &str)]) -> (ServerManager, MacpRuntimeServiceClient<Channel>) {
    let mut env = vec![RATE_LIMIT_HEADROOM];
    env.extend_from_slice(extra_env);
    let manager = ServerManager::start_with_env(&test_binary(), &env)
        .await
        .expect("server must start");
    let client = MacpRuntimeServiceClient::connect(manager.endpoint.clone())
        .await
        .expect("connect");
    (manager, client)
}

/// Open `n` decision sessions as `sender` and return their IDs in creation
/// order. The IDs are real UUID v4s, so their sort order is unrelated to
/// creation order — which is what makes the ordering assertions meaningful.
async fn create_sessions(
    client: &mut MacpRuntimeServiceClient<Channel>,
    sender: &str,
    n: usize,
) -> Vec<String> {
    let partner = "agent://partner";
    let mut ids = Vec::with_capacity(n);
    for i in 0..n {
        let sid = new_session_id();
        let ack = send_as(
            client,
            sender,
            envelope(
                MODE_DECISION,
                "SessionStart",
                &new_message_id(),
                &sid,
                sender,
                session_start_payload(
                    &format!("pagination fixture {i}"),
                    &[sender, partner],
                    600_000,
                ),
            ),
        )
        .await
        .expect("SessionStart transport ok");
        assert!(ack.ok, "SessionStart {i} rejected: {:?}", ack.error);
        ids.push(sid);
    }
    ids
}

fn page_ids(resp: &macp_runtime::pb::ListSessionsResponse) -> Vec<String> {
    resp.sessions.iter().map(|s| s.session_id.clone()).collect()
}

// ── page-size resolution ────────────────────────────────────────────────

/// `page_size = 0` means "server default", and the default is the one
/// `MACP_LIST_SESSIONS_DEFAULT_PAGE_SIZE` names. 7 is distinctive: it is
/// neither the built-in default (100) nor the max configured here (900) nor
/// any page size requested elsewhere in this file.
#[tokio::test]
async fn default_page_size_applied_when_page_size_is_zero() {
    let (_manager, mut client) = start(&[
        ("MACP_LIST_SESSIONS_DEFAULT_PAGE_SIZE", "7"),
        ("MACP_LIST_SESSIONS_MAX_PAGE_SIZE", "900"),
    ])
    .await;
    create_sessions(&mut client, "agent://pager", 10).await;

    let resp = list_sessions_as(&mut client, "agent://observer", 0, "")
        .await
        .expect("list ok");
    assert_eq!(
        resp.sessions.len(),
        7,
        "page_size=0 must yield MACP_LIST_SESSIONS_DEFAULT_PAGE_SIZE rows"
    );
    assert!(
        !resp.next_page_token.is_empty(),
        "3 of 10 sessions remain, so the traversal is not complete"
    );
}

/// An explicit `page_size` below the max wins over the configured default, so
/// the response size is 4 rather than the 7 the server would have chosen.
#[tokio::test]
async fn explicit_page_size_is_honored() {
    let (_manager, mut client) = start(&[
        ("MACP_LIST_SESSIONS_DEFAULT_PAGE_SIZE", "7"),
        ("MACP_LIST_SESSIONS_MAX_PAGE_SIZE", "900"),
    ])
    .await;
    create_sessions(&mut client, "agent://pager", 10).await;

    let resp = list_sessions_as(&mut client, "agent://observer", 4, "")
        .await
        .expect("list ok");
    assert_eq!(resp.sessions.len(), 4);
    assert!(!resp.next_page_token.is_empty());
}

/// A `page_size` above the configured maximum is clamped down to it, not
/// rejected.
///
/// **This test is what pins the env-var→field binding.** The two values are
/// deliberately distinctive *and different* (default 2, max 3). Resolution
/// clamps the default down to the max, so `default = min(D, M)` under both the
/// correct wiring and a transposition of the two variable names — meaning the
/// `page_size = 0` path alone can never detect the swap. The *max* can:
/// correctly wired the effective max is 3, transposed it is 2, so requesting an
/// over-large page returns 3 rows in the first case and 2 in the second.
#[tokio::test]
async fn page_size_above_max_is_clamped() {
    let (_manager, mut client) = start(&[
        ("MACP_LIST_SESSIONS_DEFAULT_PAGE_SIZE", "2"),
        ("MACP_LIST_SESSIONS_MAX_PAGE_SIZE", "3"),
    ])
    .await;
    create_sessions(&mut client, "agent://pager", 10).await;

    let resp = list_sessions_as(&mut client, "agent://observer", 1000, "")
        .await
        .expect("list ok");
    assert_eq!(
        resp.sessions.len(),
        3,
        "page_size=1000 must clamp to MACP_LIST_SESSIONS_MAX_PAGE_SIZE"
    );
    assert!(
        !resp.next_page_token.is_empty(),
        "7 of 10 sessions remain, so the traversal is not complete"
    );

    // The default is a separate, smaller value on the same server: this pins
    // both variables against one another rather than one in isolation.
    let resp = list_sessions_as(&mut client, "agent://observer", 0, "")
        .await
        .expect("list ok");
    assert_eq!(
        resp.sessions.len(),
        2,
        "page_size=0 must yield MACP_LIST_SESSIONS_DEFAULT_PAGE_SIZE rows"
    );
}

// ── traversal ───────────────────────────────────────────────────────────

/// Paging from the first token to the empty one visits every session exactly
/// once. Both assertions are load bearing: the set alone would hide duplicates,
/// the count alone would hide drops.
#[tokio::test]
async fn full_traversal_yields_every_session_exactly_once() {
    const TOTAL: usize = 25;
    const PAGE: i32 = 4;

    let (_manager, mut client) = start(&[]).await;
    let created = create_sessions(&mut client, "agent://pager", TOTAL).await;

    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut total_collected = 0usize;
    let mut ordered: Vec<String> = Vec::new();
    let mut token = String::new();
    let mut pages = 0usize;

    loop {
        let resp = list_sessions_as(&mut client, "agent://observer", PAGE, &token)
            .await
            .expect("list ok");
        pages += 1;
        assert!(
            pages <= TOTAL + 2,
            "traversal did not terminate; the cursor is probably stalled"
        );
        assert!(
            resp.sessions.len() <= PAGE as usize,
            "a page must never exceed the requested size"
        );
        for id in page_ids(&resp) {
            total_collected += 1;
            seen.insert(id.clone());
            ordered.push(id);
        }
        token = resp.next_page_token;
        if token.is_empty() {
            break;
        }
    }

    assert_eq!(seen.len(), TOTAL, "every session must be visited");
    assert_eq!(
        total_collected, TOTAL,
        "no session may be emitted twice across pages"
    );
    assert_eq!(
        seen,
        created.into_iter().collect::<BTreeSet<_>>(),
        "the traversal must yield exactly the sessions that were created"
    );
    let mut sorted = ordered.clone();
    sorted.sort();
    assert_eq!(
        ordered, sorted,
        "sessions must be emitted in ascending session_id order across pages"
    );
}

/// The last page carries an empty `next_page_token`, both when the page is
/// short and when the remaining sessions exactly fill it. The exact-fit case is
/// the one a naive "short page means done" implementation gets wrong.
#[tokio::test]
async fn terminal_page_returns_empty_next_page_token() {
    let (_manager, mut client) = start(&[]).await;
    create_sessions(&mut client, "agent://pager", 5).await;

    // Short final page: 5 sessions, room for 10.
    let resp = list_sessions_as(&mut client, "agent://observer", 10, "")
        .await
        .expect("list ok");
    assert_eq!(resp.sessions.len(), 5);
    assert!(
        resp.next_page_token.is_empty(),
        "a complete result must carry an empty next_page_token"
    );

    // Exact fit: 5 sessions, page size 5.
    let resp = list_sessions_as(&mut client, "agent://observer", 5, "")
        .await
        .expect("list ok");
    assert_eq!(resp.sessions.len(), 5);
    assert!(
        resp.next_page_token.is_empty(),
        "an exactly-filled final page must still terminate the traversal"
    );
}

/// A page token is a position, not a handle: replaying the same token returns
/// the identical page. The Tier-1 companion to the handler's unit coverage.
#[tokio::test]
async fn replayed_page_token_returns_the_identical_page() {
    let (_manager, mut client) = start(&[]).await;
    create_sessions(&mut client, "agent://pager", 12).await;

    let first = list_sessions_as(&mut client, "agent://observer", 4, "")
        .await
        .expect("list ok");
    assert_eq!(first.sessions.len(), 4);
    let first_ids = page_ids(&first);
    let token = first.next_page_token;
    assert!(!token.is_empty());

    let second = list_sessions_as(&mut client, "agent://observer", 4, &token)
        .await
        .expect("list ok");
    let replayed = list_sessions_as(&mut client, "agent://observer", 4, &token)
        .await
        .expect("list ok");

    assert_eq!(
        page_ids(&second),
        page_ids(&replayed),
        "replaying a page token must return the identical page"
    );
    assert_eq!(
        second.next_page_token, replayed.next_page_token,
        "replaying a page token must return the identical continuation token"
    );
    // Sanity: the second page is genuinely past the first.
    assert!(
        page_ids(&second).iter().all(|id| !first_ids.contains(id)),
        "the second page must not repeat the first"
    );
}

// ── rejection paths ─────────────────────────────────────────────────────

/// Every malformed continuation token is `INVALID_ARGUMENT`.
///
/// Note two deliberate omissions. A ~2 MiB token is *not* sent: the server sets
/// `max_decoding_message_size(max_payload_bytes + 64 KiB)` ≈ 1.06 MiB, so a
/// request that large is rejected by tonic at decode with a different code and
/// never reaches the handler's own length check. 64 KiB is far above the
/// handler's 1024-char token limit and far below the decode limit, so it proves
/// the branch under test. And the damaged token is truncated at the **front**,
/// not the end: chopping characters off the end of an encoded token often
/// yields a well-formed *shorter* cursor, which the handler correctly accepts.
#[tokio::test]
async fn garbage_page_token_returns_invalid_argument() {
    let (_manager, mut client) = start(&[]).await;
    create_sessions(&mut client, "agent://pager", 3).await;

    // A structurally valid token, used as the base for the damaged variants.
    let valid = URL_SAFE_NO_PAD.encode(format!("v1:{}", new_session_id()));
    // Guard the premise: if this stops being accepted, the negative cases below
    // would pass for the wrong reason.
    let resp = list_sessions_as(&mut client, "agent://observer", 2, &valid)
        .await
        .expect("a well-formed token must be accepted");
    assert!(resp.sessions.len() <= 2);

    let front_truncated = valid[1..].to_string();
    let cases: Vec<(&str, String)> = vec![
        ("not base64url at all", "not-a-token".to_string()),
        ("unknown token version", URL_SAFE_NO_PAD.encode("v2:abc")),
        (
            "version prefix only, no cursor",
            URL_SAFE_NO_PAD.encode("v1"),
        ),
        ("front-truncated valid token", front_truncated),
        // ~64 KiB: over the handler's token-length cap, under the gRPC
        // decode cap.
        ("oversized token", "A".repeat(64 * 1024)),
    ];

    for (label, token) in cases {
        let err = list_sessions_as(&mut client, "agent://observer", 2, &token)
            .await
            .expect_err(&format!("{label}: must be rejected"));
        assert_eq!(
            err.code(),
            Code::InvalidArgument,
            "{label}: expected INVALID_ARGUMENT, got {err:?}"
        );
    }
}

/// A negative `page_size` is a client bug, not a request for the default.
#[tokio::test]
async fn negative_page_size_is_rejected() {
    let (_manager, mut client) = start(&[]).await;
    create_sessions(&mut client, "agent://pager", 3).await;

    for page_size in [-1, i32::MIN] {
        let err = list_sessions_as(&mut client, "agent://observer", page_size, "")
            .await
            .expect_err("negative page_size must be rejected");
        assert_eq!(
            err.code(),
            Code::InvalidArgument,
            "page_size={page_size}: expected INVALID_ARGUMENT, got {err:?}"
        );
    }
}

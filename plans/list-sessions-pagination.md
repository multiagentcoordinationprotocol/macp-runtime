# PLAN — server-side pagination for `ListSessions` (macp-runtime)

**Verified against:** `d500910` on `main`, clean tree, 2026-08-30. Every file:line below was read, not recalled.
**Spec checkout verified against:** `/Users/ajitkoti/code/multiagentcoordinationprotocol/multiagentcoordinationprotocol` at `65f6805` (2026-08-30), equal to the local `origin/main` ref. Not re-fetched (read-only in that repo).
**Do not touch PR #114** (release-plz v0.7.0 release PR). This lands first.

## Context

### Current behavior

`src/server.rs:1261-1279` authenticates, calls `self.runtime.registry.get_all_sessions().await`, maps every session through `Self::session_to_metadata` (`:162-191`), and returns a hardcoded `next_page_token: String::new()`. `ListSessionsRequest.page_size` and `.page_token` are never read. The in-code comment at `:1273-1274` states the intent plainly: *"Unpaginated: always returns every session in one response."* A client sending `page_size=10` against 40,000 sessions receives 40,000 entries in one message.

The fields exist: `macp-proto` **0.1.8** is pinned in both `Cargo.lock:1212-1215` and `integration_tests/Cargo.lock:1195-1198`. (Root `Cargo.toml:65` still reads `macp-proto = "0.1.5"` as a caret requirement; the lock resolves it to 0.1.8. No manifest change needed.)

### Target behavior

`ListSessions` returns a bounded page of `SessionMetadata`, ordered by `session_id` ascending (byte-wise), continued by an opaque keyset cursor in `next_page_token`. `page_size=0` yields a configurable server default; `page_size` above a configurable hard maximum is clamped; `page_size < 0` is `INVALID_ARGUMENT`; a malformed, truncated, oversized, or wrong-version `page_token` is `INVALID_ARGUMENT` and never panics or allocates unboundedly. Authentication remains the first statement in the handler.

**This is a deliberate, spec-sanctioned behavior change.** A client that ignores `next_page_token` will now see a short list where it previously saw everything. It must be stated in the conventional-commit body (release-plz builds `CHANGELOG.md` from it) and in the PR body, in those words — not as a tidy-up.

### Normative sources, in binding order

1. **`macp-proto` 0.1.8, `proto/macp/v1/core.proto:411-426`** — the only text with normative pagination semantics. `page_size`: *"0 = server-chosen default. Servers MAY cap the effective size; clients MUST NOT assume the response is complete unless next_page_token is empty."* `page_token`: *"Opaque continuation token... Empty = first page. Tokens are short-lived and implementation-defined; a stale token yields INVALID_ARGUMENT."* `next_page_token`: *"Non-empty when more results exist; pass back verbatim as page_token."*
2. `docs/lifecycle.md:75` and `docs/transports.md:47` in the spec repo — consistent one-line summaries.

### Divergence from the brief: RFC-MACP-0006 §3.8 adds nothing

The coordinator asked that RFC-MACP-0006 §3.8 be treated as binding above the two `docs/` lines. **It does not hold against the code.** §3.8 was read in full at `rfcs/RFC-MACP-0006-transport-bindings.md:146-160`. It contains **no pagination language whatsoever**. Verbatim: *"`ListSessions` returns `SessionMetadata` for all currently known sessions (active and terminal). A runtime MUST advertise `sessions.list_sessions = true` before `ListSessions` can be assumed interoperable."* No `page_size`, no `page_token`, no ordering, no bounds, no `INVALID_ARGUMENT`. `RFC-MACP-0001-core.md:371` is likewise pagination-free and additionally *narrower* than §3.8 ("all currently active sessions" vs. §3.8's "all currently known sessions (active and terminal)").

Spec issue #38 / PR #51 shipped the proto fields and the two `docs/` summary lines but never updated the RFC prose. Consequences:

- **No decision in this plan changes.** The original brief's reading was correct; the proto comments govern.
- **§3.8 is now contradicted by a conforming implementation.** That is a spec-repo defect, not a runtime one. Closeout action: **open an upstream issue** asking that §3.8 and RFC-0001:371 be updated to describe the paged contract. Do not edit the spec repo (read-only).
- `docs/API.md:98-106` in this repo currently paraphrases §3.8's wording. After this change it must describe the paged contract even though the RFC does not yet — the doc phase states that API.md follows the proto and cites the upstream issue.

### Correctness risks identified, with evidence

**R1 — `get_all_sessions()` order is nondeterministic; naive paging both drops and duplicates.**
`SessionRegistry.sessions` is `RwLock<HashMap<String, SharedSession>>` (`crates/macp-storage/src/registry.rs:150-153`). `get_all_sessions()` (`:241-251`) snapshots `guard.values().cloned()` then locks and clones each session. `std::collections::HashMap` uses `RandomState`: iteration order varies per process (random seed) *and* reorders on rehash as the map grows. It is not "unspecified but stable" — inserting one session between two pages can move unrelated entries. There is no existing deterministic-order method on the registry.

**R2 — an offset cursor is wrong for this data structure.** Under insertion before the cursor, every later element shifts right and the boundary element is re-emitted (duplicate). Under deletion before the cursor, elements shift left and one is skipped (drop). Session IDs are UUID v4/v7 or base64url tokens (`crates/macp-core/src/session.rs`, `validate_session_id_for_acceptance`, per `CLAUDE.md` §6), i.e. effectively random, so a newly created session lands at a uniformly random position in sorted order — probability ≈ offset/n of landing before the cursor. At any meaningful offset a duplicate or drop is near-certain per page, under ordinary traffic.

**R3 — the terminal-page off-by-one.** Returning a non-empty `next_page_token` when the last page happens to contain exactly `page_size` items and nothing remains forces one extra empty round trip and, worse, makes "empty token ⇒ complete" a lie for one page. The proto makes the empty token the *only* completeness signal (`core.proto:411-414`), so this must be exact.

**R4 — deriving the next cursor from the returned sessions rather than from the ID list is a silent-drop bug.** The registry is mutated between the ID scan and the per-ID fetch. If the last candidate ID's session was removed in that window, deriving the cursor from `sessions.last()` either stalls the cursor (infinite loop) or, if the whole page vanished, produces no cursor at all — terminating the traversal early and dropping every remaining session. The cursor must come from the ID list.

**R5 — `page_token` is authenticated-but-untrusted input on a public RPC.** Length, base64 validity, UTF-8 validity, and version prefix must all be checked, in that order, with the length check *before* the decode so the decode allocation is bounded. No `unsafe`, no `from_utf8_unchecked`, no unwrap.

**R6 — `page_size` is `int32`, so `page_size < 0` is representable.** `page_size as usize` on a negative `i32` produces a colossal value on 64-bit. Must be rejected before any cast.

**R7 — a `0` or inverted configuration livelocks clients.** `max_page_size = 0` (or `default = 0`) makes every page empty while `next_page_token` stays non-empty forever. `default > max` silently discards operator intent.

**R8 — auth ordering.** The handler currently authenticates before touching the request body (`src/server.rs:1265-1269`). Introducing `page_size`/`page_token` validation creates an obvious temptation to validate the cheap thing first. It must not happen: an unauthenticated caller must get `UNAUTHENTICATED`, never `INVALID_ARGUMENT` (which would confirm the endpoint is live and leak validation behavior pre-auth).

**R9 — cloning every session on every page.** Confirmed a real scaling problem and **fixed in this change, not deferred.** `get_all_sessions()` does n `Arc` clones + n mutex `.await`s + n deep `Session` clones per call; a `Session` carries `participants: Vec<String>`, `seen_message_ids: HashSet<String>`, `mode_state: Vec<u8>`, `extensions: HashMap<String, Vec<u8>>`, `policy_definition` (field set at `registry.rs:59-95`). At 100k sessions a full traversal at 100/page would be 1000 pages × 100k deep clones ≈ 10^8 session clones plus 10^8 per-session mutex acquisitions. The fix is not gold-plating — it *is* what a paging primitive naturally does: clone only the ≤ page_size sessions actually returned. What **is** deferred (see Follow-ups) is making the *scan* sub-linear.

**R10 — the map-lock contract.** `registry.rs:141-147` documents: map lock BEFORE session mutex, and never hold the map lock while awaiting a session mutex — snapshot, drop the guard, then lock. Any new registry method must obey it. `count_open_sessions_for_initiator` (`:261-284`) shows the alternative (`try_lock`), which is *not* appropriate here: paging must return the real session, not a conservative guess.

**R11 — cargo-semver-checks (`release-plz.toml:16`, `semver_check = true`).** An API break blocks the release PR. Net public-API delta of this plan, all additive: two `pub` fields added to `macp_auth::security::SecurityLayer`, and one new inherent `pub async fn` on `macp_storage::registry::SessionRegistry`.

**The two `SecurityLayer` fields are re-exported from this crate and therefore *are* public API here.** `src/lib.rs:37` is `pub use macp_auth::{auth, security};`, so they surface as `macp_runtime::security::SecurityLayer::{list_sessions_default_page_size, list_sessions_max_page_size}`. The semver conclusion is unchanged: `SecurityLayer` has 5 of its 6 fields private (`security.rs:66-71` — only `max_payload_bytes` at `:69` is `pub`), so it is not externally struct-literal-constructible, and it carries no `#[non_exhaustive]` (`:64` is just `#[derive(Clone)]`). Adding fields to such a struct is additive. What *is* **zero new public API** is the token codec: `src/pagination.rs` is crate-private (`mod pagination;`, not `pub mod`).

---

## Reverify (2026-08-30)

A fresh Opus pass audited every citation in this plan against `d500910`, attacked the paging algorithm and its concurrency semantics, and returned **FLAWED**: four blocking defects and six coverage gaps.

**Blocking defects — all patched below:**

1. **Dead-code gate.** The original Phase 2 shipped `src/pagination.rs` with "nothing calls it yet". Its functions would then be reachable only from `#[cfg(test)]`, i.e. `dead_code` in a plain `--lib` build. `.github/workflows/ci.yml:116` runs `cargo clippy --all-targets -- -D warnings`, and `--all-targets` still builds the non-test lib target; there is no crate-level `#![allow(dead_code)]` anywhere in this workspace (the only one is `src/bin/support/common.rs:1`, a binary support module). That phase's own "full local gate green" criterion was unachievable. **Fix:** the codec and the handler are now one phase (new Phase 3), and config now lands before the handler that reads it. Six phases.
2. **An eleventh `SecurityLayer` construction site.** The list of ten was built from a `grep "SecurityLayer {"`, which cannot see `Ok(Self { … })` at `security.rs:226` (the tail of `from_env`), and mislabelled `dev_mode`'s literal.
3. **Wrong unit-test harness citation.** `src/server.rs:1767-1773` is an ad-hoc build inside one test that reads ambient process env — the exact nondeterminism the `pub` fields exist to avoid. The canonical harness is `make_server()` at `:1648`.
4. **`clippy::map_clone`** in Phase 1's prescribed `into_sorted_vec() … .map(|s| s.clone())` — an error under `-D warnings`.

**Load-bearing claims the reverify confirmed correct** (these are what the plan rests on):

- The bounded max-heap yields the `limit` byte-wise-smallest candidates, and `BinaryHeap::into_sorted_vec()` returns ascending order.
- `Option::is_none_or` stabilized in Rust 1.82.0 — below the pinned MSRV `1.89.0` (`Cargo.toml:26`) and the pinned toolchain `1.96.1` (`rust-toolchain.toml`).
- No pagination counterexample could be constructed, including the deleted-cursor case: the cursor is minted from the ID list, so it advances regardless of the fetch outcome, strictly increases, and terminates.
- The unsigned-token argument verified on both legs — `src/server.rs:1265` binds `let _identity` and discards it (no per-caller filtering), and `docs/deployment.md:76-82` documents the all-sessions-to-any-identity shape verbatim.
- `SecurityLayer` has 5 of 6 fields private and no `#[non_exhaustive]`, so the semver conclusion holds.
- Tier-1 counts are exactly 90 + 8 JWT (`CLAUDE.md:142`).

---

## Decisions, made — not deferred to the implementer

### D1 — Sort key: `session_id`, ascending, byte-wise

`String`/`str` `Ord` is lexicographic over UTF-8 bytes. It is a **total** order here because the registry map key *is* the session_id (`registry.rs:151`) — uniqueness is structural, ties are impossible.

It is stable because `session_id` is effectively create-once. `crates/macp-core/src/session.rs:60` declares `pub session_id: String`, and a grep for `.session_id = ` across `src/` and `crates/` finds exactly **one** assignment: `src/replay.rs:55`, `session.session_id = session_id.into();`. That one is harmless — it runs on a locally-deserialized `Session` recovered from a checkpoint, *before* registry insertion, and sets the field to the very ID being recovered. Every other match is struct construction, a request-field read, or a `tracing` field binding. So from the registry's perspective `session_id` never changes after insertion.

(Structural, not enforced — see **G3** in Phase 1 for the `debug_assert` that pins the key/field agreement this rests on.)

`started_at_unix_ms` was rejected: millisecond granularity admits ties, and two sessions created in the same millisecond would page unstably.

### D2 — Keyset cursor, not offset

Keyset (`session_id > cursor`) is chosen. R2 rules out offset on correctness. There is no counter-argument from this codebase: the registry is a `HashMap` with no index, so an offset cursor would have to sort the whole key set anyway — it costs the same and is wrong. A snapshot cursor (server-side copy of the ID list per token) is rejected outright: it makes an authenticated public RPC an unbounded server-memory allocator, and server-side state is forbidden absent proof of necessity. There is none.

**Documented semantics (this paragraph goes verbatim into `docs/API.md`):**

> Traversal is a keyset scan over session IDs in ascending byte order. Every session that exists for the whole traversal is returned exactly once. A session created during a traversal is returned if and only if its ID sorts after the current cursor. A session deleted during a traversal may or may not appear, depending on whether the cursor had already passed it. A page is not a point-in-time snapshot; `next_page_token` is a position, not a snapshot handle. A page may contain fewer than `page_size` entries while `next_page_token` is still non-empty — per the proto, only an empty `next_page_token` means the result set is complete.

### D3 — Token format

Plaintext: `"v1:" + <last-emitted candidate session_id>`. Wire: `base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(plaintext)`.

Decode, in this order; **every** failure returns the same `Status::invalid_argument("INVALID_ARGUMENT: page_token is not a valid continuation token")` with no detail about which check failed:

1. `token.len() > MAX_PAGE_TOKEN_CHARS` (const `1024`) → reject **before decoding**. Bounds the decode allocation to < 768 bytes. (R5)
2. `URL_SAFE_NO_PAD.decode(token)` → `Err` → reject.
3. `String::from_utf8(bytes)` → `Err` → reject. No `unsafe`.
4. `strip_prefix("v1:")` → `None` → reject. Covers truncation, other-version tokens, and tokens minted by a future format.
5. Remainder empty → reject. An empty cursor is indistinguishable from "first page"; erroring stops a client silently restarting a traversal it believes is continuing.
6. Otherwise cursor = remainder. **No existence check** — deliberately. The cursor is a position, not a handle; requiring the session to still exist would break precisely the deleted-cursor case keyset paging exists to handle (D2).

**Never log the token bytes.** (G1) The token is attacker-chosen arbitrary bytes and reaches the handler at up to the server's configured gRPC decode limit — `src/main.rs:407` sets `max_decoding_message_size(max_payload_bytes + ENVELOPE_OVERHEAD_BYTES)` with `ENVELOPE_OVERHEAD_BYTES = 64 * 1024` (`:390`), i.e. ≈ 1.06 MiB at the default `MACP_MAX_PAYLOAD_BYTES` — because step 1's 1024-char check happens only *after* tonic has materialized the field. An embedded `\n` in a raw-token log line forges log records. Any `tracing` call on this path emits **only the `PageTokenError` discriminant**, never the token, never a prefix of it, never its length-annotated content.

**Why the format is reversible-and-that-is-fine:** the proto declares the token opaque and implementation-defined, so its internal shape is a free choice. The `v1:` prefix makes a future format change *detectable and rejectable* rather than silently misinterpreted.

**What the token deliberately does not contain:**

- **No server-side state / snapshot id** — see D2.
- **No `page_size`.** `page_size` is a per-request field (`core.proto:411-415`). Baking it in would silently override the client's next request.
- **No issued-at timestamp or TTL.** The proto's "short-lived and implementation-defined; a stale token yields INVALID_ARGUMENT" (`core.proto:416-419`) constrains what a client may *assume*, not what a server must implement. A keyset cursor has no staleness — it stays meaningful indefinitely. A TTL would invent a real failure mode (a client paging 100k sessions over a slow link starts failing mid-traversal) for zero correctness gain, and would add a third config knob. The `INVALID_ARGUMENT`-on-bad-token path the proto requires is fully exercised by cases 1-5 above.
- **No MAC/signature.** The threat would be a caller forging a cursor to reach data it should not see. There is nothing to protect: `list_sessions` performs no per-caller filtering (`src/server.rs:1261-1279`; `:1265` binds `let _identity` and discards it), and `docs/deployment.md:76-82` documents this as intentional — *"`ListSessions` and `WatchSessions` return metadata for **all** sessions to any authenticated identity (RFC-0006 permits this shape)."* A forged cursor yields a strict subset of what the caller can already obtain by paging from the start; it cannot be used to probe. Signing would add key management for no gain. **A code comment must record that this conclusion is contingent on ListSessions remaining unfiltered — if per-caller filtering is ever added, the cursor becomes attacker-controllable positioning into a filtered set and must be re-analyzed.**

### D4 — Error paths

- Negative `page_size` and bad `page_token` → **`Status::invalid_argument` constructed directly**, not `Self::status_from_error`. `status_from_error` (`src/server.rs:757-786`) takes a `MacpError` and maps the kernel's domain errors; these are request-shape errors with no `MacpError` variant, and the surrounding handlers already use direct construction for exactly this class — `src/server.rs:797-799` (the message literal `"INVALID_REQUEST: supported_protocol_versions must not be empty"` is at `:798`; `:797` is the `return Err(Status::invalid_argument(` line), `:860`, `:1385`, `:1477`.
- **There is no message-prefix convention in this file — do not invent one from a single sample.** `:797-799` is the *only* prefixed message; `:860` (`"SendRequest must contain an envelope"`), `:1385` and `:1477` (both `"descriptor required"`) are bare. The prefix is a local habit, not a rule.
- **Both new errors in this handler use the same prefix: `INVALID_ARGUMENT:`.** Fixed here so the executor does not choose, and so one handler does not emit two prefixes for the same error class:
  - negative page size → `"INVALID_ARGUMENT: page_size must not be negative"`
  - undecodable token → `"INVALID_ARGUMENT: page_token is not a valid continuation token"`

  `INVALID_ARGUMENT` (rather than `INVALID_REQUEST`) because it is the proto's own word for both failures (`core.proto:411-426`) and the gRPC code actually returned.
- Authentication keeps using `.map_err(Self::status_from_error)` exactly as at `:1265-1269`. Not touched.

### D5 — Config: two env vars on `SecurityLayer`

| Variable | Default | Meaning |
|---|---|---|
| `MACP_LIST_SESSIONS_DEFAULT_PAGE_SIZE` | `100` | Effective page size when the client sends `page_size = 0` |
| `MACP_LIST_SESSIONS_MAX_PAGE_SIZE` | `1000` | Hard cap; a larger `page_size` is clamped down to it |

**Home: `crates/macp-auth/src/security.rs`, two `pub usize` fields on `SecurityLayer` beside `pub max_payload_bytes` (`:69`).** Rationale: `max_payload_bytes` sets the precedent that per-request wire resource caps live on `SecurityLayer` and are read straight from the handler (`src/server.rs:146`, `self.security.max_payload_bytes`). Same shape here: `self.security.list_sessions_default_page_size`. `pub` (matching `max_payload_bytes`) makes them injectable from `src/server.rs` unit tests without env mutation — `let mut security = SecurityLayer::dev_mode(); security.list_sessions_max_page_size = 3;` — which is deterministic under `cargo test`'s thread pool in a way `std::env::set_var` is not. (Phase 3 adds the harness hook that makes this injection possible; see B3 there.)

**Three mandatory touch points:**

1. **`SecurityLayer::from_env()` — the function spans `security.rs:118-234`.** The *idiom to copy verbatim* is the `max_payload_bytes` parse block at **`:119-122`**: `std::env::var(NAME).ok().and_then(|v| v.parse::<usize>().ok()).unwrap_or(DEFAULT)`. Add the two parses there, then `.max(1)` on each, then `default = default.min(max)` with a `tracing::warn!` when the clamp fires, then add both fields to the `Ok(Self { … })` at `:226-233`. This layer stays silent-on-garbage, matching the existing three vars.
2. `SecurityLayer::dev_mode()` (`security.rs:78-93`) — set the **production** defaults `100` / `1000`, **not** `usize::MAX`. The two rate limits use `usize::MAX` there because unlimited is the safe test default; an unlimited page size would make `effective + 1` overflow-adjacent and would make "default cap applied" pass vacuously in unit tests while failing over the wire.
3. **`validate_env_config()` — the function starts at `src/main.rs:20`** (`:19` is its doc comment; `:48` is only the `// Validate positive integer environment variables` comment). Add both names to the positive-integer list whose entries are at **`:50-52`** (alongside `MACP_MAX_PAYLOAD_BYTES`), so `=0` and unparseable values abort startup at **`:129-133`**. **Plus one new cross-field check**: if both are set and default > max, push `"MACP_LIST_SESSIONS_DEFAULT_PAGE_SIZE (N) must not exceed MACP_LIST_SESSIONS_MAX_PAGE_SIZE (M)"`. Without it the two-layer design silently clamps and the operator never learns their intent was discarded (R7).

**Known compile fallout: `SecurityLayer` is struct-literal-constructed at 11 sites, not 10.** All eleven need the two new fields:

| Site | What it is |
|---|---|
| `security.rs:79` | the `Self {` inside `dev_mode()` (`:78` is the fn signature, not the literal) |
| `security.rs:226` | the `Ok(Self { … })` that closes `from_env()` — **invisible to `grep "SecurityLayer {"`**, which is how the original ten-site list was built |
| `security.rs:416` | test helper `layer_with_tokens` |
| `security.rs:434` | test helper `insecure_layer` |
| `security.rs:611, :636, :663, :833, :863, :890, :915` | seven in-test literals |

Mechanical, but named here so the executor does not discover it as a surprise mid-phase.

### D6 — `base64` dependency

Currently: `[workspace.dependencies] base64 = "0.23"` (root `Cargo.toml:68`), a real dependency of `macp-auth` (`crates/macp-auth/Cargo.toml:21`), and a **dev**-dependency of `macp-runtime` (root `Cargo.toml:114`). **The plan promotes it to a real `[dependencies]` entry of `macp-runtime`** and removes the now-redundant `[dev-dependencies]` line. No new crate enters the tree (already present via macp-auth), no new `cargo audit` surface, and the `deps-isolation` CI job places no constraint on the top crate (it guards `macp-core`, `macp-modes`, `macp-policy`, `macp-storage`). Engine is `URL_SAFE_NO_PAD`; note macp-auth uses `STANDARD` (`auth/resolvers/jwt_bearer.rs:370`), so this is a different engine constant of the same crate, not a new pattern.

### D7 — Where the helpers live

- **Ordering / keyset selection → `macp-storage`** (`crates/macp-storage/src/registry.rs`). The registry owns the map, the lock, and the lock-ordering contract (R10). Putting the selection anywhere else forces the caller to reach through the `pub sessions` field and re-implement the locking discipline. Additive `pub async fn`.
- **Token codec → `macp-runtime`**, new **crate-private** module `src/pagination.rs` (`mod pagination;` in `src/lib.rs`, *not* `pub mod`). It is a gRPC transport concern, so it does not belong in `macp-core` ("Vocabulary… transport-free" per the `CLAUDE.md` crate table) or `macp-storage`. Only `src/server.rs` needs it. Crate-private keeps the published API surface at zero for this piece (R11). Nothing depends back on `macp-modes`; layering is unaffected. **It must ship in the same phase as its caller** — see the Reverify note on the dead-code gate.

### D8 — WatchSessions stays out of scope. Confirmed, with reasons.

- `WatchSessionsRequest` is `message WatchSessionsRequest {}` — empty (`core.proto:428`). No `page_size`, no `page_token`. **There is no protocol-level pagination to implement.**
- `watch_sessions` (`src/server.rs:1281-1357`) calls `get_all_sessions()` for its initial sync, yields one `CREATED` event per session, and builds a `HashSet<String>` for buffered-event dedup. That *is* an unbounded materialization, but it is a **streaming** surface: events go out one at a time; the cost is a transient `Vec<Session>` + `HashSet`, not a single oversized response message. Bounding it is either a pure memory refactor (chunked lock-and-clone) or an upstream proto change. Different problem, different risk profile.
- `plans/defer/follow_ons.md:21-26` item 2 must therefore be **edited, not deleted**, at closeout: strike the shipped `list_sessions` half, strike the stale "replace the documented server-side cap" clause, and leave the `watch_sessions` initial-sync bound as remaining open work with a one-line note that it is a memory bound, not a protocol change, and would need an upstream proto field to become client-controllable.
- **Stale-clause confirmation:** `docs/API.md:98-106` documents no cap — its whole return description is *"Returns a `sessions` array of `SessionMetadata` entries. Authentication is required; the RPC is not filtered by caller identity, so callers should apply their own participation or tenancy checks before exposing results to end users."* — and `src/server.rs:1272-1278` applies none. There is nothing to replace. That second sentence is also the documented, user-facing statement of the unfiltered-listing property the no-MAC decision (D3) rests on, which is why the doc phase must not weaken it.

Also out of scope and untouched: `ListPolicies` (`src/server.rs:1541-1561`), `ListModes`, `ListRoots`.

---

## Phase 1 — Deterministic bounded page selection in the registry

**Status:** DONE — `0c2df9b` (PASS after 2 verify rounds; accumulating, not shipped standalone)

**Delivers.** `SessionRegistry::session_ids_after(after, limit)` — a keyset primitive returning up to `limit` session IDs strictly greater than `after`, in ascending byte order, cloning only the survivors. Plus the `debug_assert` that pins D1's map-key invariant. Nothing on the wire changes; `get_all_sessions()` is untouched and still used by `watch_sessions`.

**Files.** `crates/macp-storage/src/registry.rs`.

**Approach.**
```rust
/// Session IDs strictly greater than `after`, ascending (byte order), at most
/// `limit`. Keyset cursor primitive for ListSessions paging (see plan D1/D2).
///
/// Holds only the map read lock, for one synchronous pass — no session mutex is
/// taken and no `.await` happens under the guard, per the lock-ordering contract
/// documented above (map lock BEFORE session mutex; never hold the map lock
/// across an await).
pub async fn session_ids_after(&self, after: Option<&str>, limit: usize) -> Vec<String>
```
Body: return `Vec::new()` immediately when `limit == 0`. Otherwise take `self.sessions.read().await`, then one synchronous pass over `guard.keys()` maintaining a `BinaryHeap<&String>` (max-heap) of bounded size `limit`: push each key for which `after.is_none_or(|a| k.as_str() > a)`, and `pop()` whenever `len() > limit`. Then materialize with **`into_sorted_vec().into_iter().cloned().collect()`** — ascending, ≤ `limit` elements. Drop the guard before returning.

> **Not `.map(|s| s.clone())`.** Over an iterator of `&String` that is `clippy::map_clone`, which is an **error** under the CI gate `cargo clippy --all-targets -- -D warnings` (`.github/workflows/ci.yml:116`). Use `.cloned()`.

Why the bounded heap rather than collect-all-then-sort: the sort-then-truncate version clones every ID and sorts the whole key set *per page*, so a full traversal is O(n² log n) in string clones and holds the read lock across an n-element sort, blocking writers. The heap is O(n log k) with exactly k clones and the same single pass.

`Option::is_none_or` is stable since Rust 1.82.0, below the pinned MSRV of 1.89.0.

**G3 — enforce the map-key invariant D1 rests on.** D1 argues `session_id` is a stable total order because "the registry map key *is* the session_id". That is structurally true of the type but **never asserted anywhere**: `insert_recovered_session(session_id, session)` (`registry.rs:253-259`) takes the key and the value independently and never checks they agree, and `src/replay.rs:55` assigns the field separately. This matters because `session_ids_after` returns map **keys** and the cursor is a **key**, while `session_to_metadata` emits `session.session_id` — a mismatch would silently violate the ordering acceptance criterion with no test able to see it. Add, as the first statement of `insert_recovered_session`:

```rust
debug_assert_eq!(
    session.session_id, session_id,
    "registry map key must equal Session::session_id — ListSessions paging \
     orders by the key but emits the field (plan D1)"
);
```

All four existing call sites pass matching pairs (`src/main.rs:261`; `registry.rs:324`, `:339`, `:360`), so nothing breaks. `debug_assert` keeps it free in release.

**Acceptance criteria.**
1. `session_ids_after(None, k)` returns the k byte-wise-smallest IDs in the registry, ascending.
2. `session_ids_after(Some(x), k)` never returns `x` and never returns an ID `<= x`.
3. `session_ids_after(Some(x), k)` behaves identically whether or not `x` is present in the map.
4. `limit == 0` returns an empty `Vec` without touching the map contents.
5. `get_all_sessions()` (`registry.rs:241-251`) is byte-for-byte unchanged; the only edit to an existing function is the `debug_assert_eq!` added to `insert_recovered_session`.
6. The method contains no `.await` between acquiring and dropping the map read guard.
7. The materialization uses `.cloned()`, not `.map(|s| s.clone())` — `cargo clippy --workspace --all-targets -- -D warnings` is green, which is the actual proof.
8. `insert_recovered_session` carries the key/field `debug_assert_eq!`, and the existing registry tests still pass (they already pass matching pairs).
9. `cargo build`, `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace --all-targets` all green.

**Tests** (in `registry.rs`'s existing test module — `#[cfg(test)]` at `:287`, `mod tests {` at `:288`, running to the end of the file at `:369`; its only helper is `sample_session` at `:294`, which is what new tests should build on):
- `session_ids_after_returns_ascending_ids`
- `session_ids_after_respects_limit`
- `session_ids_after_is_exclusive_of_cursor`
- `session_ids_after_tolerates_absent_cursor`
- `session_ids_after_zero_limit_is_empty`
- `session_ids_after_matches_sort_then_truncate_reference` — 200 deterministic pseudorandom IDs vs. a `keys().sorted().filter(> cursor).take(limit)` reference
- `session_ids_after_full_traversal_covers_every_id_once`

**Docs.** None (crate-internal primitive, doc-comment only).

**Verifier tier.** Opus.

---

## Phase 2 — Config plumbing for the two page-size limits

**Status:** DONE — `9b91fbf` (PASS after 3 verify rounds; accumulating)

**Delivers.** `MACP_LIST_SESSIONS_DEFAULT_PAGE_SIZE` and `MACP_LIST_SESSIONS_MAX_PAGE_SIZE` parsed, defaulted, clamped, startup-validated, and carried on `SecurityLayer`. Nothing reads them yet; nothing on the wire changes. **This phase runs before the handler so the handler never reads a field that does not exist yet.**

**Files.** `crates/macp-auth/src/security.rs` (struct `:65-72`; `dev_mode` `:78-93`; `from_env` `:118-234`, parse idiom at `:119-122`, `Ok(Self { … })` at `:226-233`; nine test literals at `:416, :434, :611, :636, :663, :833, :863, :890, :915`), `src/main.rs` (`validate_env_config`, starting at `:20`; positive-integer list entries at `:50-52`; abort at `:129-133`), `integration_tests/tests/tier1_protocol/test_startup_gate.rs`.

**Approach.** Exactly D5. Constants `DEFAULT_LIST_SESSIONS_PAGE_SIZE: usize = 100` and `MAX_LIST_SESSIONS_PAGE_SIZE: usize = 1000` at module scope in `security.rs` so `from_env` and `dev_mode` share one source of truth. In `from_env`, parse with the `:119-122` idiom, `.max(1)` each, then `default = default.min(max)` with a `tracing::warn!` on the clamp, and extend the `Ok(Self { … })` at `:226`. In `validate_env_config`, extend the list at `:50-52` and add the cross-field check as a separate block after the loop.

**All eleven `SecurityLayer` construction sites** from D5's table must gain both fields — including `security.rs:79` (`dev_mode`'s `Self {`) and `security.rs:226` (`from_env`'s `Ok(Self {`), neither of which a `grep "SecurityLayer {"` will show you.

`src/main.rs` has **no** `#[cfg(test)] mod tests`. `validate_env_config`'s behavior is therefore proven at Tier 1, in `test_startup_gate.rs`, which already spawns the binary and asserts on exit status.

**G6 — the new startup-gate tests must set `MACP_ALLOW_INSECURE=1`.** Follow the `startup_refuses_invalid_policies_dir` pattern (`test_startup_gate.rs:227-232`), which spawns a raw `std::process::Command` with `MACP_ALLOW_INSECURE=1` + `MACP_MEMORY_ONLY=1` + `MACP_BIND_ADDR=127.0.0.1:0`. Without the flag the binary also refuses to start for an unrelated reason — the no-auth gate at `src/main.rs:307-323`, which `startup_refuses_without_auth_or_insecure_flag` already pins — and an exit-status assertion would then pass while proving nothing about page-size validation. (Today `validate_env_config` runs at `:124`, ahead of that gate at `:307`, so the ordering happens to be favorable; setting the flag makes the test independent of an ordering nothing guarantees.) Assert on the **stderr message naming the variable**, not on exit status alone.

**Acceptance criteria.**
1. With neither var set, `SecurityLayer::from_env()` yields `100` / `1000`.
2. `SecurityLayer::dev_mode()` yields `100` / `1000` — **not** `usize::MAX`.
3. **REVISED after Phase 2 verification.** `MACP_LIST_SESSIONS_DEFAULT_PAGE_SIZE=5000` with max unset **aborts startup**, naming the effective max and whether it was explicit or the built-in default. The original criterion (clamp to 1000, warn, start) left the guard asymmetric: an explicit `DEFAULT > MAX` aborted loudly, while the identical operator error with `MAX` unset was silently clamped behind a `tracing::warn!` that goes to stdout and vanishes under `RUST_LOG=off`. The resolver's `default.min(max)` clamp is retained as defence-in-depth for library consumers who call `SecurityLayer::from_env()` directly and never reach `validate_env_config` (which is private to `src/main.rs`); it simply stops firing for the binary.
4. Setting either var to `0` or to a non-integer aborts startup with a message naming the variable — proven by an actual process spawn that sets `MACP_ALLOW_INSECURE=1`, so the abort is attributable to page-size validation and not to the auth gate.
5. Setting `DEFAULT=2000 MAX=1000` aborts startup with the cross-field message (same `MACP_ALLOW_INSECURE=1` condition).
6. **All eleven** `SecurityLayer` construction sites compile; `cargo test -p macp-auth` green.
7. Adding two `pub` fields to a struct that already has private fields and no `#[non_exhaustive]` is confirmed additive for semver — including through the `macp_runtime::security` re-export at `src/lib.rs:37` (R11).
8. Full local gate green, including Tier 1.

**Tests.**
- `crates/macp-auth/src/security.rs`: `from_env_page_size_defaults_without_env_vars`, `dev_mode_uses_production_page_size_defaults`, `from_env_clamps_default_page_size_to_max`.
- `integration_tests/tests/tier1_protocol/test_startup_gate.rs`: `startup_refuses_zero_or_invalid_list_sessions_page_size`, `startup_refuses_default_page_size_above_max`.

**Docs.** None in this phase — all four env-var tables are updated together in Phase 5 so they cannot drift out of sync one at a time.

**Verifier tier.** Opus.

---

## Phase 3 — Page-token codec and the paged handler (the behavior change)

**Status:** DONE — `3532390` (PASS round 1)

**Delivers.** `src/pagination.rs` (crate-private) with `encode_page_token(session_id) -> String`, `decode_page_token(token) -> Result<String, PageTokenError>` and `MAX_PAGE_TOKEN_CHARS` — **and, in the same diff**, the `list_sessions` rewrite that calls them. `list_sessions` honors `page_size` and `page_token` and emits a real `next_page_token`. **This is the deliberate, spec-sanctioned behavior change.**

**Why codec and handler are one phase.** Shipping the codec alone leaves functions reachable only from `#[cfg(test)]`. `.github/workflows/ci.yml:116` runs `cargo clippy --all-targets -- -D warnings`, and `--all-targets` still builds the plain lib target where those functions are `dead_code` — an error, not a warning, under that gate. There is no crate-level `#![allow(dead_code)]` in this workspace to fall back on. Merging the phases means the codec is called the moment it exists.

**Files.**
- `src/pagination.rs` (new)
- `src/lib.rs` (add `mod pagination;` — **not** `pub mod`)
- root `Cargo.toml` (move `base64` from `[dev-dependencies]:114` to `[dependencies]`), `Cargo.lock` (resolver edge only)
- `src/server.rs` — `list_sessions` (`:1261-1279`); the `make_server()` harness at `:1648` (see the extraction below); new unit tests in the existing `#[cfg(test)] mod tests`

**Approach — (A) the codec.** Exactly D3. `encode` = `URL_SAFE_NO_PAD.encode(format!("v1:{session_id}"))`. `decode` runs checks 1-6 in that order, length before base64. No `unsafe`, no `unwrap`, no `expect`. `PageTokenError` is a crate-private enum whose variants distinguish the failure for *tests and tracing only*; the handler collapses all of them to one opaque `Status` message so the token is not an oracle. Module doc comment records: the format-is-a-free-choice reasoning, the "no TTL because keyset cursors do not go stale" reasoning, and — load-bearing — **the no-MAC reasoning together with the condition that voids it**.

**G1 — the token is never logged.** Per D3: any `tracing` call on the decode path emits the `PageTokenError` discriminant and nothing else. The token is attacker-chosen arbitrary bytes arriving at up to the configured gRPC decode limit (≈1.06 MiB by default; `src/main.rs:390,407`) because the 1024-char check runs only after tonic materializes the field, so a raw-token log line lets an embedded `\n` forge log records.

**Approach — (B) the handler.**
```
1. authenticate  — byte-identical to today's :1265-1269, still the FIRST statement
2. let req = request.into_inner();
3. if req.page_size < 0 -> Err(Status::invalid_argument(
       "INVALID_ARGUMENT: page_size must not be negative"))
4. let effective = if req.page_size == 0 { self.security.list_sessions_default_page_size }
                   else { (req.page_size as usize).min(self.security.list_sessions_max_page_size) };
4b. let effective = effective.max(1);   // floor — see below
5. let cursor = if req.page_token.is_empty() { None }
                else { Some(decode_page_token(&req.page_token).map_err(|_| Status::invalid_argument(
                    "INVALID_ARGUMENT: page_token is not a valid continuation token"))?) };
6. let ids = registry.session_ids_after(cursor.as_deref(), effective.saturating_add(1)).await;
7. let has_more = ids.len() > effective;
8. let page_ids = &ids[..effective.min(ids.len())];
9. let next_page_token = if has_more { encode_page_token(page_ids.last()) }
                         else { String::new() };            // <- from the ID LIST (R4)
10. for id in page_ids { if let Some(s) = registry.get_session(id).await {
        debug_assert_eq!(s.session_id, *id);                // G3: key == field
        push session_to_metadata(&s) } }
11. Ok(Response::new(ListSessionsResponse { sessions, next_page_token }))
```

Points the implementer must not re-decide:
- **Step 3 must be after step 1** (R8). Hoisting the cheap negative-`page_size` check makes an unauthenticated caller receive `INVALID_ARGUMENT`, confirming liveness and leaking validation order pre-auth. A named test pins this.
- **Step 4's cast happens only after the `< 0` guard** (R6).
- **Step 4b is not redundant.** Without a floor, `effective == 0` makes step 7's `has_more` true while step 8's `page_ids` is empty, so step 9's `page_ids.last()` is `None` and `next_page_token` is unspecified — a page that is empty forever with a token that never advances. Today `effective >= 1` is guaranteed four ways (`from_env` applies `.max(1)`, `dev_mode` sets `100`, `validate_env_config` rejects `0`, and `page_size > 0 ⇒ >= 1`), **but D5 deliberately makes the two fields `pub` so callers can set them** — a library consumer of `macp_runtime::security` or a sloppy test can reach `0`. One `.max(1)` closes it. A one-line comment must say exactly that, or it will be deleted as dead code.
- **`effective + 1` is the terminal-page mechanism** (R3): fetch one extra ID, discard it, and "no more results" becomes exact with no extra round trip. `saturating_add` because `effective` is operator-controlled.
- **Step 9 reads `page_ids.last()`, not `sessions.last()`** (R4). A comment must say why, or a future refactor will "simplify" it back into the bug. Prefer `if let Some(last) = page_ids.last()` over `unwrap()`.
- **Step 10 silently skipping a `None`** is correct: the session was removed between the ID scan and the fetch. The resulting short-page-with-non-empty-token is explicitly permitted by `core.proto:411-414`.
- The old `:1273-1274` comment ("Unpaginated: always returns every session…") is replaced by a comment stating the keyset semantics and citing `core.proto:411-426`.

**Approach — (C) the unit-test harness hook (required, not incidental).** The canonical harness in this module is `make_server()` at **`src/server.rs:1648`**, which every test uses. It builds `MacpServer::new(runtime.clone(), SecurityLayer::dev_mode())` inline at `:1654`, so there is no `SecurityLayer` binding to mutate and D5's prescribed `let mut security = SecurityLayer::dev_mode(); security.list_sessions_max_page_size = 3;` has nowhere to attach. **Extract:**

```rust
fn make_server_with_security(security: SecurityLayer) -> (MacpServer, Arc<Runtime>)
```

with `make_server()` reduced to `make_server_with_security(SecurityLayer::dev_mode())`. Every existing caller of `make_server()` is unchanged.

> Do **not** copy the ad-hoc construction at `:1767-1773` (inside `register_ext_mode_requires_authenticated_registry_permission`). It calls `SecurityLayer::from_env().unwrap_or_else(|_| SecurityLayer::dev_mode())` — it reads **ambient process env**, which is precisely the nondeterminism the `pub` fields exist to avoid.

**Acceptance criteria.**

*Codec:*
1. `decode(encode(id)) == Ok(id)` for: a UUID v4, a 22-char base64url token, a legacy non-conforming ID, an ID containing multi-byte UTF-8, and a 200-char ID.
2. `encode(id) != id` for every non-trivial input (opacity is observable).
3. A 2 MiB token is rejected by the length branch — asserted by matching the specific error variant, proving no large decode buffer was allocated. (Safe at unit level; see Phase 4 AC3 for why the *Tier-1* oversize case must be smaller.)
4. Every rejection path returns `Err`; **no input anywhere in the adversarial-input test panics, aborts, or hangs.**
5. `grep -n 'unsafe\|unwrap()\|expect(' src/pagination.rs` returns nothing outside `#[cfg(test)]`.
6. `src/lib.rs` gains `mod pagination;` and **no** new `pub` item.
7. `cargo tree -p macp-runtime | grep base64` shows base64 already present pre-change via macp-auth; `Cargo.lock` gains no new package entry.
8. **No token bytes are logged (G1).** No `tracing` call in `src/pagination.rs` or in `list_sessions` takes the token, a slice of it, or any content-derived value as a field — only the `PageTokenError` discriminant. Verify by reading every `tracing::` call added in this phase.

*Handler:*
9. `page_size = 0` returns at most `list_sessions_default_page_size` entries.
10. `page_size = k` where `0 < k <= max` returns at most `k`.
11. `page_size = max + 1000` returns at most `max`.
12. `page_size = -1` returns `Code::InvalidArgument`.
13. Any token failing D3's checks 1-5 returns `Code::InvalidArgument` with the single opaque message. Both new error messages carry the **`INVALID_ARGUMENT:`** prefix (D4) — the handler emits exactly one prefix.
14. Iterating from an empty token until `next_page_token` is empty visits every session exactly once — no duplicates, no drops — with the registry quiescent.
15. The final page's `next_page_token` is `""`, and no page before it has an empty token.
16. Results are ordered by `session_id` ascending across the whole traversal, not just within a page.
17. An **unauthenticated** request carrying `page_size = -1` returns `Code::Unauthenticated`, not `Code::InvalidArgument`.
18. A token encoding a session_id that no longer exists still returns the correct subsequent page.
19. **Replaying the same `next_page_token` twice returns the identical page** (G4) — the cheapest direct test of D2's load-bearing "a token is a position, not a snapshot handle" claim.
20. With `list_sessions_max_page_size` forced to `0` through the now-`pub` field, the handler still returns a well-formed non-livelocking response (step 4b's floor) — no empty page paired with a non-empty token.
21. `make_server_with_security` exists, `make_server()` delegates to it, and no existing test in the module changed.
22. `git diff src/server.rs` shows changes confined to `list_sessions`, `make_server`/`make_server_with_security`, and the test module — `watch_sessions` (`:1281+`), `list_policies` (`:1541+`), and `session_to_metadata` (`:162-191`) untouched.
23. Full local gate green, including Tier 1 (existing `tier1_jwt.rs:400-408` calls ListSessions with a default request, i.e. `page_size = 0` — must still pass unchanged).

**Tests — codec** (in `src/pagination.rs`):
- `token_round_trips_session_id`
- `token_is_not_the_bare_session_id`
- `decode_rejects_garbage`
- `decode_rejects_non_base64`
- `decode_rejects_truncated_token`
- `decode_rejects_oversized_token_without_decoding`
- `decode_rejects_wrong_version_prefix` (`v2:`, `v:`, `1:`, no prefix)
- `decode_rejects_valid_base64_of_invalid_utf8`
- `decode_rejects_empty_cursor_after_prefix`
- `decode_never_panics_on_adversarial_input` — ~1000 deterministic mutations

**Tests — handler** (in `src/server.rs`'s `#[cfg(test)] mod tests`; build servers via the new `make_server_with_security`, mutating the two `pub` page-size fields for determinism; populate via `registry.insert_recovered_session`, passing a `Session` whose `session_id` equals the key — the Phase 1 `debug_assert_eq!` enforces it):
- `list_sessions_applies_default_page_size_when_zero`
- `list_sessions_honors_explicit_page_size`
- `list_sessions_clamps_page_size_above_max`
- `list_sessions_rejects_negative_page_size`
- `list_sessions_rejects_garbage_page_token`
- `list_sessions_full_traversal_visits_every_session_exactly_once`
- `list_sessions_terminal_page_has_empty_next_page_token`
- `list_sessions_orders_by_session_id_ascending`
- `list_sessions_still_requires_authentication` — the R8 ordering guard
- `list_sessions_tolerates_cursor_for_removed_session`
- `list_sessions_replaying_a_token_returns_the_identical_page` — G4
- `list_sessions_survives_zero_effective_page_size` — the step-4b floor

**Docs.** None in this phase — Phase 5 owns every doc edit.

**HARD REQUIREMENT carried from Phase 2 verification — the env-var binding is currently unproven.** Phase 2's `from_env` passes the two page-size env values into the resolver, and a verifier swapped them so `MACP_LIST_SESSIONS_MAX_PAGE_SIZE` fed the default and vice-versa: **the entire suite still passed** (626 workspace, 92 Tier-1, 8 JWT, 5 Tier-2, zero failures). Every unit test drives the name-agnostic pure resolver, and the one `from_env` test runs with both vars unset, where a swap is indistinguishable. Phase 2 made the swap unrepresentable via a named struct, but only an end-to-end test proves the binding. This phase (or Phase 4) MUST add Tier-1 coverage using `server_manager::start_with_env` that sets `MACP_LIST_SESSIONS_DEFAULT_PAGE_SIZE` to a distinctive non-default (e.g. `7`) and asserts `ListSessions{page_size: 0}` returns exactly that many rows, plus a second case setting `MACP_LIST_SESSIONS_MAX_PAGE_SIZE` to a **different** distinctive value (e.g. `3`) and asserting an over-large `page_size` clamps to it. **The two values must differ** — identical values would not detect a swap.

**Verifier tier.** **Opus.** Wire-visible but not a one-way door: nothing persisted in a new format, no on-wire format committed (the `v1:` prefix reserves the escape hatch), single-function revert. The irreversible moment is the human's merge, which is outside this plan.

---

## Phase 4 — Tier-1 gRPC integration tests

**Status:** DONE — `06b5d94` (PASS round 1; Tier-1 92→100)

**Delivers.** The required tests exercised through the real gRPC boundary against the real binary, plus a reusable `list_sessions_as` helper.

**Files.** `integration_tests/tests/tier1_protocol/test_list_sessions_pagination.rs` (new), `integration_tests/tests/tier1_protocol/mod.rs` (add `mod test_list_sessions_pagination;`), `integration_tests/src/helpers.rs` (add `list_sessions_as`).

**Approach.** Each test spawns its **own** server via `ServerManager::start_with_env` (`integration_tests/src/server_manager.rs:60`), following `test_limits.rs:14-18`, setting the two new env vars per test. Not optional: the shared server in `tests/common/mod.rs` accumulates sessions from every other test in the binary, so any page-count or exact-traversal assertion against it is inherently flaky. `start_with_env` already forces `MACP_ALLOW_INSECURE=1` and `MACP_MEMORY_ONLY=1` (`server_manager.rs:68-69`, with `MACP_BIND_ADDR` at `:70`), giving each test a clean, empty registry.

`list_sessions_as` mirrors `get_session_as` (`helpers.rs:117-131`):
```rust
pub async fn list_sessions_as(
    client: &mut MacpRuntimeServiceClient<Channel>,
    sender: &str,
    page_size: i32,
    page_token: &str,
) -> Result<ListSessionsResponse, tonic::Status>
```
Sessions are created with existing `send_as` + `session_start_payload` helpers so IDs are real UUIDs — arbitrary sort order relative to creation order, which is what makes the ordering assertion meaningful.

**G5 — stay under the session-start rate limit.** `MACP_SESSION_START_LIMIT_PER_MINUTE` defaults to `60` **per sender** (`security.rs:124-130`). The full-traversal test creates 25 sessions from one sender, which is safe today but would silently become a rate-limit failure dressed up as a pagination bug if that default is ever lowered or the fixture grows. Either spread the creations across several senders, or pin the ceiling explicitly by passing `("MACP_SESSION_START_LIMIT_PER_MINUTE", "1000")` through `start_with_env`.

**Acceptance criteria.**
1. Tests exist with these exact names:
   - `default_page_size_applied_when_page_size_is_zero`
   - `explicit_page_size_is_honored`
   - `page_size_above_max_is_clamped`
   - `full_traversal_yields_every_session_exactly_once`
   - `terminal_page_returns_empty_next_page_token`
   - `garbage_page_token_returns_invalid_argument`
   - `negative_page_size_is_rejected`
   - `replayed_page_token_returns_the_identical_page` — the Tier-1 companion to Phase 3's G4 unit test
2. `full_traversal_...` creates ≥ 25 sessions, pages at size 4, and asserts both `set.len() == 25` **and** `total_collected == 25` (together these rule out duplicates *and* drops — a set alone hides duplicates). It stays under the session-start rate limit per G5.
3. `garbage_page_token_...` sends at minimum `"not-a-token"`, base64url of `"v2:abc"`, a token whose **prefix** is damaged (base64url of `"v1"`, plus a front-truncated `valid[1..]`), and an **oversized token of ~64 KiB** — each asserting `Code::InvalidArgument`. **Not 2 MiB over the wire:** `src/main.rs:407` sets `max_decoding_message_size(max_payload_bytes + ENVELOPE_OVERHEAD_BYTES)` = 1 MiB + 64 KiB by default (`:390`), so a 2 MiB request is rejected by tonic at decode with a *different* status code and never reaches the handler's length check. 64 KiB is far above `MAX_PAGE_TOKEN_CHARS` (1024) and far below the decode limit, so it proves the handler's own branch. (The 2 MiB case belongs in the Phase 3 unit test, where no gRPC decode is involved.) **Also corrected during Phase 3: do NOT use an end-truncated valid token.** Chopping bytes off the end of an encoded token frequently yields a well-formed *shorter* cursor — `base64url("v1:session-000")` truncated by 3 chars decodes cleanly to `v1:session-0` — so the handler correctly returns a page instead of rejecting, and the assertion fails. This is harmless in itself (a cursor is a position, not a handle, so a shorter cursor yields a differently-positioned but still-correct page), but it means D3's "the prefix check covers truncation" holds only for damage that actually reaches the prefix.
4. `page_size_above_max_is_clamped` runs with `MACP_LIST_SESSIONS_MAX_PAGE_SIZE=3`, creates 10 sessions, requests `page_size=1000`, asserts exactly 3 returned with a non-empty token.
5. `mod.rs` declares the new module; the suite runs clean under `--test-threads=1`.
6. Tier-1 suite green; record the real observed test count (was 90 + 8 JWT).
7. `integration_tests/Cargo.lock` checked for staleness before pushing; if it changes, commit separately as housekeeping.

**Docs.** None; `CLAUDE.md`'s Tier-1 count is updated in Phase 5 with the real number measured here.

**Verifier tier.** Opus.

---

## Phase 5 — Documentation, env-var table sync, and deferred-item bookkeeping

**Status:** DONE — `d77e9c1` (PASS after 2 rounds)

**Delivers.** Every doc describing `ListSessions` or the env-var surface reflects the new contract; the mirrored env tables stay in sync; `plans/defer/follow_ons.md` item 2 is correctly narrowed rather than wrongly closed, and its live index in `plans/defer/README.md` is narrowed with it.

**Files.**
- `docs/API.md` — rewrite the ListSessions section (`:98-106`): request fields, clamp/default rules, the **verbatim D2 keyset-semantics paragraph**, the `INVALID_ARGUMENT` conditions, and a "pass `next_page_token` back verbatim; stop when empty" loop sketch. Keep `:106`'s second sentence (the unfiltered-listing caveat) — D3's no-MAC decision cites it. Add a new **"Resource limits"** heading near the Rate Limiting table at `:283-292` — a page cap is not a rate limit. Add a note that API.md follows `core.proto:411-426` because RFC-0006 §3.8 has not been updated, pointing at the upstream issue.
- `README.md` — `:73` (paged wording) and the Resource limits table at `:192-196`. `:241` stays as-is.
- `docs/deployment.md` — env table at `:19-44`; the "Observation-surface authorization" paragraph at `:76-82` gains one sentence noting ListSessions is now paged and that the no-signature token decision rests on this section's unfiltered-listing property.
- `CLAUDE.md` — env-var table at `:341-372`, and the Tier-1 test count in "Integration tests" (`:142`).
- `docs/README.md:13` — add "paged" for ListSessions.
- `plans/defer/follow_ons.md:21-26` — rewrite item 2 per D8.
- **`plans/defer/README.md:9-10` (G2)** — this is a **live index**, not a dated journal entry: it enumerates `follow_ons.md`'s contents and names *"ListSessions pagination"* among them. Narrowing item 2 makes it wrong. Amend the enumeration to name the remaining work (`watch_sessions` initial-sync bound) rather than the shipped one.

**The env-table set: there are three today, and this phase creates a fourth.** `MACP_MAX_PAYLOAD_BYTES` appears in exactly three places — `docs/deployment.md:37`, `README.md:194`, `CLAUDE.md:360` — and **not** in `docs/API.md`. So the new "Resource limits" table in API.md is a *new* mirror. **If it listed only the two new page-size vars, the four tables would diverge the moment they were created.** The new API.md table must therefore list **all five** resource-limit variables: `MACP_MAX_PAYLOAD_BYTES`, `MACP_SESSION_START_LIMIT_PER_MINUTE`, `MACP_MESSAGE_LIMIT_PER_MINUTE`, `MACP_LIST_SESSIONS_DEFAULT_PAGE_SIZE`, `MACP_LIST_SESSIONS_MAX_PAGE_SIZE` — the same set the other three tables carry after this phase. (The two rate-limit vars already have their own table at API.md `:287-290`; cross-reference rather than duplicate them, or fold that table into the new heading — either way one page must not list a variable twice.)

**Approach.** All four env tables in one commit so they cannot drift.

**Historical records that stay untouched** (both are dated entries; amending either would falsify the record):
- `docs/change-review-phases-a-e.md:360` — a change-review record.
- **`plans/BUILD_STATUS.md:145`** — sits inside the dated **`2026-07-05`** entry that begins at `:135` ("v0.5.0 RELEASED AND VERIFIED"), where "real ListSessions pagination (replace the server-side cap)" describes what was queued *at that date*. It is a log, not an index. **Leave it alone** — stated explicitly so an executor does not have to guess, and does not "helpfully" update it by analogy with `plans/defer/README.md`, which *is* an index and *does* get amended.

Also untouched: `docs/testing.md` (line 40 has no hardcoded count).

**Example clients:** `grep -rn "list_sessions\|ListSessions" src/bin/` returns **nothing**. No `src/bin` change and no `docs/examples.md` change required. State this in the PR body so a reviewer need not re-derive it.

**Acceptance criteria.**
1. All four env tables (`docs/deployment.md`, `README.md`, `CLAUDE.md`, the new `docs/API.md` "Resource limits") list the **same five** resource-limit variables with identical names and defaults — `1048576` / `60` / `600` / `100` / `1000`. Verify by grepping each of the five names and counting four hits per variable.
2. `docs/API.md`'s ListSessions section contains the D2 paragraph verbatim, no longer claims an unbounded list, and retains `:106`'s unfiltered-listing caveat.
3. `plans/defer/follow_ons.md` item 2 no longer claims the `list_sessions` work is open, notes the "documented server-side cap" clause was stale, and retains the `watch_sessions` initial-sync bound with the memory-vs-protocol distinction.
4. `plans/defer/README.md:9-10` no longer lists "ListSessions pagination" as pending work, and instead names the remaining `watch_sessions` bound — the index and `follow_ons.md` agree.
5. `plans/BUILD_STATUS.md` and `docs/change-review-phases-a-e.md` are byte-for-byte unchanged (`git diff --stat` proves it).
6. `CLAUDE.md`'s Tier-1 count matches Phase 4's observed number.
7. `grep -rn "list_sessions\|ListSessions" src/bin/` returns nothing (re-verify at implementation time).
8. `cargo doc --no-deps` (the `docs` CI job) green.

**Verifier tier.** Opus.

---

## Phase 6 — Ship (branch, PR, CI). No merge.

**Status:** NOT STARTED

**Delivers.** A feature branch off current `main` (`d500910`), one commit per phase, a PR whose body names the behavior change, and green CI. **The human merges. This plan does not.**

**Approach.**
- Branch `feat/list-sessions-pagination` off `main` at `d500910`.
- Local gate before **any** push: `cargo fmt`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace --all-targets`, `cargo build`, then `cd integration_tests && MACP_TEST_BINARY=../target/debug/macp-runtime cargo test -- --test-threads=1`. `integration_tests/` is a separate crate outside the workspace with its own `Cargo.lock` — check for staleness, commit any change separately.
- Conventional commit on the **Phase 3** commit (the one carrying the codec + handler) with the break stated in the body:
  ```
  feat(server): paginate ListSessions with page_size and an opaque page_token

  BEHAVIOR CHANGE: ListSessions now returns a bounded page. A client that
  ignores next_page_token will see a short list where it previously saw
  every session. This is the contract macp-proto 0.1.8 defines
  (proto/macp/v1/core.proto:411-426); the previous unpaginated response
  was non-conforming. page_size=0 yields a server default
  (MACP_LIST_SESSIONS_DEFAULT_PAGE_SIZE, 100); larger values are clamped to
  MACP_LIST_SESSIONS_MAX_PAGE_SIZE (1000); negative values and malformed
  tokens return INVALID_ARGUMENT.
  ```
- PR body must state: the behavior change in the same terms; the public-API delta (two `pub` fields on `SecurityLayer` — also reachable as `macp_runtime::security::SecurityLayer` via the `src/lib.rs:37` re-export — and one method on `SessionRegistry`, both additive); that `WatchSessions` is deliberately untouched and why (D8); that RFC-0006 §3.8 is stale with an upstream issue filed; and that no `src/bin` example needed updating.
- **Do not rebase, retarget, close, or otherwise touch PR #114.**

**Acceptance criteria.**
1. Branch is off `d500910`.
2. Every commit is conventional-commit formatted; the **Phase 3** commit body contains the behavior-change statement.
3. All CI jobs in `ci-pass`'s needs-list are green — or any red job is proven pre-existing on `origin/main` and stated as such in the PR body.
4. PR #114 shows no new activity attributable to this work.
5. The PR is **not** merged.

**Verifier tier.** Opus. The one-way door in this phase (merge) is explicitly excluded from it.

---

## Deferred follow-ups (do not do in this change)

1. **Sub-linear paging scan.** `session_ids_after` is O(n) per page even with the bounded heap. A `BTreeMap<String, ()>` ID index alongside the `HashMap` would give O(log n + k), but it changes the registry's data structure and every insert/remove path, is invisible at the wire, and is unnecessary at the scales this runtime targets.
2. **`watch_sessions` initial-sync memory bound** — per D8, remains open in `plans/defer/follow_ons.md` item 2 (and in the `plans/defer/README.md:9-10` index, which Phase 5 narrows to name this item rather than the shipped `list_sessions` half).
3. **Pagination for `ListPolicies` / `ListModes` / `ListRoots`** — their proto messages carry no page fields; needs an upstream spec change first.
4. **Upstream RFC-0006 §3.8 / RFC-0001:371 update** — the RFC prose still describes an unpaginated listing.

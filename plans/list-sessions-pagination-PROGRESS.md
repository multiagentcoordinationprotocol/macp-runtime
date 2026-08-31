# PROGRESS — ListSessions pagination

Plan: [`plans/list-sessions-pagination.md`](list-sessions-pagination.md)
Branch: `feat/list-sessions-pagination`, cut from `main` @ `d500910` (2026-08-30)
Model tiering for this run: **Opus plans, Opus executes, fresh Opus verifies** (user override
of `/implement`'s default Sonnet executor; verifier independence preserved).

## Status

| Phase | Title | Status | Verdict | Rounds | Commit |
|-------|-------|--------|---------|--------|--------|
| 0 | Plan + reverify | DONE | FLAWED → patched | 1 | (uncommitted) |
| 1 | Registry keyset primitive | DONE | PASS (2 rounds) | 2 | `0c2df9b` |
| 2 | Config plumbing | DONE | PASS (3 rounds) | 3 | `9b91fbf` |
| 3 | Page-token codec + paged handler | DONE | PASS (1 round) | 1 | `3532390` |
| 4 | Tier-1 integration tests | DONE | PASS (1 round) | 1 | `06b5d94` |
| 5 | Docs + env-table sync | DONE | PASS (2 rounds) | 2 | `d77e9c1` |
| 6 | Ship (PR, CI, no merge) | IN PROGRESS | — | — | — |

## Repo map

Built once (2026-08-30) so later phases read this instead of re-scanning.

### The RPC surface
| Path | Purpose |
|------|---------|
| `src/server.rs:1261-1279` | `list_sessions` — the primary edit site. Auth, `get_all_sessions()`, hardcoded empty `next_page_token`. |
| `src/server.rs:162-191` | `session_to_metadata` — associated fn, used as `.map(Self::session_to_metadata)`. Unchanged by this work. |
| `src/server.rs:1281-1359` | `watch_sessions` — also calls `get_all_sessions()` for initial sync. **Deliberately out of scope** (plan D8). |
| `src/server.rs:757-785` | `status_from_error` — maps `MacpError` to `Status`. Used for auth failures only in this work. |
| `src/server.rs:797, 860, 1385, 1477` | Precedents for direct `Status::invalid_argument` on request-shape errors — the pattern this work follows. |
| `src/server.rs:34-52` | `MacpServer` struct + `new()`. Holds `security: SecurityLayer`. |
| `src/server.rs:1635-2808` | Inline `#[cfg(test)] mod tests`. Harness: `make_server():1648`, `do_send():1667`, `start_payload():1672`. |

### Storage
| Path | Purpose |
|------|---------|
| `crates/macp-storage/src/registry.rs:150-153` | `SessionRegistry { sessions: RwLock<HashMap<String, SharedSession>>, .. }` — the nondeterministic-order root cause (plan R1). |
| `crates/macp-storage/src/registry.rs:141-147` | **Lock-ordering contract**: map lock before session mutex; never hold the map lock across an await. Binding on Phase 1. |
| `crates/macp-storage/src/registry.rs:241-251` | `get_all_sessions()` — must remain byte-for-byte unchanged. |
| `crates/macp-storage/src/registry.rs:~287+` | `#[cfg(test)] mod tests` with `sample_session` helper — where Phase 1's tests go. |

### Config
| Path | Purpose |
|------|---------|
| `crates/macp-auth/src/security.rs:65-72` | `SecurityLayer` struct — `pub max_payload_bytes` is the precedent for the two new page-size fields. |
| `crates/macp-auth/src/security.rs:77-93` | `dev_mode()` — new fields must use **production** defaults here, not `usize::MAX` (plan D5). |
| `crates/macp-auth/src/security.rs:118-137` | `from_env()` — the parse site. Silent fallback on garbage, by design. |
| `src/main.rs:48-67` | `validate_env_config()` — the *aborting* layer. Rejects unparseable and zero. No `#[cfg(test)]` module in this file. |
| `src/main.rs:305, 328` | `SecurityLayer::from_env()` → `MacpServer::new()` — how config reaches the handlers. |

### Tests
| Path | Purpose |
|------|---------|
| `integration_tests/tests/tier1_protocol/mod.rs` | Module list; Phase 5 adds `mod test_list_sessions_pagination;`. |
| `integration_tests/tests/common/mod.rs:14,49` | `endpoint()` / `grpc_client()` — **shared** server per binary. Not usable for exact-count assertions. |
| `integration_tests/src/server_manager.rs:55,60` | `start()` / `start_with_env()` — Phase 5 uses `start_with_env` for an isolated registry. Precedent: `test_limits.rs:16`. |
| `integration_tests/src/helpers.rs:33,44,64,101,117` | `with_sender`, `envelope`, `session_start_payload`, `send_as`, `get_session_as` (the shape `list_sessions_as` mirrors). |
| `integration_tests/tests/tier1_jwt.rs:400-410` | The **only** existing ListSessions test — auth-only, asserts nothing about the response shape. |
| `integration_tests/Cargo.lock` | Separate crate outside the workspace; dependabot never updates it. Check staleness before pushing. |

### Docs that must move together
`CLAUDE.md:341-372` (canonical env table) · `README.md:73, 191-196` · `docs/deployment.md:19-44, 76-82` ·
`docs/API.md:98-106, 284-292` · `docs/README.md:13` · `plans/defer/follow_ons.md:21-26`.

**No example client calls ListSessions** — `grep -rn "list_sessions\|ListSessions" src/bin/` is empty, so
`CLAUDE.md`'s "Example maintenance" list needs no `src/bin` change.

## Log

### 2026-08-30 — Phase 0 (plan)
- Opus planner produced `plans/list-sessions-pagination.md` after a deep read at `d500910`.
- **Correction it made to the brief:** RFC-MACP-0006 §3.8 (`rfcs/RFC-MACP-0006-transport-bindings.md:146-160`
  in the spec repo @ `65f6805`) contains **no pagination language at all** — it still says ListSessions
  "returns `SessionMetadata` for all currently known sessions." `RFC-MACP-0001-core.md:371` likewise.
  Spec issue #38 / PR #51 shipped the proto fields and the `docs/` summaries but never updated the RFC prose.
  The proto comments (`core.proto:411-426`) are therefore the binding source, as originally briefed.
  No plan decision changed; the consequence is an upstream spec defect needing an issue.
- **Scope confirmed:** `WatchSessions` stays out — `WatchSessionsRequest` is empty at `core.proto:428`, so
  there is no protocol-level pagination to add there. `plans/defer/follow_ons.md` item 2 gets *narrowed*,
  not closed.
- **Stale doc confirmed:** `follow_ons.md`'s "replace the documented server-side cap" clause is inaccurate —
  `docs/API.md:98-106` documents no cap and `src/server.rs` applies none.
- Fresh Opus reverify pass dispatched (adversarial: citation audit, algorithm attack, semver claim, coverage gaps).

### 2026-08-30 — Phase 0 decisions confirmed by repo owner
- **Page-size defaults: `100` default / `1000` max.** Confirmed as-is; the plan's D5 numbers stand.
  These become documented public config surface at v0.7.0, raisable later by env var without a code change.
- **Upstream spec issue: APPROVED to file.** Cross-repo write authorized for *issue creation only* on
  `multiagentcoordinationprotocol/multiagentcoordinationprotocol` — no edits to that repo. Filed in Phase 7
  so it can link the PR. Covers RFC-MACP-0006 §3.8 and RFC-MACP-0001-core.md:371 still describing
  ListSessions as unpaginated.
- Release ordering (decided before planning): **PR #114 (release-plz v0.7.0) is held** so this work lands
  first and ships inside v0.7.0. #114 must not be touched.

### 2026-08-30 — Phase 0 reverify: VERDICT FLAWED, patched
Fresh Opus adversarial pass (citation audit + algorithm attack + semver check + coverage sweep).

**Four blocking defects, all patched into the plan:**
1. **Dead-code gate.** Old Phase 2 landed `src/pagination.rs` crate-private with "nothing calls it yet".
   No crate-level `#![allow(dead_code)]` exists anywhere in the workspace, and `ci.yml:116` runs
   `clippy --all-targets -- -D warnings` (which still builds the non-test lib target). The phase could
   not have passed its own gate. **Fix: merged the codec into the handler phase**; phases renumbered 7 → 6.
2. **`SecurityLayer` has 11 construction sites, not 10** — the list missed `Ok(Self { .. })` closing
   `from_env()` at `security.rs:226`, invisible to a `grep "SecurityLayer {"`. Also `:78` → `:79`.
3. **Wrong unit-test harness.** Old Phase 4 cited `src/server.rs:1767-1773`, an ad-hoc block that calls
   `SecurityLayer::from_env()` — ambient process env, the exact nondeterminism the `pub` fields exist to
   avoid. Canonical harness is `make_server()` at `:1648`, which needs a
   `make_server_with_security(..)` extraction to be injectable.
4. **`.map(|s| s.clone())`** in Phase 1 trips `clippy::map_clone` → error under `-D warnings`.
   Fix: `.into_iter().cloned().collect()`.

**Six gaps folded into phases:** log injection via raw token bytes in `tracing` (G1);
`plans/defer/README.md:9-10` going stale (G2); the map-key == `session.session_id` invariant assumed
but never enforced (G3); no page-token idempotence test (G4); Tier-1 fixtures near the 60/min
session-start rate limit (G5); startup-gate tests needing `MACP_ALLOW_INSECURE=1` or they pass for the
wrong reason (G6). Plus a reachable `effective == 0` hole in the handler, guarded with `.max(1)`.

**Also corrected:** D1's "no post-construction mutation of `session_id`" is false —
`src/replay.rs:55` assigns it (harmlessly, in the recovery path). R11's "zero new public API" is
overstated — `src/lib.rs:37` re-exports `security`, so the new `pub` fields do surface; the semver
conclusion still holds. Several line numbers off by 1-3. Three env tables exist today, not four.

**Load-bearing claims CONFIRMED correct:** the bounded max-heap yields the `limit` smallest candidates
and `into_sorted_vec()` is ascending; `Option::is_none_or` stabilized in 1.82.0, under the pinned
MSRV 1.89.0 / toolchain 1.96.1; **no pagination counterexample could be constructed**, including the
deleted-cursor case (the cursor is minted from the ID list, so it advances regardless of fetch outcome,
strictly increases, terminates); the unsigned-token argument holds on both legs (`src/server.rs:1265`
binds `let _identity` and discards it; `docs/deployment.md:76-82` documents all-sessions-to-any-identity);
`SecurityLayer` is 5-of-6 private with no `#[non_exhaustive]`; Tier-1 is exactly 90 + 8 JWT.

### 2026-08-30 — Phase 1 round 1: VERDICT GAPS
Executor (Opus) delivered `session_ids_after` + tests; full workspace gate green (24 binaries, 0 failures).
Fresh Opus verifier confirmed the **core algorithm correct** — ordering, cursor exclusivity, absent-cursor
behaviour, borrow correctness and termination all verified; it could not construct a skip or repeat.
It independently re-ran the executor's three mutations rather than taking them on trust.

**Gaps found (fix round dispatched):**
- **G1 — latent panic + memory amplification.** `BinaryHeap::with_capacity(limit + 1)` sizes from the
  *requested* limit, unbounded by map size. `session_ids_after(None, usize::MAX)` **panics** in debug
  ("attempt to add with overflow"); `limit = 10_000_000` eagerly allocates ~80 MB against a 3-entry map.
  Since `limit` will come from a client-supplied `page_size`, this is a memory-amplification vector.
- **G2 — the `debug_assert_eq!` guards nothing reachable.** Its only non-test caller (`src/main.rs:261`)
  passes two values that are equal by construction, so it can never fire. The path that *can* violate the
  invariant — `load_sessions` (`registry.rs:192-195`), which inserts under the JSON map key without
  comparing it to `PersistedSession.session_id` — is unguarded. A hand-edited `sessions.json` would make
  paging order by one value and emit another.
- **G3 — a mutation the tests do NOT catch.** With the heap bound deleted,
  `full_traversal_covers_every_id_once` still passed: an unbounded impl returns one giant page, which
  satisfies coverage and no-duplicates trivially. The loop never asserts `page.len() <= page_size`.
- G4 weak count-only assertion at `:417`; G5 no large-`limit` test (G1 uncovered); G6 rustdoc omits the
  not-a-consistent-snapshot contract; G7 three misleading comments.

**Verifier recommendation NOT taken:** it proposed dropping the `session_id` parameter from
`insert_recovered_session` to make the invariant unrepresentable. Correct instinct, but `macp-storage` is
published and `release-plz.toml` sets `semver_check = true` — removing a parameter from a `pub async fn`
is a breaking change that would block the release PR being held for this work. Taking the additive fix
instead: a `tracing::warn!` + repair in `load_sessions`, signature untouched.

**Shippability:** hold for the closing PR, not a standalone one. `dead_code` is not a blocker (verified —
`pub` on a `pub` struct in a `pub mod`), but publishing a new `pub async fn` to crates.io *before its only
consumer exists* risks a semver-breaking signature change later if Phase 3's handler needs a different
shape (e.g. `(Vec<String>, bool)` or the `Arc` handles).

**Operational note:** the Phase 1 change is uncommitted; `git checkout` on `registry.rs` destroys it.
The verifier hit this and recovered from a byte copy. Fixer instructed to back up with `cp`, never git.

### 2026-08-30 — Phase 1 round 1 fixes applied
All seven gaps closed inside `crates/macp-storage/src/registry.rs`; no other file touched, no API change.
- **G1** `BinaryHeap::with_capacity(limit.saturating_add(1).min(guard.len().saturating_add(1)))` — allocation
  is now proportional to the map, never to the caller-supplied limit, and the overflow is gone. Chose bounded
  capacity over `BinaryHeap::new()` to keep the single-allocation property for the normal small-page case.
- **G2** real guard added in `load_sessions` (`:195-213`): on `record.session_id != id`, `tracing::warn!` naming
  both values, then repair via `record.session_id.clone_from(&id)` — the map key wins, since that is what paging
  orders by and what `get_session` looks up. Lenient (no skip, no abort) so startup recovery stays available.
  `insert_recovered_session`'s signature untouched; the `debug_assert_eq!` stays as a boundary-contract marker
  with a comment pointing at where real enforcement lives.
- **G3** in-loop `assert!(page.len() <= page_size)` — confirmed it now catches the deleted-heap-bound mutation
  that previously slipped through (`page_size=1: page of 200 exceeds the limit`).
- **G4** count-only assertion replaced with a full-contents one. **G5** `session_ids_after_handles_huge_limits`
  covers `usize::MAX`, `usize::MAX - 1`, `10_000_000`, `1 << 40`, with/without cursor, plus an empty registry.
- **G6** rustdoc now states the not-a-consistent-snapshot contract for multi-page traversal. **G7** three
  comments reworded; the `{ }` block deliberately **kept** — flattening it would extend the guard's life to the
  end of the function instead of ending it at the clone.

**G5 fail-then-pass demonstrated:** against the unmodified code the new test panicked at `registry.rs:272:75`
("attempt to add with overflow") — column 75 is the `limit + 1` expression, confirming the verifier's diagnosis
exactly. After the G1 fix: 10 passed, 0 failed.

**Gate:** fmt clean, clippy `-D warnings` clean, `cargo test --workspace --all-targets` **621 passed, 0 failed**.

**Follow-up dispatched:** the fixer wrote a test for G2's new repair behaviour, verified it, then deleted it to
respect a "no additions beyond the listed gaps" instruction. That was my instruction being too blunt — G2
introduced runtime behaviour with zero standing coverage, so the test is being restored (asserting the repaired
`session_id`, the absent stale key, AND that `session_ids_after` reflects the repair, which is what ties the
guard to the invariant it protects).

### 2026-08-30 — Phase 1 round 2 re-verify: all 7 gaps CLOSED, 1 comment-only item
Fresh Opus re-verify against the round-1 gap list (not a cold diff review). **G1, G3, G4, G5, G6, G7 and the
restored G2 test: CLOSED and mutation-proven.** G2 PARTIAL — the repair itself is correct and correctly
placed (`registry.rs:203-213`, warn then `clone_from` before `record.into()`), but a *comment* about it is
false; fix dispatched.

**Mutations the re-verifier ran itself (5):** deleted heap bound → traversal test now fails
(`page of 200 exceeds the limit`) — the regression round 1 proved was previously invisible; repair removed
→ `left: "B", right: "A"`; `map`→`filter_map` skip → `unwrap()` on `None`; `limit + 1` restored →
overflow panic; `saturating_add` without the map-size cap → `capacity overflow`. That last one shows the
huge-limits test bites **both halves** of G1, not just the overflow. Forcing `capacity = 0` left all 8
tests passing, proving capacity is purely an allocation hint and the `.min()` cannot lose elements.

**The comment-only gap:** the fixer's note claims "the enforcement that matters for real data lives in
`load_sessions`". False here — `src/main.rs:178` uses `SessionRegistry::new()`, never `with_persistence`
(which has **zero** non-test callers repo-wide), and `sessions.json` is the legacy v1/v2 format that
`storage/migration.rs:37,134` renames to `.migrated` at startup. Production recovery is
`storage.list_session_ids()` → `replay_session()` → `insert_recovered_session()`. The repair is real value
for external consumers of the published API, but the production path is guarded *structurally* instead:
`src/replay.rs:55` and `:279` both force the session id to the directory-derived one. Comment reworded.

**Round-1 findings re-confirmed:** the algorithm body is byte-identical to round 1 except the one capacity
line; ordering, strict exclusivity, absent-cursor equivalence, no `.await` under the guard, and an
untouched `get_all_sessions()` all still hold. 622 passed / 0 failed re-run and confirmed; clippy and fmt
clean; no public API signature changed.

**Pre-existing semver break found (NOT caused by this work, and NOT a blocker):**
`cargo semver-checks check-release -p macp-storage` reports a major break —
`RocksDbBackend is no longer UnwindSafe/RefUnwindSafe` (`storage/rocksdb.rs:13`) — reproduced on a clean
worktree at `d500910`. The re-verifier concluded it "will block a release PR". **It will not.**
`cargo-semver-checks` is not in `ci-pass`'s needs list (`ci.yml:592`); per `release-plz.toml:12-16` it runs
only while release-plz *computes* the release PR, where its job is to pick the version bump, not to veto.
Corroborated by PR #114 itself: it proposes `0.6.1 → 0.7.0`, and in 0.x a minor bump **is** the breaking
bump — release-plz already absorbed this regression into the version choice.

### 2026-08-30 — Phase 1 CLOSED: PASS after 2 rounds
Comment corrected and re-verified (41 macp-storage tests green, clippy/fmt clean). Committed as **`0c2df9b`**;
plan + tracker as **`1f05da9`**. Accumulating toward the closing PR — **not** shipped standalone, per both
verifiers: `session_ids_after` has zero consumers, and all seven crates publish to crates.io in lockstep, so
merging a new `pub async fn` before Phase 3 validates its shape risks a semver-breaking change later.
Verifier tier: Opus both rounds (not a one-way door — no persisted format, no wire format, revertable).

Files touched: `crates/macp-storage/src/registry.rs` only (+350/-2), plus the two `plans/` docs.

### 2026-08-30 — Phase 2 (config plumbing), rounds 1-3
Executor + two fresh Opus verify rounds. Gate green throughout: 626 workspace tests, Tier-1 92 (was 90),
8 JWT, 5 Tier-2; fmt + clippy `-D warnings` clean on both the workspace and `integration_tests`.
`cargo semver-checks -p macp-auth` and `-p macp-runtime`: 196 pass / 58 skip, no semver update required —
the two new `pub` fields are additive through the `macp_runtime::security` re-export.

**The finding that mattered — a swap nothing could see.** A verifier transposed the two env values feeding
`resolve_list_sessions_page_sizes` in `from_env`; **the entire suite passed** (626 workspace + 105
integration). `DEFAULT=2 MAX=900` would have resolved to a cap of 2. Every unit test drives the
name-agnostic pure resolver, and the one `from_env` assertion runs with both vars unset, where a swap is
indistinguishable. Mitigated by a named `RawPageSizeEnv` struct — but the fixer then **disproved my own
framing**: transposing the env-var *name strings* still compiles and still passes everything, so the struct
makes the error visible, not impossible. **G1 stays PARTIAL by design; it is closed only by the Phase 3
Tier-1 test**, now recorded as a hard requirement in the plan's Phase 3 section (two *distinct* non-default
values — identical values cannot detect a swap).

**Two CI-hazard catches.** (a) The startup-gate tests as first written would have **hung CI** rather than
failed: with validation removed the binary does not exit, it starts a server, and `Command::output()`
blocks until the child closes its pipes — a mutation check blew a 10-minute timeout. Rewritten around
`spawn` + `try_wait` + a 15s deadline, now failing in ~30s. The two *pre-existing* tests in that file still
use `output()` and carry the same latent hazard (logged in `ASSUMPTIONS.md`, out of scope here).
(b) That helper leaked a raw `Child` holding inherited pipes on its error path; now wrapped in
`TrackedChild`, which needed a new `wait_with_output` on `server_manager.rs`.

**Plan deviation, approved under /drive tier 2:** acceptance criterion 3 revised — `DEFAULT=5000` with
`MAX` unset now **aborts** instead of clamping-and-starting. The guard was asymmetric: the same operator
error aborted loudly when both vars were set, but was clamped silently behind a stdout-only
`tracing::warn!` when only one was. Resolver clamp retained as defence-in-depth for library embedders who
call `from_env()` directly and never reach `validate_env_config` (private to `src/main.rs`).

**Operator-facing defect found in round 2:** `MACP_LIST_SESSIONS_MAX_PAGE_SIZE=50` alone — a plausible
single-knob config — passes validation, then trips the clamp and emits a warning naming
`MACP_LIST_SESSIONS_DEFAULT_PAGE_SIZE`, **a variable the operator never set**. That path had zero coverage,
which is why a verifier mutation disabling the clamp survived all 626 + 92 tests. Round 3 fixes the message
to be provenance-aware and adds the missing assertion.

**Also corrected:** a comment claiming the clamp branch is "unreachable for the binary" (it is reachable via
the single-knob case above), and a comment in the present tense pointing at Tier-1 coverage that does not
exist yet.

**Deferred to Phase 5, confirmed by the verifier:** the two env vars are absent from `README.md`,
`docs/API.md`, `docs/deployment.md` and the `CLAUDE.md` table. Correct to defer while nothing reads them —
must not ship without it.

**Shippability:** accumulate. Confirmed by both rounds — seven crates release in lockstep, the fields have
no reader, and G1's pointer comment resolves only once Phase 3 lands.

### 2026-08-30 — Phase 2 CLOSED: PASS after 3 rounds, committed `9b91fbf`
Round 3 verifier ran **both** mutation shapes and confirmed the new
`clamps_builtin_default_against_a_smaller_explicit_max` test is the *sole* killer under the stronger one
(`default_raw.is_some()`, where `Some("0")` counts as explicit) — the weaker shape was also caught by an
existing test, so only the new test proves the branch. Provenance definition upheld on review: "the value in
hand came from the env var" is the only reading under which `default_source` and `effective_default` agree.
627 workspace tests, Tier-1 92, 8 JWT, 5 Tier-2; `cargo semver-checks -p macp-auth` clean.

**Tier-1 flake observed and dismissed:** one run had `cancel_active_session` fail at `tests/common/mod.rs:38`
("failed to start local MACP runtime") — the lazily-started *shared* server, not an assertion. Passed in
isolation and on three subsequent full runs. Unrelated to page-size code, which never touches that path.
Noting it because it will likely recur in CI and should not be mistaken for a regression from this work.

Phase 2 rounds: 3. Phase 1 rounds: 2. Cross-phase thrash trigger (2 consecutive phases each >2 rounds) is
**armed** — if Phase 3 also needs 3+ rounds, stop and re-examine the plan rather than continuing to grind.

### 2026-08-30 — Phase 3 (codec + paged handler): PASS on round 1, 2 comment fixes
650 workspace tests (was 627), Tier-1 92, 8 JWT, 5 Tier-2; fmt + clippy `-D warnings` clean.
`cargo semver-checks -p macp-runtime`: 196 pass / 58 skip — the codec adds **zero** public API
(everything `pub(crate)`, `mod pagination;` is private).

**The finding of the whole run: the prescribed test suite could not detect the most important bug.**
Deriving `next_page_token` from the *materialized sessions* instead of the *ID list* is observationally
identical unless the registry mutates between the ID scan and the per-ID fetch — a window that never opens
in a sequential unit test. Under that mutation **649 of 650 tests pass**. Real-world effect: if the last
candidate's session is removed inside that window, the cursor either stalls or drops every remaining
session — a silently truncated listing, which is precisely the failure this feature exists to prevent.

The executor wrote an unprescribed test for it,
`list_sessions_cursor_comes_from_the_id_list_not_the_returned_sessions`, and the verifier proved it is
**deterministic rather than a lucky race**: `#[tokio::test]` defaults to current_thread, both futures are
polled by one `tokio::join!` on one task, and `Arc::strong_count == 3` is a genuine yield point because
`get_shared` clones the Arc and drops the map guard *before* `get_session` awaits the session mutex
(the lock-ordering contract at `registry.rs:141-147` is what makes this safe). The spin is bounded at
10,000 with an `assert!`, so a missed interleave fails loudly instead of hanging CI. Verified across
**55 runs, 25 of them under 12 concurrent CPU burners — zero flakes**. It is the single test standing
between this branch and a silently wrong session listing.

**Four more mutations, all caught:** floor removed → the zero-page-size test; length-check moved after the
base64 decode → `decode_rejects_oversized_token_without_decoding`; `has_more` weakened to `>=` → the
terminal-page test; cursor taken from the over-fetched extra ID (a silent one-session drop per page) → 4
tests. Auth ordering is pinned twice, at unit level and through the real gRPC boundary (`tier1_jwt.rs:401`).

**Plan correction discovered here:** D3's "the prefix check covers truncation" is only half true. End-
truncating an encoded token frequently yields a well-formed *shorter* cursor — `base64url("v1:session-000")`
minus 3 chars decodes cleanly to `v1:session-0`; on a UUID seed, 35 of 51 end-truncations produce a valid
shorter cursor and none reproduce the original. Harmless (a cursor is a position, not a handle), but it
means an end-truncated token is **not** a rejection case. Phase 4's AC3 has been corrected to use
prefix-damaging truncations instead.

**Two comment-accuracy fixes in flight** (verifier: "neither warrants a re-verification round"): the floor
comment described a livelock, but the actual failure mode is an empty page with an *empty* token — the
traversal terminates and ListSessions silently returns nothing forever; and three places attribute
front-truncation to a failed prefix check when it actually fails UTF-8 validation first.

**`integration_tests/Cargo.lock` gained exactly one line** (`base64 0.23.1` into `macp-runtime`'s dep list —
an already-present package, no new entry). It must be committed or `--locked` builds in that crate break.

**Hard sequencing constraint restated by the verifier:** `docs/API.md:99` still claims ListSessions
"enumerates metadata for *every* session currently held in the registry" and never mentions the paging
fields. This branch must **not** be PR'd before Phase 5 lands, or the PR ships a user-visible breaking
change alongside docs that contradict it.

### 2026-08-30 — Phases 4 & 5 closed; end-of-plan closeout
- **Phase 4** `06b5d94`, PASS round 1. Tier-1 92→100. Closed the env-binding gap — but only after the
  executor disproved the *prescribed* fix: because the resolver clamps `default = min(D, M)` and startup
  refuses `D > M`, correct and transposed wiring produce an **identical default**, so a `page_size=0` test
  cannot detect a name swap. Detection needs an over-large request with both vars set to *different* values.
  Verified: transposing the names fails exactly one test. A verifier's own mutation (`>` → `>=` on the keyset
  cursor) also proved the "assert set size AND running total" insistence was load-bearing — under it the set
  size, set-equals-created, and ordering assertions **all passed** while the total went 25→32.
- **Phase 5** `d77e9c1`, PASS after 2 rounds. Verifier found four factual defects, three born wrong in the
  phase. The serious one: `docs/API.md` claimed the runtime rejects tokens "not minted by this runtime" —
  it performs **no provenance check at all**, which is deliberate and is exactly what the no-MAC rationale
  rests on. An implementer could have built a client treating the page token as an authenticated capability.
  Also: `README.md` (the copy that *ships*) still said 90 Tier-1 tests while the gitignored `CLAUDE.md` was
  corrected to 100; and a claim that "an upstream issue tracks the RFC correction" when none existed.
- **Upstream issue filed:** spec repo **#76** (RFC-MACP-0006 §3.8 / RFC-MACP-0001 §7 still describe
  ListSessions as unpaginated). Cross-repo *issue only*, as authorized. `docs/API.md` now cites it by number.
- **End-of-plan regression:** workspace **650 passed / 0 failed**; fmt, clippy `-D warnings`, `cargo doc`
  clean. Integration suite run four times: one early run had a single unidentified Tier-1 failure whose name
  was lost to truncated output; **three subsequent full runs were clean** (100 + 8 + 5, 3 ignored). Recorded
  as an unreproduced flake rather than attributed to the known `cancel_active_session` shared-server flake,
  since it was not actually identified.

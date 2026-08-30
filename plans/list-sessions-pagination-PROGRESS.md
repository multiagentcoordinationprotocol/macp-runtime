# PROGRESS — ListSessions pagination

Plan: [`plans/list-sessions-pagination.md`](list-sessions-pagination.md)
Branch: `feat/list-sessions-pagination`, cut from `main` @ `d500910` (2026-08-30)
Model tiering for this run: **Opus plans, Opus executes, fresh Opus verifies** (user override
of `/implement`'s default Sonnet executor; verifier independence preserved).

## Status

| Phase | Title | Status | Verdict | Rounds | Commit |
|-------|-------|--------|---------|--------|--------|
| 0 | Plan + reverify | DONE | FLAWED → patched | 1 | (uncommitted) |
| 1 | Registry keyset primitive | IN PROGRESS | — | — | — |
| 2 | Config plumbing | NOT STARTED | — | — | — |
| 3 | Page-token codec + paged handler | NOT STARTED | — | — | — |
| 4 | Tier-1 integration tests | NOT STARTED | — | — | — |
| 5 | Docs + env-table sync | NOT STARTED | — | — | — |
| 6 | Ship (PR, CI, no merge) | NOT STARTED | — | — | — |

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

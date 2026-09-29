# PROGRESS — session lifecycle entries (follow-on items 9 & 10)

Companion to `plans/session-lifecycle-entries-9-10.md`. Written 2026-09-28.
**Round 3 base correction:** `5e95c4a` is **not an ancestor of `main`** (local
commit squashed into PR #203). Every citation below was re-verified at
`8115e75` (`chore: release v0.8.4 (#202)`, workspace `0.8.4`,
`CURRENT_SEMANTICS_REV = 3`) and holds; diff against `8115e75`.

`/implement` reads this instead of re-scanning the repo.

---

## Headline for the executor

**Item 10 is already shipped** (`7c652b6`, 2026-09-13) — do not re-implement it.
**Item 9 is rejected on the merits** — implementing it would violate
RFC-MACP-0006 §3.2:117 and :122 and would break replay. Read the plan's Context
and Q1 before touching anything. The four phases below are what remains.

---

## PR strategy: two PRs

**PR 1 — "pin the ordinal/delivery contract and make the reader loud"
(Phases 1 + 3).**
Both are protective and additive: tests plus one `tracing::warn!`. Neither
changes what the runtime writes or acknowledges, so both are verifiable by
`cargo test` plus tier 1 alone. Landing them first means PR 2's behavior change
arrives on a suite that already asserts the invariant PR 2 must not disturb —
that is the reason for the split, not tidiness. Phase 3 has no dependency on
Phase 1; they share a PR for packaging only.

**PR 2 — "correct banked_ms and settle the record" (Phases 2 + 4).**
Phase 2 is the only behavior change in the whole plan. Phase 4's docs describe
Phase 2's corrected quantity and close out the backlog reasoning, so they share
one reviewer's context and one release note.

*Why not one PR:* PR 2 is the only thing a reviewer must scrutinize for
behavior. Burying a one-line arithmetic change under ~15 files of test and doc
churn is how a wrong formula ships unreviewed.

*Why not three or four:* Phases 1 and 3 have no independent reviewer value —
Phase 3 is two lines plus a test that sits in the same module as Phase 1's
replay-adjacent work. Phase 4 has no meaning before Phase 2 lands, since it
documents Phase 2's output.

---

## Phases, risk tags, gates

| # | Phase | Risk | PR | Depends on |
|---|---|---|---|---|
| 1 | Pin the ordinal and stream-delivery conformance invariant | **complex** | PR 1 | — |
| 2 | Correct `SessionResumePayload.banked_ms` to the normative quantity | **complex** | PR 2 | 1 |
| 3 | Make the replay reader loud about unrecognized runtime entries | **simple** | PR 1 | — |
| 4 | Settle the record: rustdoc, docs, backlog, upstream issues | **simple** | PR 2 | 1, 2 |

**Risk reasoning (per `/implement`'s rule: complex = establishes a foundation,
crosses a boundary, or is already critical via a one-way door / trust
boundary).**

- **Phase 1 — complex.** Establishes the foundation for every other phase and
  encodes a normative contract. The specific hazard is that an assertion written
  backwards would *bless* the non-conformance while making the suite look
  healthier. Criterion 6 (the mutation proof) is the gate that catches this and
  must be demonstrated, not asserted.
- **Phase 2 — complex.** Changes the value of a field written permanently into
  append-only history; logs already on disk keep the old quantity forever with
  no discriminator. One-way for logs written after it.
- **Phase 3 — simple.** One `tracing::warn!` replacing a silent arm, plus one
  test. No state change, no acceptance decision, no wire behavior. Reversible in
  a commit.
- **Phase 4 — simple.** Documentation, backlog, and two upstream issues. No code
  path changes. (`/implement` may batch the gate across Phases 3 and 4 — they are
  the two simple phases — but note they sit in different PRs.)

---

## Repo map

### The two files the whole plan turns on

| Path | Purpose / what matters here |
|---|---|
| `src/runtime.rs` (3804 L) | Coordination kernel. `make_incoming_entry` `:247`; `make_internal_entry` `:282` (takes `at_ms` — item 10's fix, rustdoc `:266-281`); `publish_accepted_envelope` `:229`; `get_session_envelopes_after` `:198`; `maybe_expire_session` `:317` (clock `:322`, `TtlExpired` `:329`); `synthesize_due_accept` `:721` (the deliberate *opposite* treatment — `make_incoming_entry` `:757`, dedup insert `:764`, publish `:795`); `process_message` `:806` (client boundary `:865`, synthesis `:876`); `cancel_session` `:998` (clock `:1027`, entry `:1032`); `suspend_session` `:1068` (clock `:1087`, entry `:1092`, mutate `:1104`); `resume_session` `:1123` (clock `:1141`, **`banked_ms` bug `:1153-1156`**, rustdoc `:1142-1152`, entry `:1162`, mutate `:1176`); `maybe_compact_log` `:1225` (ordinal count `:1229-1244`); `evict_stale_sessions` `:1572` (registry-before-log ordering: registry remove `:1601` in the guard block `:1598-1603`, log cache `:1605`, stream_bus `:1608` — Round 3 fix, the draft said `:1607` for the log cache) |
| `src/replay.rs` (1817 L) | Session rebuild. `replay_session` `:18`; `try_replay_from_checkpoint` `:36`; `replay_entry` `:85` — `Incoming` arm `:92-138` (**`authorize_sender` `:125`** is why item 9 breaks replay), `Internal` arm `:139-168` (`TtlExpired` `:140`, `SessionCancel` `:145`, `SessionSuspend` `:151`, `SessionResume` `:159`, **silent `_ => {}` `:167`** = Phase 3), `Checkpoint` `:169`; `validate_replay_consistency` `:191-276` (8 fields, no `semantics_rev`); `replay_from_start` `:279` (SessionStart find `:288`, rev binding `:346-349`) |

### Storage layer

| Path | Purpose / what matters here |
|---|---|
| `crates/macp-storage/src/log_store.rs` (244 L) | `EntryKind` `:4-9` — **no serde attrs, no `#[serde(other)]`**, not `#[non_exhaustive]`; adding a variant is a semver major *and* makes old binaries skip whole log lines. `LogEntry` `:11-56` — **not `#[non_exhaustive]`**, public, literal-constructed across crates → adding a field is `constructible_struct_adds_field`; `semantics_rev` `:40-41` and `compacted_incoming_ordinals` `:54-55` both `#[serde(default)]`. Ordinal contract doc `:90-102` (already cites RFC-0006 §3.2). `get_incoming_after` `:103-134`, **the `Incoming`-only filter `:128`**. Only ordinal test `:189` (generic `Internal` entry — the gap Phase 1 closes) |
| `crates/macp-storage/src/registry.rs` (823 L) | `PersistedSession` `:34-80` — **is** `#[non_exhaustive]` (`:35`); `suspension_intervals` `:66-71`, `semantics_rev` `:72-75`, both `#[serde(default)]`. `insert_recovered_session` `:391` |
| `crates/macp-storage/src/storage/recovery.rs` | `recover_session` `:6-24` — folds every non-empty `message_id` into dedup, **kind-blind**; public, **no production caller** (re-export `storage/mod.rs:21`) |
| `crates/macp-storage/src/storage/compaction.rs` | `compact_session_log` `:13-51`; payload is a serialized `PersistedSession`, ordinal base on the enclosing `LogEntry` `:44` |
| `crates/macp-storage/src/storage/{file,rocksdb,redis_backend}.rs` | Per-record deserialize failures are **skipped with a warning, returning `Ok`** (`file.rs:145-155`, `rocksdb.rs:211-221`, `redis_backend.rs:134-141`) — so `MACP_STRICT_RECOVERY` cannot catch a bad log line |

### Core vocabulary

| Path | Purpose / what matters here |
|---|---|
| `crates/macp-core/src/session.rs` (1518 L) | `MAX_SUSPENSION_CYCLES` `:65`; **`CURRENT_SEMANTICS_REV = 3` `:110`** with the revision doc block `:67-109` (the project's source of truth for gating); `Session` `:139-192` (`#[non_exhaustive]`); builder default sets the rev `:232`; `suspend` `:250-257` (**does not touch `ttl_expiry`** — why Phase 2's expression is valid); `resume` `:282-317` (banks `:287`, `:314`); `cancel` `:321-328`; `suspend_cap_exceeded` `:332`; `unsuspended_deadline` `:405` with the under-count invariant `:368-404` (**source #2 at `:377-381`** is why not to add mid-session checkpoints) |
| `crates/macp-core/src/mode.rs` | `MessageContext` `:24-41` — the acceptance clock contract |

### Modes

| Path | Purpose / what matters here |
|---|---|
| `crates/macp-modes/src/mode/mod.rs` | `Mode` trait `:35`; `validate_client_envelope` `:100` (hazard note `:84-99`, loud-stale-reader posture **`:80-82`**); `due_synthetic_envelope` `:162` (the RFC-0001 §7.5 analogy in its doc `:145-150`); **default `authorize_sender` `:169-174`** — rejects a non-participant, i.e. `_runtime` |
| `crates/macp-modes/src/mode/handoff.rs` | `IMPLICIT_ACCEPT_MESSAGE_ID_PREFIX` `:34`; `validate_client_envelope` `:276` (rev-gated `:277`); **`authorize_sender` `:299-308`** (`_ => Forbidden` at `:307`); `due_synthetic_envelope` `:403` |
| `crates/macp-modes/src/mode_registry.rs` | `ModeRef` delegation: `validate_client_envelope` `:700`, `due_synthetic_envelope` `:717` |

### Transport & startup

| Path | Purpose / what matters here |
|---|---|
| `src/server.rs` (3643 L) | `validate_envelope_shape` `:114-152` (inbound only; requires non-empty `message_id`); `process_stream_request` `:349`; `process_subscribe_frame` `:458` (authz `:483-490`, history `:512-521`); `cancel_session` `:925`, `suspend_session` `:987`, `resume_session` `:1049`; `stream_session` `:1154`; **`stream_session_emits_accepted_envelopes_only` `:1945` — name overclaims, reads one envelope**; `subscribe_after_sequence_filters_history` `:2518` |
| `src/main.rs` (670 L) | Startup replay `:313-402`: `replay_session` `:340`, `validate_replay_consistency` `:349-356`, log re-append `:371-374`, registry insert `:376`, **strict abort `:379-385`, skip-on-error `:386-392`**; mismatch metric `:411-418`; background maintenance loop ~`:570` |
| `src/stream_bus.rs` (119 L) | `SessionStreamBus`; capacity 256 `:6`; `subscribe` `:27`; `publish` `:38-46` (**no-op when no channel exists** — subscribe before publishing in tests); `remove_if_unused` `:54` |
| `src/metrics.rs` | `record_session_cancelled` `:90`, `record_session_suspended` `:96`, `record_session_resumed` `:102` |

### Tests to extend (Phase 1 & 2 build on these)

| Path | What is there |
|---|---|
| `tests/integration_mode_lifecycle.rs` | `make_runtime()` `:15`; **`suspend_resume_entries_share_the_session_mutation_clock` `:556-642`** — item 10's regression test; builds the exact fixture Phase 2 needs (`stamp_of` `:589-603`, replay check `:629-641`) |
| `tests/stream_integration.rs` | `make_runtime()` `:13`, `envelope()` `:36`; `stream_receives_accepted_envelopes` `:57` (subscribe-before-start ordering); **`stream_subscribers_see_the_synthetic_envelope_in_order` `:213`** — pins the *opposite* answer for the synthetic accept; handoff fixtures `:171-193` |
| `tests/handoff_implicit_accept_live.rs` (1154 L) | The template file. `make_harness()` `:56` (MemoryBackend → no compaction) vs `make_durable_harness()` `:62`; `assert_log_is_uncompacted` `:95`; **`incoming()` `:216`** (the accepted-ordinal view); `synthetic_of` `:224`; `sleep_past_the_deadline` `:251`; `assert_replay_matches` `:264`; `replay_live_log` `:292`; `seed_from_fixture` `:831` |
| `src/replay.rs` `mod tests` `:395` | `make_registry()` `:403`; `incoming_entry(..)` `:423`; **`internal_entry(message_type, received_at_ms)` `:448`** — takes the type as a parameter, so Phase 3 needs no new fixture; `replay_handles_ttl_expired` `:538`; `replay_handles_session_cancel` `:556`; `replay_consistency_flags_state_and_dedup_divergence` `:971`; suspension fixtures `:1318`, `:1423`; `replay_rebuilds_suspension_intervals_from_the_log` `:1495` |
| `integration_tests/tests/tier1_protocol/test_suspend_resume.rs` | `suspend_resume_lifecycle` `:9`, `suspend_from_non_initiator_rejected` `:100`, `suspend_unknown_session_not_found` `:137`. Requests built inline — **no shared `suspend_session_as` helper exists** |
| `integration_tests/tests/tier1_protocol/test_passive_subscribe.rs` | Canonical stream helpers: `subscribe_frame` `:24`, `envelope_frame` `:32`, `open_stream` `:42`, `next_envelope` `:63`, `next_error_code` `:75` |
| `integration_tests/src/helpers.rs` | `new_session_id` `:22`, `with_sender` `:33`, `envelope` `:44`, `session_start_payload` `:64`, `send_as` `:101`, `get_session_as` `:117`, `cancel_session_as` `:154` |
| `integration_tests/tests/common/mod.rs` | `endpoint()` `:14`, one shared `ServerManager::start()` `:36`, `grpc_client()` `:49`. **Round 3 fix:** this file sets **no** env vars — `MACP_MEMORY_ONLY=1` is the `ServerManager` default at `integration_tests/src/server_manager.rs:69` (in `start_with_env` `:60`; `start()` `:55-57` delegates with empty `extra_env`) |
| `tests/conformance/` + `tests/conformance_loader.rs` | **Vendored from the spec repo and byte-diffed by CI — cannot be extended.** Format only carries client messages, so runtime-originated entries are inexpressible |

### Docs in scope

| Path | What changes |
|---|---|
| `docs/API.md` (400 L) | Phase 4: `StreamSession` `:82-92`; `CancelSession` `:180-190`; `SuspendSession` `:192-202`; `ResumeSession` `:204-212` (Phase 2 also adds `banked_ms` here — the field appears nowhere in `docs/` today) |
| `docs/deployment.md` (409 L) | Phase 2: the `log.jsonl` audit path `:140` — the only surface where the old `banked_ms` was observable |
| `docs/testing.md` (141 L) | Phase 1: the tier-1 coverage sentence `:68` |
| `plans/defer/follow_ons.md` | Phase 4: rewrite item 9 (`:148-178`) as closed-not-a-defect; mark item 10 (`:180-224`) DONE |
| **Deliberately NOT touched** | `docs/architecture.md`, `docs/sdk-guide.md`, `docs/examples.md`, `README.md`, `CLAUDE.md` (`:65-66`, `:74`, `:76`) — their "accepted envelopes" phrasing stays accurate because no entry changes classification. `docs/change-review-phases-a-e.md:221` is **correct** and now has a normative citation; do not "fix" it. `CHANGELOG.md` is release-plz-generated — never hand-edit |

### Normative sources (spec repo, read-only)

`/Users/Shared/multiagentcoordinationprotocol/multiagentcoordinationprotocol/`

| Path | What matters |
|---|---|
| `rfcs/RFC-MACP-0006-transport-bindings.md` | **`:117`** — `SessionSuspend`/`SessionResume` "MUST NOT consume ordinals"; **`:122`** — "MUST NOT deliver an internal annotation on this stream"; `:116` ordinal definition; `:123` ordinal stability; `:125` compaction. Landed `f1489df`, **2026-08-30** |
| `rfcs/RFC-MACP-0001-core.md` | `:269` `SessionCancel` = "terminal annotation", runtime sole emitter (§7.3); `:314` suspend/resume "enter the accepted history" (§7.5, landed `048739d`, **2026-06-22**); **`:318`** the `banked_ms` quantity; `:340-362` §8.3 accepted-history discipline. (`rfcs/RFC-MACP-0001.md` is a 24-line compatibility stub — not canonical) |
| `rfcs/RFC-MACP-0003-determinism.md` | **`:48`** "TTL under suspension" — `banked = deadline − t_s`, and the determinism argument resting on the **event timestamps**, not on `banked_ms` |
| `rfcs/RFC-MACP-0010-handoff-mode.md` | `:71-116` §5.1; **`:95-99`** the "same construction as runtime-emitted SessionSuspend/…" analogy that produced item 9's misreading, plus its wrong §7.5 anchor for `SessionCancel` |
| `~/.cargo/registry/.../macp-proto-0.1.10/proto/macp/v1/core.proto` | `:132-136` `SessionCancelPayload`; `:138-145` `SessionSuspendPayload`; `:147-156` `SessionResumePayload` (`banked_ms` field 3, `:153-155`). No `semantics_rev` field anywhere in any `.proto` |

---

## Facts worth not re-deriving

**Round 3 additions (independent reviewer). Read these before writing Phase 1.**

- **`LogEntry` does NOT implement `PartialEq`.**
  `crates/macp-storage/src/log_store.rs:11` derives only
  `Clone, Debug, serde::Serialize, serde::Deserialize`; `PartialEq` is on
  `EntryKind` (`:4`). So `assert_eq!` on `get_incoming_after`'s
  `Vec<(u64, LogEntry)>` **does not compile**. Compare a
  `Vec<(u64, String)>` projection of `(ordinal, message_id)`, the shape the
  existing test already uses (`log_store.rs:203-205`). Do not derive `PartialEq`
  to work around it — that is a public-API addition inside a tests-only phase.
- **`banked_ms` has zero readers — re-verified Round 3** by grepping the whole
  repo, not a path subset: the only hits outside `plans/` are
  `src/runtime.rs:1142`, `:1150`, `:1152` (comments) and `:1160` (the write).
  Nothing in `docs/`, `README.md` or `CLAUDE.md`.
- **Item 10's regression test pins the invariant; it is not a differential
  proof of the race** — the test says so itself at
  `tests/integration_mode_lifecycle.rs:550-554`. The structural guarantee is the
  injected-clock signature. Phase 4's backlog rewrite must not overclaim it.
- **RFC-0006 §3.2's two clauses have different scopes.** `:117` (no ordinals) is
  unrestricted; `:122` (no delivery) sits inside `#### Passive Session
  Subscription` (`:98-142`) and is textually scoped to "a subscribe stream". The
  live-`StreamSession` half of the invariant rests on `:117` + `:120`'s counting
  argument. Cite accordingly in the new tests' doc comments.
- **Do not hardcode a release version** in the Phase 2 rustdoc. `0.8.4` is
  released; release-plz computes the next number at release time.

- `CURRENT_SEMANTICS_REV = 3` (`crates/macp-core/src/session.rs:110`), released
  in 0.8.3. Rev 0/1/2 = handoff; rev 3 = multi_round `Contribute` tie-break. Any
  new gate is rev 4. **No `==` comparison on the field exists anywhere**, which
  is what keeps bumps additive.
- `semantics_rev` is per-session, bound at SessionStart via the builder default
  (`session.rs:232`), stamped onto the SessionStart log entry
  (`src/runtime.rs:501`, `:573`), restored by `src/replay.rs:349` and
  `registry.rs:164`. **Not on the wire** — no proto field, so a client can
  neither set nor observe it, and a rev bump needs no proto/SDK coordination.
- Revision gating is **structural**: replay rebuilds the `Session` at the
  recorded rev and re-runs ordinary mode dispatch, so gates fire again at replay
  time. `src/replay.rs` branches on the rev nowhere.
- `banked_ms` has **zero readers** workspace-wide (`grep -rn banked_ms` over
  `src crates tests integration_tests benches` → the write at
  `src/runtime.rs:1160` and its own comment only) and **zero** hits in `docs/`,
  `README.md`, `CLAUDE.md`.
- Suspend/resume/cancel entries are appended **before** the session mutation
  (`:1099-1104`, `:1169-1176`, `:1039-1046`). For resume this is **load-bearing**:
  the cap-exceeded arm (`:1191-1219`) appends no `TtlExpired`, so replay
  re-derives the force-expiry by re-running `session.resume(at)` off the recorded
  `SessionResume` entry (`src/replay.rs:159-166`). Do not reorder.
- `recover_session` is public, kind-blind, and has no production caller.
- **Compaction forks on the backend, and Phase 1's cancel leg depends on it.**
  `StorageBackend::replace_log` has a **default** impl returning
  `Err(Unsupported)` (`crates/macp-storage/src/storage/mod.rs:42-47`);
  `MemoryBackend` does not override it, `FileBackend` (`file.rs:159`),
  `rocksdb` (`:226`) and `redis` (`:144`) do. So on `MemoryBackend` a terminal
  `cancel_session` → `maybe_compact_log` returns false → `force_insert_checkpoint`
  **appends** a `Checkpoint` with `compacted_incoming_ordinals: 0`
  (`src/runtime.rs:1049`, `:1299`) → the log grows by 2, ordinals unchanged. On
  `FileBackend` compaction succeeds → `replace_session_log(sid, vec![checkpoint])`
  (`:1257-1259`) → the in-memory log becomes one checkpoint with base `N` →
  `get_incoming_after(.., 0)` returns **`Err(N)`**
  (`crates/macp-storage/src/log_store.rs:123-125`). This is why
  `tests/handoff_implicit_accept_live.rs` keeps two harnesses (`:56` vs `:62`)
  and the `assert_log_is_uncompacted` guard (`:95`).
- `get_incoming_after`'s base is a `.max()` over Checkpoint entries
  (`log_store.rs:117-122`), so a force-inserted mid-life checkpoint recording `0`
  cannot reset a base set by an earlier real compaction.
- `publish_accepted_envelope` has exactly **three** call sites —
  `src/runtime.rs:622` (`process_session_start`), `:795`
  (`synthesize_due_accept`), `:943` (`process_message`). None in
  `suspend_session` / `resume_session` / `cancel_session`, which is why
  `banked_ms` is not client-visible.
- `MACP_MEMORY_ONLY=1` selects `MemoryBackend` (`src/main.rs:261-262`; the env
  read is `:255`), which is the tier-1 default — set at
  `integration_tests/src/server_manager.rs:69`, **not** in
  `tests/common/mod.rs` (Round 3 fix). `test_persistence_replay.rs:22` and
  `test_backends.rs:28` override it to `0`.
- `integration_tests/` is a separate cargo workspace with its own `Cargo.lock`
  (guarded by `cargo metadata --locked` in the `integration` job) and is covered
  by **neither** the `fmt` nor the `clippy` CI gate (follow-on 19). No phase here
  changes a dependency, so no lock regeneration is needed; Phase 1 adds a file
  there and must be formatted by hand:
  `cargo fmt --manifest-path integration_tests/Cargo.toml --all`.

---

## Commands

```bash
cargo build
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
cargo fmt --manifest-path integration_tests/Cargo.toml --all -- --check

# Tier 1 (Phase 1's third level)
cargo build
cd integration_tests && MACP_TEST_BINARY=../target/debug/macp-runtime \
  cargo test -- --test-threads=1
# or: make test-integration-grpc
```

Phase 1 criterion 6 (the mutation proof) — run both directions and record the
failures in the commit body:

```bash
# flip src/runtime.rs:295 to EntryKind::Incoming, then:
cargo test --workspace 2>&1 | grep -E '^(test .* FAILED|failures:)'
# ... and in integration_tests/. Then restore and confirm green.
```

---

## Assumptions to log (`/implement` → `ASSUMPTIONS.md`, all `UNCONFIRMED`)

| # | Assumption | Plan ref |
|---|---|---|
| A1 | Item 9 is rejected: RFC-MACP-0006 §3.2:117/:122 require the current `Internal` treatment, and they postdate the §7.5 clause item 9 relied on. Flagged for user review because it reverses the brief. | Q1 |
| A2 | `SessionCancel` is an internal annotation (no ordinal, no delivery) — defensible default over a real spec gap; upstream issue filed in Phase 4. | Q2 |
| A3 | Replay continues to ignore `banked_ms` and re-derives banking from the two entry timestamps, even once Phase 2 makes the field correct. | Q3 |
| A4 | Correcting `banked_ms` is `fix(runtime)` without `!` — zero readers, zero doc mentions, not deliverable to clients. Becomes `!` if an external `log.jsonl` consumer exists. | Q4 |
| A5 | `src/server.rs:1945`'s overclaiming test name is left as-is to keep Phase 1's diff auditable. | Q5 |

## Phase log

| Phase | Status | Commit | Notes |
|---|---|---|---|
| 1 | DONE | `c173c9e` | Opus verifier, round 1, PASS. See checkpoint below. |
| 2 | DONE | (pending — committing next) | Opus verifier, round 1, PASS, no gaps. See checkpoint below. |
| 3 | DONE | `0efa11a` | Opus verifier, round 1, PASS, no gaps. See checkpoint below. |
| 4 | TODO | — | Record the two spec-issue URLs here |

**Execution order note:** phases run 1 → 3 → 2 → 4, not the plan's numeric 1-2-3-4
order — Phase 3 explicitly says "sequenced after Phase 1 only for PR packaging,"
and the PR strategy groups {1,3} into PR 1 and {2,4} into PR 2. Finishing PR 1's
both phases before starting Phase 2 lets PR 1 ship (per `/ship`) as a complete,
independently-mergeable unit ahead of PR 2's behavior change, which is the whole
point of the two-PR split.

### Checkpoint — Phase 1 (2026-09-29)

- **Verdict:** PASS, round 1, fresh Opus subagent (not Fable — no genuine judgment
  fork per the plan's Q1 reasoning, just verbatim spec citations to re-check).
- **Environment note (repo-wide, not phase-specific):** the default SDK on this
  machine is broken (`MacOSX27.0.sdk`'s `.tbd` files fail `tapi` parsing at link
  time). Every local `cargo` invocation needs
  `RUSTC_WRAPPER="" SDKROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX26.sdk`
  — already documented in `plans/spec-drift-catchup-169-170.md` and others; adding
  it here too since this phase is the first to need a build in this session.
- **Files touched:** `tests/integration_mode_lifecycle.rs` (+2 tests: ordinal
  accounting for suspend/resume incl. the restart round-trip, and cancel),
  `tests/stream_integration.rs` (+1 test: live-stream non-delivery),
  `integration_tests/tests/tier1_protocol/test_suspend_resume.rs` (+1 test + local
  StreamSession plumbing helpers), `docs/testing.md` (one sentence). No `crates/`
  or `src/` changes — this phase is tests-only, as specified.
- **Verification performed:** full workspace suite, tier-1 gRPC suite (128 tests),
  `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all --
  --check` — all clean, independently re-run by the verifier. The mutation proof
  (criterion 6) was run twice: the plan's stated `EntryKind::Internal → Incoming`
  mutation (reds levels 1 and 3), plus a second, level-2-specific mutation (an
  explicit `publish_accepted_envelope` call added to `suspend_session`, reds level
  2) — see the Round 4 correction now in `plans/session-lifecycle-entries-9-10.md`
  under Phase 1, and the two new ASSUMPTIONS.md entries (Q1, Q2) for the premises
  these tests encode.
- **Gap found and closed:** criterion 6's "one mutation reds all three levels"
  claim doesn't hold for level 2, for a structural reason (live delivery is gated
  by explicit `publish_accepted_envelope` call sites, not by `entry_kind`). Fixed
  by recording the correction inline in the plan (not a code or test defect — the
  tests themselves needed no changes).
- **Known pre-existing, out-of-scope fmt drift:** `cargo fmt --manifest-path
  integration_tests/Cargo.toml --all -- --check` fails on `test_policy_registry.rs`
  and `test_session_lifecycle.rs` — confirmed via `git stash` (by the verifier) to
  exist identically on base commit `8115e75`, before this diff. Not touched by
  this plan; not this phase's to fix.
- **Next:** Phase 3 (same PR, per PR strategy), then `/ship` PR 1.

### Checkpoint — Phase 3 (2026-09-29)

- **Verdict:** PASS, round 1, no gaps, fresh Opus subagent.
- **Files touched:** `src/replay.rs` only — the `_ => {}` catch-all in
  `replay_entry`'s `EntryKind::Internal` match replaced with a `tracing::warn!`
  carrying `session_id`, `message_type`, `received_at_ms`, plus a new test
  `replay_warns_but_continues_on_unrecognized_internal_entry`. No `Err`, no
  metrics counter, no rate limiting — all three explicitly rejected in the plan
  and confirmed absent by the verifier.
- **Verification performed:** `cargo test --lib replay::` (31/31), full
  workspace suite, tier-1 gRPC suite (128 tests, unaffected as expected —
  no wire behavior changed), `cargo clippy --workspace --all-targets -- -D
  warnings` (re-forced with a `touch` to rule out a stale-cache false-clean),
  `cargo fmt --all -- --check` — all clean, independently re-run by the
  verifier. `replay_handles_ttl_expired`/`replay_handles_session_cancel`
  confirmed unmodified and still passing.
- **Gaps:** none.
- **Next:** both PR 1 phases (1, 3) are DONE — hand off to `/ship` for PR 1,
  then start Phase 2 (PR 2) once PR 1 has merged.
pushed feat/session-lifecycle-ordinal-conformance b4351ee8d06e367dc6698f828f3c454003c8b776
PR #206 opened: https://github.com/multiagentcoordinationprotocol/macp-runtime/pull/206
merged #206 (27220e8)

### Checkpoint — Phase 2 (2026-09-29)

- **Verdict:** PASS, round 1, no gaps, fresh Opus subagent.
- **Branch:** `fix/session-resume-banked-ms`, created off `main` at `27220e8` (PR
  #206's merge commit).
- **Files touched:** `src/runtime.rs` (`resume_session`'s `banked_ms` expression
  and its rustdoc), `tests/integration_mode_lifecycle.rs` (+4 tests: criterion
  3+4 combined, criterion 5's cycle-shrink, and the two edge cases), `docs/API.md`
  (`ResumeSession` section), `docs/deployment.md` (`log.jsonl` audit-path note).
- **Verification performed:** full workspace suite (`cargo test --workspace
  --no-fail-fast`, all 36 suites green, all 4 new tests pass), tier-1 + tier-2
  integration suite (128 + 8 JWT + 5 Rig tests, all green, tier-3 ignored as
  expected — no `OPENAI_API_KEY`), `cargo clippy --workspace --all-targets`
  (force-rechecked via `cargo clean -p macp-runtime` first, per the Phase 1/3
  stale-cache lesson — clean), `cargo fmt --all -- --check` (found 2 formatting
  nits in the new tests, fixed with `cargo fmt`, re-verified clean) — all
  independently re-run by the verifier. Verifier also independently confirmed
  via `grep -rn banked_ms` over `src crates tests integration_tests benches docs
  README.md CLAUDE.md` that the field has zero readers anywhere in the
  workspace (only the write site and this phase's own comments/tests/docs), and
  independently read `src/replay.rs`'s `SessionResume` arm to confirm replay
  never decodes the payload — corroborating the rustdoc's "informational only"
  claim rather than trusting it.
- **Gaps:** none. One non-blocking observation from the verifier (this table
  showing Phase 2 as TODO mid-verification) — resolved by this checkpoint.
- **Assumptions logged:** Q3 (replay does not consume the corrected `banked_ms`)
  and Q4 (`fix(runtime):` without `!`, on measured zero-reader exposure) — both
  `UNCONFIRMED` in `ASSUMPTIONS.md`, per the plan's explicit instruction on Q3.
- **Next:** Phase 4 (same PR, per PR strategy) — rustdoc cross-links, `docs/`
  sweep, backlog rewrite, upstream spec issues — then `/ship` PR 2.

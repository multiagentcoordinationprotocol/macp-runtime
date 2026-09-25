# Plan: fix `parse_contribute_value`'s canonical-proto/JSON collision (issue #192)

## Context

`parse_contribute_value` (`crates/macp-modes/src/mode/multi_round.rs:63-78`) is the sole
decoder for `ext.multi_round.v1`'s `Contribute` message payload. It supports two wire
formats for backward-compat reasons that are load-bearing, not cosmetic: legacy JSON
(`{"value":"..."}`, the only format that existed before proto encoding was added) and
canonical protobuf (`macp.modes.multi_round.v1.ContributePayload { string value = 1; }`,
defined at the vendored `macp-proto` crate's
`proto/macp/modes/multi_round/v1/multi_round.proto:16-18` — confirmed single-field, no other
fields, no `oneof`, no repeated fields). It tries JSON first, unconditionally, "permanently"
per its own doc comment (`:50-56`), because RFC-MACP-0003 §1 requires that bytes accepted
before the proto encoding existed keep decoding identically forever.

The docstring's safety argument (`:55-56`) — "A proto payload never parses as a JSON
object, so this order is deterministic and costs proto senders one failed JSON parse" — is
false. The protobuf tag byte for field 1 (`0x0A`) and, at three specific value byte-lengths
(13, 32, 123), the length-varint byte that follows it, are themselves insignificant JSON
whitespace (`0x09/0x0A/0x0D/0x20`) or the literal `{` (`0x7B` = 123 decimal) to `serde_json`.
When that happens, `serde_json`'s leading-whitespace skip exposes the *value's own bytes* to
the parser, and if the value itself happens to be shaped like `{"value":"<string>"}`, a
canonical, correctly-encoded proto `Contribute` silently decodes to the WRONG string — a
substring of the true value — with no error anywhere. This was verified directly this
session (byte-level derivation below, independent of the issue's own reproducer) and is
exploitable via `parse_contribute_value` alone, no network access needed:

```
value  = {"value":"x"}                # 13 bytes, chosen so serde_json is fooled
proto  = 0x0A 0x0D {"value":"x"}       # 15 bytes: tag=field1/LEN, len=13=0x0D (JSON CR)
```
`0x0D` is JSON whitespace, so `serde_json::from_str::<ContributeJson>` skips both the `0x0A`
tag byte and the `0x0D` length byte and parses the remaining 13 bytes as
`{"value":"x"}`, returning `"x"` instead of the true 13-byte value.

**Rust's stricter typed JSON deserialization already closes one of the two mechanisms the
sibling Python SDK had to handle.** `ContributeJson` (`multi_round.rs:41-44`) is a
`#[derive(Deserialize)] struct { value: String }` — `serde_json::from_str::<ContributeJson>`
fails outright on a bare JSON number, string, or array (the "significant-JSON-opener"
mechanism that hit `macp-sdk-python`'s untyped `json.loads` at lengths 34, 45, 48, 49-57, 91).
Only the "length byte is itself whitespace/`{`" mechanism reaches this function, and only at
lengths where the *value's own bytes*, once exposed, also happen to satisfy the
`{value: String}` shape — exactly 13, 32, 123 in the 0-127 single-byte-varint range (verified
below in "Repo map" / Phase 1's Tests). Lengths 9 and 10 also land on JSON-whitespace-valued
length bytes but are physically too short to contain a minimal `{"value":""}`-shaped object
(12 bytes), so they cannot actually collide — this repo does not need to carry Python's
length-9/10 test cases, only 13/32/123.

**A structurally identical bug was already found and fixed in `macp-sdk-python`** (issue #69,
PR #77, commit `3403034`, read directly from
`/Users/Shared/multiagentcoordinationprotocol/macp-sdk-python` this session). The fix is a
canonicality tie-break in `_decode_json_first_then_proto`/`_is_canonical_proto`
(`src/macp_sdk/proto_registry.py`): trust a successful JSON parse only if the same bytes do
NOT also round-trip byte-identically through `ContributePayload.ParseFromString` →
`DiscardUnknownFields()` → `SerializeToString()`. That fix has one deliberately-accepted,
explicitly-priced residual on the *reverse* direction (legacy JSON misread as proto): a
payload starting with literal byte `0x0A` whose remainder happens to form a complete,
well-formed proto field-1 string. `macp-sdk-python/ASSUMPTIONS.md`'s "Contribute payload
canonicality tie-break (option A vs. B)" entry (lines 117-167) prices this precisely: no known
encoder, including that SDK's own, ever emits a leading-`0x0A`-prefixed legacy JSON payload,
so the residual is real but not realistically reachable by an honest sender.

**Why this is NOT simply "port the Python fix" in macp-runtime, and why that matters more
here than it did there — narrowed to its real scope, not overstated.** `macp-sdk-python` is
a client library; this runtime is the durable, replay-forever authority the client trusts.
Verified directly this session (`src/replay.rs:92-138`, `replay_entry`'s `EntryKind::Incoming`
branch): on every session replay — which happens on every server restart unless a checkpoint
or compaction covers the entry — the runtime rebuilds `Envelope.payload =
entry.raw_payload.clone()` (the *original* accepted bytes, byte-for-byte) and re-invokes
`mode.on_message_at(session, &replay_env, &ctx)`, which for `multi_round` `Contribute`
messages calls `handle_contribute` → `parse_contribute_value` again, against the exact
original bytes. `handle_commitment` (`multi_round.rs:191-222`) does not persist a stored
resolution either — it **recomputes** `ResolutionPayload` fresh from `state.contributions` on
every `on_message_at` call, replay included.

However, **terminal (resolved/cancelled/expired) sessions are largely protected already**,
which this plan's first draft did not account for and an independent review pass caught:
`Runtime::maybe_compact_log` (`src/runtime.rs:1225-1271`) is called unconditionally whenever a
session transitions to terminal (`src/runtime.rs:937,1048,1376`) — **not** gated by
`MACP_CHECKPOINT_INTERVAL` (confirmed by this repo's own `CLAUDE.md` env-var table: "Terminal-
session log compaction is unconditional and NOT gated by this"). On success,
`compact_session_log` (`crates/macp-storage/src/storage/compaction.rs:13-50`) **replaces the
entire on-disk and in-memory log with a single `Checkpoint` entry** holding the serialized
`PersistedSession` (`src/runtime.rs:1245-1260`) — so a *resolved* multi_round session's
original `Contribute` bytes are gone from what any future replay reads; its already-computed
(possibly wrong) `converged_value` is exactly what a future replay reproduces either way, via
the checkpoint's snapshot, not via re-decoding raw bytes.

The real, still-genuine exposure is narrower than "any already-existing session, forever":
**(a) sessions that are still `Open` (not yet resolved) when this ships**, whose logs still
hold the raw `Contribute` bytes and will be replayed via `replay_entry`'s full-decode path on
every restart until they terminate — bounded by `MAX_TTL_MS` (24 hours,
`crates/macp-core/src/session.rs:21`) in the worst case; and **(b) any session, terminal or
not, where compaction was skipped** — it is explicitly best-effort (the `Err` arm of
`compact_session_log` logs "log compaction skipped (backend may not support it)" and leaves
the full log in place, `src/runtime.rs:1262-1269`). That is a real, non-hypothetical window —
not a "forever" one — and it is independently sufficient to justify Option A below:
`CONTRIBUTING.md:44-47`'s legacy-log-fixture rule applies regardless of how large the exposure
window is, and Option A's marginal implementation cost over an unconditional fix is small
enough (one constant, one comparison, one fixture) that narrowing the risk estimate doesn't
change the recommended approach — it only corrects how that recommendation should be
justified.

**This repo already has a designed, precedented mechanism for exactly this class of
problem**, and it is a *written* project rule, not an inferred convention:
`Session::semantics_rev` / `CURRENT_SEMANTICS_REV` (`crates/macp-core/src/session.rs:67-99`,
currently `2`), used twice already for Handoff-mode acceptance-time behavior changes
(RFC-MACP-0010 §5.1). `CONTRIBUTING.md:44-47`: "Changes affecting message acceptance or
replay need a regression test, and — if they change semantics of persisted histories — a
legacy-log fixture proving old logs still replay under their original semantics (see
`Session::semantics_rev`)." `session` is already in scope at the one call site
(`handle_contribute`, `multi_round.rs:163-189`, `session: &Session`) but is not threaded into
`parse_contribute_value` today. This plan uses that mechanism (Phase 1) rather than treating
the fix as an unconditional global change — see "Long-term posture" for why this converts
what would otherwise be a genuine one-way door into a safely reversible, well-precedented
change, and "Open questions" for the critical-decision consult (Fable) that confirmed this
before any code was written.

**One resource cited by this function's own doc comment does not exist at the path given —
the issue itself never mentions this path.** This function's own doc comment
(`multi_round.rs:58-60`) says the manifest lives at `schemas/parity/contract.json` (confirmed:
the issue's full body, pulled via `gh issue view 192`, contains zero occurrences of "parity",
"contract.json", or "schemas/parity" — the stale reference is this file's own, not the
issue's). In this repo it is vendored at **`tests/parity/contract.json`**
(confirmed: `find . -iname contract.json` finds no `schemas/` directory in this repo at all,
matching `CLAUDE.md`'s own statement that "there is no `schemas/` directory in this
repository"). The canonical, normative copy is `schemas/parity/contract.json` in the sibling
spec repo (`../multiagentcoordinationprotocol/`); `tests/parity/SOURCE.md` documents that this
repo's copy must stay byte-identical to it (enforced by `.github/workflows/ci.yml`'s
`conformance-oracle` job, `check_dir`, pinned to `SPEC_REV`). This distinction matters
directly for Phase 2 below: this repo cannot locally add new `contribute_payload` test
vectors to `tests/parity/contract.json` without first getting them accepted upstream, or the
byte-identity CI gate breaks.

**The canonical parity contract already documents this exact gap as open, unfixed work.**
`schemas/parity/README.md`'s "Open items" section (spec repo, read directly this session):
*"`Contribute` payload decode on non-canonical inputs. 'No known drift' is true only on the
canonical vectors this manifest pins. On non-canonical inputs (leading whitespace, a
non-string `value`, an empty payload) the three implementations already disagree... Tracked
as a follow-up issue per SDK."* None of the four existing `contribute_payload.vectors` in
`schemas/parity/contract.json` hit the 13/32/123 collision (their byte-lengths are 6, 5, 130,
127 — confirmed by decoding each vector's `protobuf_hex` this session). Closing this properly
is cross-repo work — see Phase 2.

## Phases

### Phase 1 — Canonicality tie-break, `semantics_rev`-gated, plus the docstring fix

**Status:** DONE (2026-09-25) — implemented as planned, no divergence. `CURRENT_SEMANTICS_REV`
bumped 2 → 3 with the rev-3 doc bullet; `parse_contribute_value` gained the `semantics_rev`
parameter and the round-trip tie-break exactly per the code sketch; the false docstring claim
rewritten; all three `tests/parity_contract.rs` call sites updated to pass
`CURRENT_SEMANTICS_REV`, `tests/parity/contract.json` untouched. All 12 acceptance criteria
covered by new tests in `crates/macp-modes/src/mode/multi_round.rs` (11 tests: the 6
length-13/32/123 rev2-vs-rev3 pairs, the reverse-direction-residual test, the
non-canonical-proto regression pin, the exhaustive 1..=300×5-shape differential sweep, and the
two end-to-end `handle_contribute` tests) plus 2 replay-layer legacy-log fixtures in
`src/replay.rs`. Verification could not run locally (see toolchain note below); verified via
CI on the PR instead — see `PROGRESS.md`.

**Delivers:** `parse_contribute_value` stops silently mis-decoding canonical proto
`Contribute` payloads at value lengths 13/32/123 for any **new** session (one whose
`SessionStart` is accepted after this ships), while every **existing** session's replay
outcome — including ones that already hit this collision — stays byte-for-byte identical to
what it produced at original acceptance, forever. The false safety claim in the function's
doc comment is corrected.

**Depends on:** nothing.

**Files:**
- `crates/macp-core/src/session.rs` — bump `CURRENT_SEMANTICS_REV` (`:99`) from `2` to `3`;
  extend the doc block (`:67-98`) with a new bullet for revision 3, matching the existing
  style for 0/1/2: *"3 — `ext.multi_round.v1` `Contribute` decode gains a canonical-proto
  tie-break (macp-runtime issue #192): a successful legacy-JSON parse is trusted only when
  the same bytes do NOT also round-trip byte-identically through the canonical
  `ContributePayload` proto encoding. Revisions 0-2 keep the unconditional JSON-first decode,
  including its known collision at value lengths 13/32/123 — byte-for-byte faithful to how
  those sessions were originally accepted. Unlike revisions 0-2 (all Handoff-specific), this
  revision changes no Handoff, Quorum, Proposal, Task, or Decision behavior — every existing
  gate on this field is `>= 2` or `<= 1` (never `== 2`), so bumping the shared counter to 3 is
  additive for every other consumer."*
- `crates/macp-modes/src/mode/multi_round.rs`:
  - `parse_contribute_value` (`:63-78`) — add a `semantics_rev: u32` parameter (its own doc
    comment already says "Not a stability promise," `:61`, so a signature change needs no
    external-compat carve-out); decode proto once and reuse the result for both the
    tie-break and the final fallback (avoids two near-diverging decode paths — the two-function
    split considered and rejected below). Sketch (not final code — implementer verifies
    against the acceptance criteria, not this literal text):
    ```rust
    pub fn parse_contribute_value(payload: &[u8], semantics_rev: u32) -> Result<String, MacpError> {
        if payload.is_empty() {
            return Err(MacpError::InvalidPayload);
        }
        let proto_decoded =
            <macp_pb::multi_round_pb::ContributePayload as prost::Message>::decode(payload).ok();
        if let Ok(text) = std::str::from_utf8(payload) {
            if let Ok(c) = serde_json::from_str::<ContributeJson>(text) {
                let trust_proto = semantics_rev >= 3
                    && proto_decoded.as_ref().is_some_and(|m| m.encode_to_vec() == payload);
                if !trust_proto {
                    return Ok(c.value);
                }
                tracing::debug!(
                    value_len = payload.len(),
                    "Contribute payload is both valid JSON and canonical proto; preferring proto (issue #192 tie-break)"
                );
            }
        }
        proto_decoded.map(|c| c.value).ok_or(MacpError::InvalidPayload)
    }
    ```
    Note for the implementer: `multi_round.rs` currently imports `prost::Message` only inside
    `#[cfg(test)] mod tests` (`:230`); this sketch's `m.encode_to_vec()` needs that trait in
    scope at module level too (add `use prost::Message;` near the top of the file, or call
    `prost::Message::encode_to_vec(m)` fully-qualified instead).

    The `tracing::debug!` call is new observability, not incidental — see "Enterprise
    concerns." `tracing` is already a `macp-modes` dependency and already used this way
    elsewhere in the crate (`crates/macp-modes/src/mode/util.rs:54,61,143,152`,
    `crates/macp-modes/src/mode/quorum.rs:398`).
  - `handle_contribute` (`:163-189`) — change the call at `:174` from
    `parse_contribute_value(&env.payload)` to
    `parse_contribute_value(&env.payload, session.semantics_rev)`.
  - Doc comment `:46-61` — rewrite `:55-56`'s false claim. Replace "A proto payload never
    parses as a JSON object, so this order is deterministic and costs proto senders one
    failed JSON parse" with an accurate statement: canonical proto CAN parse as JSON at value
    lengths 13/32/123 (cite issue #192), which is why a canonicality tie-break is applied at
    `semantics_rev >= 3`, and why the docstring must not claim unconditional determinism.
  - `#[cfg(test)] mod tests` (`:225-788`) — new tests, see Tests below. Existing helpers
    `contribute_env`/`contribute_env_json`/`contribute_env_with_payload` (`:253-279`) are
    reused as-is; every existing call to `parse_contribute_value` and to
    `mode.on_message`/`handle_contribute` via `session_with_state` continues to work because
    `session_with_state` (`:306-311`) builds via `base_session()` → `Session::builder(...)
    .build()`, which defaults `semantics_rev` to `CURRENT_SEMANTICS_REV` (confirmed:
    `crates/macp-core/src/session.rs:187-225`, `Session::builder`'s initial struct literal at
    `:193-224` sets `semantics_rev: CURRENT_SEMANTICS_REV` at `:221`) — so all of this file's
    *existing*
    tests automatically run at the new rev 3 and continue to pass unchanged, since none of
    them hit the collision lengths.
- `tests/parity_contract.rs` — the only external call site of `parse_contribute_value`
  outside `macp-modes` and its own tests (confirmed via a workspace-wide grep this session:
  only `crates/macp-modes/src/mode/multi_round.rs` and this file reference the symbol). Three
  call sites need the new argument: `:467` (protobuf decode), `:477` (legacy JSON decode),
  `:538` (empty-payload rejection check). Import `macp_core::session::CURRENT_SEMANTICS_REV`
  alongside the existing `DEFAULT_CONFIGURATION_VERSION`/`DEFAULT_MODE_VERSION` import
  (`:38`) and pass it at all three sites — asserting against the *current* constant, not a
  hardcoded `3`, matches this file's own stated philosophy ("asserting the manifest's content
  against macp-runtime's real behavior," `:34`) and means the test keeps working unmodified
  if a future, unrelated revision bump happens. **Do not touch `tests/parity/contract.json`
  itself in this phase** — it must stay byte-identical to the spec-repo canonical copy
  (`tests/parity/SOURCE.md`); the existing 4 vectors are all non-colliding lengths (6, 5,
  130, 127 bytes) and their assertions are unaffected by this fix either way.

**Approach:** `semantics_rev`-gating (matching this repo's own established mechanism) over
an unconditional global change, for the reasons in Context above — this is the one piece of
this plan that was routed through a scoped Fable consult before being written here rather
than decided unilaterally; see "Open questions" for the consult's verdict and the two
alternatives it rejected (a bare global fix; a `MultiRoundState`-local flag instead of the
session-wide revision). The `tracing::debug!` call when the tie-break actually overrides a
JSON reading is included because this is exactly the kind of silent-until-now corruption class
an operator should be able to observe in production traffic going forward, not just prove
absent in tests, and it costs one log line using an already-established crate dependency and
call pattern.

**Edge cases & failure modes:**
- **In-flight sessions across the upgrade.** A session whose `SessionStart` was accepted
  before this ships (bound `semantics_rev <= 2`) keeps the pre-fix decode for the rest of its
  lifetime, including `Contribute` messages received *after* the upgrade — `semantics_rev` is
  fixed once at `SessionStart` and never changes mid-session (confirmed: nothing in
  `replay_entry` or the live acceptance path in `src/runtime.rs` reassigns
  `session.semantics_rev` after construction). This is intentional, not a gap: it is the same
  trade the two prior handoff revision bumps already made, and it is bounded by the session's
  own TTL. Pin with a test (see Tests).
- **Checkpointed and compacted sessions.** `try_replay_from_checkpoint` (`src/replay.rs:36-82`)
  restores a `PersistedSession` snapshot directly and only replays entries *after* the
  checkpoint — a colliding `Contribute` that predates the checkpoint is never re-decoded
  post-fix. This is the **dominant** protection in practice, not a narrow edge case: any
  session that goes terminal is unconditionally compacted to exactly one `Checkpoint` entry
  (`Runtime::maybe_compact_log`, `src/runtime.rs:1225-1271`, called at `:937,1048,1376`, not
  gated by `MACP_CHECKPOINT_INTERVAL` — see Context), so a *resolved* multi_round session's raw
  `Contribute` bytes are gone from every future replay. The real exposure is `Open` sessions
  (bounded by `MAX_TTL_MS` = 24h) and any session where compaction was skipped (best-effort;
  `src/runtime.rs:1262-1269` logs and continues on failure, leaving the full log — and the
  re-decode exposure — in place).
- **`MultiRoundState`'s JSON encoding.** `encode_mode_state`/`decode_mode_state`
  (`crates/macp-modes/src/mode/util.rs`) are plain `serde_json::to_vec`/`from_slice` over the
  state struct; a `{"value":"..."}`-shaped contribution is stored as a properly-escaped JSON
  string value inside `contributions: BTreeMap<String, String>` and round-trips unambiguously
  regardless of what the string's own content looks like — no interaction with this fix.
- **The reverse-direction residual**, mirroring the Python fix exactly: at `semantics_rev >=
  3`, a payload starting with literal byte `0x0A` whose remainder is *also* a complete,
  well-formed proto field-1 string will be read as proto even if a sender genuinely meant it
  as legacy JSON with an incidental leading-whitespace byte. Concretely constructible (derived
  and arithmetic-checked this session, not copied from Python): let `content = "a".repeat(112)`,
  `json = format!(r#"{{"value":"{}"}}"#, content)` (124 bytes, so its own length as a
  single-byte varint value minus the leading `{` is 123 = `0x7B`), and `payload =
  [0x0A].iter().chain(json.as_bytes()).copied().collect::<Vec<u8>>()` (125 bytes). Read as
  JSON with the leading `0x0A` skipped as insignificant whitespace, this is
  `{"value":"aaa...a"}` (112 a's) — a legitimate legacy reading. Read as proto, `payload[0] =
  0x0A` (tag), `payload[1] = 0x7B = 123` (single-byte length varint), and the following 123
  bytes (`payload[2..125]`, which is `json.as_bytes()[1..]`) form a valid UTF-8 string that
  re-encodes byte-identically — so the tie-break prefers proto, returning the raw string
  `"value":"aaa...a"}` (123 bytes of JSON-looking garbage) instead of the intended 112-`a`
  value. No known encoder in this codebase or `macp-sdk-python` (both use their language's
  standard JSON serializer, which never emits a leading-whitespace byte) produces this shape
  unprompted. Document and pin it as an accepted, priced residual — do not attempt to "fully
  close" it; see Open questions for why a further fix is out of scope. This is also a
  contract-level argument, not just an empirical one: `tests/parity/contract.json`'s
  `contribute_payload.first_byte.legacy_json` is pinned to `"0x7b"`, and
  `contribute_payload_first_byte_markers_match_vectors` (`tests/parity_contract.rs:507-530`)
  asserts every `legacy_json_hex` vector starts with `7b` — a leading-`0x0A` legacy-JSON
  payload already violates this manifest's own cross-implementation-agreed first-byte fact,
  independent of this fix.
- **`MACP_MEMORY_ONLY=1`.** No durable log exists, so there is no cross-restart replay within
  a single process's run; every session in that run is "new" and uses `CURRENT_SEMANTICS_REV`
  throughout. The fix behaves identically whether persistence is on or off — the safety
  property this phase is built around specifically concerns replay across restarts, which by
  construction cannot happen in this mode.
- **`MultiRoundState`'s `#[non_exhaustive]` sealing (CLAUDE.md §8a, 0.8.0).** Not touched —
  this phase changes only the free function and its one call site, not the persisted state
  struct's shape, so there is no interaction with the sealed-record/`semver_check` release
  gate.
- **Empty payload / proto3 no-presence hole** (`value=""` serializes to zero bytes,
  indistinguishable from absent). Already correctly handled by the existing early-return
  (`:67-69`, unchanged by this phase) — `contribute_empty_payload_rejected` continues to pass
  as-is.
- **Concurrency.** The added proto decode/encode is pure, stateless, CPU-bound work over an
  already-in-hand byte slice — no new locking, no shared mutable state, safe under concurrent
  `Send` calls exactly as the function was before.

**Acceptance criteria:**
1. `parse_contribute_value(&buf, 2)` for the 15-byte reproducer (`0x0A 0x0D` +
   `{"value":"x"}`) returns `Ok("x".to_string())` — the historically-wrong-but-preserved
   value — proving legacy replay is byte-for-byte unchanged at rev 2.
2. `parse_contribute_value(&buf, 3)` for the same 15-byte reproducer returns
   `Ok("{\"value\":\"x\"}".to_string())` (13 bytes, the true value) — proving the fix closes
   the bug for new sessions.
3. The same "rev-2 preserves the wrong answer, rev-3 gives the right one" pair holds for
   length 32, using `format!(r#"{{"value":"{}"}}"#, "a".repeat(20))` (a self-contained
   `{"value":"..."}`-shaped value: the length byte `0x20` is JSON whitespace, so both the tag
   byte and the length byte are skipped and the value's own leading `{` opens the object — the
   same mechanism as the length-13 case). **Length 123 uses a different value shape**, because
   it exploits a different mechanism: the single-byte length varint for a 123-byte value is
   `0x7B` (`{`) itself, not whitespace — so JSON parsing treats *the length byte* as the
   object's opening brace, not the value's own first byte. A `{"value":"..."}`-shaped value
   (with its own leading `{`) does **not** collide at length 123 — verified by direct
   simulation this session: the value's leading `{` becomes an invalid second token
   immediately after the object the length byte already opened, the JSON parse fails, and the
   pre-fix decoder already falls through to proto correctly. The length-123 collision instead
   requires the value to supply everything *after* the opening brace:
   `format!(r#""value":"{}"}}"#, "a".repeat(112))` (123 bytes, no leading `{` of its own) —
   this is the identical 125-byte payload as acceptance criterion 5 below; see that criterion
   for why the two are the same bytes read from opposite directions, not a contradiction.
4. A property-style, differential sweep test: for every value length `1..=300` (length `0` is
   excluded — its canonical encoding is zero bytes, which `parse_contribute_value` rejects by
   design regardless of revision, `:67-69`) across **five** value shapes — plain ASCII,
   all-digit, quoted-JSON-string-shaped, `{"value":"..."}`-object-shaped (leading-brace form;
   exercises the whitespace/length-byte mechanism at 13/32), and `"value":"..."}`-shaped
   (no leading brace; exercises the length-byte-is-`{` mechanism at 123) —
   `parse_contribute_value(&ContributePayload{value}.encode_to_vec(), 3)` equals the original
   `value` with zero exceptions in that range. The sweep must be differential to be
   falsifiable, not tautological: at `semantics_rev = 2`, the identical loop must show
   mis-decodes at exactly length `{13, 32}` for the first four shapes and exactly `{123}` for
   the fifth (no-leading-brace) shape, and nowhere else — proving both that the bug is fully
   characterized and that the fix closes exactly it, not a differently-shaped problem.
5. The reverse-direction residual's concrete 125-byte construction — `[0x0A]` prepended to
   `format!(r#"{{"value":"{}"}}"#, "a".repeat(112))` (124 bytes; identical bytes to acceptance
   criterion 3's length-123 case, read from the opposite direction) — decodes as proto (not
   the "intended" JSON reading) at rev 3. **This payload is irreducibly ambiguous, not merely
   under-tested**: read as JSON (leading `0x0A` skipped as whitespace, then a well-formed
   object), it means one thing; read as canonical proto (tag `0x0A`, length byte `0x7B` = 123,
   123 value bytes that round-trip exactly), it means another, and both readings are
   simultaneously valid — no further tie-break can distinguish them from the bytes alone. The
   tie-break's choice (prefer proto) is what makes criterion 3's length-123 case a genuine
   *fix* and simultaneously what makes this criterion's case a documented, accepted
   *residual* — one mechanism, two framings of the same trade-off. Assert both with one test
   (or two tests that explicitly cross-reference each other in their doc comments) rather than
   presenting them as unrelated facts.
6. A legacy-log replay fixture (Tests below) proves a `semantics_rev = 2` session's replayed
   `Commitment.converged_value` for a colliding-length historical `Contribute` matches exactly
   what `handle_commitment` would have produced at original acceptance (the wrong value) — not
   the corrected one.
7. A parallel fixture at `semantics_rev = 3` (a session started fresh) proves the same
   colliding-length `Contribute`, sent to a *new* session, resolves to the correct value.
8. `crates/macp-modes/src/mode/multi_round.rs:55-56`'s docstring no longer contains the
   sentence "A proto payload never parses as a JSON object" as an unconditional claim; a fresh
   reviewer reading it alongside the reproducer above finds no remaining false statement.
9. `tests/parity_contract.rs`'s `contribute_payload_vectors_round_trip_through_the_real_codec`
   and `contribute_acceptance_empty_payload_is_rejected` pass with only their call-site
   arguments changed (new `CURRENT_SEMANTICS_REV` argument) — no assertion values change.
10. `cargo test --workspace` passes with zero failures, new test count reflected in the total.
11. `cargo clippy --workspace --all-targets -- -D warnings` and `cargo fmt --check` are clean
    (or any failure is confirmed pre-existing and unrelated via `git stash`, per this repo's
    own precedent in `plans/parity-contract-176-PROGRESS.md`).
12. `tests/parity/contract.json` is byte-unchanged by this phase (`git diff --stat` shows no
    change to that file) — the CI `check_dir` byte-identity gate against `SPEC_REV` still
    passes.

**Tests** (all in `crates/macp-modes/src/mode/multi_round.rs`'s existing `#[cfg(test)] mod
tests` unless noted):
- `canonical_proto_length_13_preserved_wrong_at_rev2` / `..._corrected_at_rev3` (and the `_32`
  / `_123` siblings, or one parameterized test covering all three lengths) — acceptance
  criteria 1-3.
- `canonical_proto_exhaustively_round_trips_at_rev3` — acceptance criterion 4.
- `reverse_direction_residual_is_a_documented_trade_off_at_rev3` — acceptance criterion 5,
  doc comment cites this plan and `macp-sdk-python/ASSUMPTIONS.md`'s precedent explicitly.
- `whitespace_prefixed_legacy_json_with_unknown_proto_field_shape_still_decodes_as_json` —
  regression-pins that prost's decode-drops-unknown-fields behavior (verified this session:
  `macp-pb`'s `build.rs` uses plain `tonic_prost_build::configure()` with no unknown-field
  retention option, unlike Python's protobuf runtime which needed an explicit
  `DiscardUnknownFields()` call) makes the round-trip check correctly reject a payload that
  merely *parses* as some foreign protobuf message but isn't the canonical encoding of
  `ContributePayload` specifically.
- `handle_contribute_end_to_end_applies_the_rev3_fix` — through `mode.on_message`, not just
  the free function, proving the fix is wired into the live dispatch path — and its sibling
  `handle_contribute_end_to_end_preserves_the_collision_at_rev2`, proving the legacy/in-flight
  path is equally wired through `mode.on_message` (this is the test the Edge cases section's
  "in-flight sessions across the upgrade" bullet promises).
- `existing_multi_round_tests_pass_under_default_rev` — not a new test, but confirms (via
  running the full existing suite) that `session_with_state`'s default `semantics_rev =
  CURRENT_SEMANTICS_REV = 3` doesn't change any of this file's current 21 tests' outcomes.
- Replay-layer fixtures, placed in `src/replay.rs`'s test module (`crates/macp-modes` is not
  where replay lives — `replay_session` and its existing `semantics_rev` legacy-log fixtures,
  e.g. `legacy_rev0_handoff_history_replays_under_envelope_clock`
  (`src/replay.rs:1171-1193`), are the direct structural precedent to follow): a
  `legacy_rev2_multi_round_history_replays_to_the_original_collision` fixture (SessionStart at
  `semantics_rev: 2`, a colliding-length `Contribute`, a `Commitment`; asserts the replayed
  `resolution` matches the historically-wrong value) and a
  `rev3_multi_round_history_replays_to_the_corrected_value` sibling (fresh SessionStart,
  same colliding `Contribute`, asserts the corrected value) — acceptance criteria 6-7.
  Implementation notes: `src/replay.rs`'s test module has no existing multi_round fixture to
  copy from (these are new); assert the replayed **`session.resolution`** field (the
  `ResolutionPayload` JSON `handle_commitment` produces and `apply_mode_response` stores) —
  there is no field literally named `Commitment.converged_value`. `make_registry()`
  (`src/replay.rs:403-405`, = `ModeRegistry::build_default`) already registers
  `ext.multi_round.v1` (`crates/macp-modes/src/mode_registry.rs:287-291`), so no custom
  registry is needed. `ext.multi_round.v1` requires the strict canonical `SessionStart`
  contract (`requires_strict_session_start`, `crates/macp-modes/src/mode_registry.rs:747`) —
  bind non-empty `participants`, `mode_version`, `configuration_version`, positive `ttl_ms`.
  Convergence needs every declared participant to contribute an equal value, and
  `authorize_sender` requires `Commitment.sender == initiator_sender` while `Contribute.sender`
  must be a declared participant — the simplest fixture uses one participant plus a distinct
  initiator sender. `CommitmentPayload.mode_version`/`configuration_version` must match the
  session-bound values or `validate_commitment_payload_for_session` rejects it.
- `tests/parity_contract.rs`'s three updated call sites (no new test names, existing tests
  continue to assert the same values) — acceptance criterion 9.

**Local toolchain note (not a plan defect, but relevant to executing this plan):** at planning
time, `cargo build`/`cargo test` fail on this machine at the link step (`ld: tapi error:
malformed file … unknown architecture arm64e.x1-macos`, a broken Xcode Command Line Tools
install unrelated to this change). Acceptance criteria 10-11 cannot be discharged locally
until that's repaired; every byte-level claim in this plan was independently verified by
direct simulation of `serde_json`'s whitespace/typed-struct rules and prost's canonical
varint encoding rather than by running the (currently unbuildable) test suite, and must be
re-confirmed against the real suite once the toolchain is fixed or in CI.

**Docs:** No `CLAUDE.md`/`docs/` edit required. `CLAUDE.md`'s existing `semantics_rev`
mentions are scoped to the dedup/rejected-message invariant (Freeze-profile priorities) and
the sealed-records section (§8a), neither of which this phase touches;
`crates/macp-core/src/session.rs`'s own `CURRENT_SEMANTICS_REV` doc block is this project's
authoritative revision log and gets the new rev-3 bullet as part of this phase's own Files
list, not a separate docs update. `docs/testing.md:19`'s one-line mention of
`parse_contribute_value` describes only its *existence*, not its signature — unaffected.
`CHANGELOG.md` is not hand-edited (release-plz generates it from the conventional-commit
message; same reasoning `plans/parity-contract-176.md` already applied to this repo).

---

### Phase 2 — File the cross-repo ask to close the parity-contract's documented gap

**Status:** DONE (2026-09-25) — `plans/cross-repo/multiagentcoordinationprotocol-parse-contribute-value-192.md`
written, with the three proposed vectors regenerated from the actual shipped Phase 1 code
(not a pre-implementation guess) and independently re-derived (not copied from this plan's
Context section). GitHub issue filed in the spec repo — see `PROGRESS.md` for the issue URL.

**Delivers:** A GitHub issue filed against the spec repo
(`multiagentcoordinationprotocol/multiagentcoordinationprotocol`) proposing that
`schemas/parity/contract.json`'s `contribute_payload` section gain collision-length vectors
(13, 32, 123) and a short addendum to its `decode_order`/`source` prose describing the
canonicality tie-break — closing the gap `schemas/parity/README.md`'s own "Open items"
section already names ("Contribute payload decode on non-canonical inputs"). Plus a local
plan document this repo owns, describing exactly what re-vendoring work is needed here once
that upstream change lands.

**Depends on:** Phase 1, as a **sequencing** preference, not a hard code dependency — Phase 2
produces a doc and a filed issue, no code, so it could technically be drafted from Phase 1's
planned (pre-implementation) tie-break semantics alone. It is sequenced after Phase 1 anyway
because proposing vectors before the exact implemented behavior is verified risks proposing
vectors that don't match what actually shipped. Consistent with
`plans/parse-contribute-value-192-PROGRESS.md`'s one-PR recommendation, which treats both
phases as co-shippable in a single PR.

**Files:**
- `plans/cross-repo/multiagentcoordinationprotocol-parse-contribute-value-192.md` (new, this
  repo) — the cross-repo plan doc. Contents: proposed new `contribute_payload.vectors` entries
  for lengths 13, 32, 123 (each with `name`, `value`, `protobuf_hex`, and — critically,
  *without* a `legacy_json_hex` field, since encoding these values as legacy JSON is exactly
  what collides; the vector's point is that only the *proto* encoding exists safely — and with
  `decode_only` explicitly absent or `false`: the one existing vector that omits
  `legacy_json_hex` (`one_byte_varint_boundary`) also sets `decode_only: true`, which disables
  the encode round-trip assertion, and copying that shape here would silently disable the
  exact round-trip check these new vectors exist to pin), a
  proposed one-sentence addition to the `contribute_payload.source`/`decode_order` field
  documenting that "json" being tried first is now qualified by a canonicality check once
  `macp-sdk-python` (already shipped, PR #77) and `macp-runtime` (Phase 1) apply it, a note
  that `macp-sdk-typescript` has not yet been checked for the same collision class (explicitly
  flagged as unknown, not assumed either way — see Open questions), and the exact re-vendoring
  steps this repo will run once the spec PR merges (`cp` per `tests/parity/SOURCE.md`, bump
  `SPEC_REV` in `.github/workflows/ci.yml`, update the hardcoded `vectors.len(), 4` assertion
  in `tests/parity_contract.rs:456-461` to the new count, update `tests/parity/SOURCE.md`'s
  pinned commit/date) — all of which is **out of scope for this plan's own execution**, since
  it cannot happen before the upstream PR merges on its own timeline (see Long-term posture).
- No file in `../multiagentcoordinationprotocol` is edited directly — only a GitHub issue is
  filed there, per this repo's own `/plan` convention that a cross-repo *write* always needs
  an explicit gate, while filing a tracked issue does not.

**Approach:** Per this repo's own cross-repo convention: don't fold a change owned by another
repo into this plan's phases as if it were a local file, and don't leave "someone should
update the spec repo" as a vague aspiration. Write the concrete ask locally, file it as a
real issue with the proposed diff attached, and let the spec repo's own maintainers decide
on its own timeline (whether the vectors are accepted as proposed, revised, or rejected is
that repo's call, not this plan's). This also gives `macp-sdk-typescript` a shared target to
implement the same protection against, if it has the same class of bug (unknown — see Open
questions) — closing this only in `macp-runtime` and `macp-sdk-python` while leaving
`macp-sdk-typescript` unchecked would be a partial, silently-incomplete parity story.

**One consequence of choosing Option A that the filed issue must disclose, not hide.** The
proposed vectors would encode the tie-break as a cross-implementation-agreed contract, but
macp-runtime only satisfies it at `semantics_rev >= 3` — `macp-sdk-python`'s PR #77 applies its
equivalent tie-break unconditionally (no revision concept exists there). Phase 1's
`tests/parity_contract.rs` change (passing `CURRENT_SEMANTICS_REV`) makes macp-runtime's own
test suite pass against the new vectors, but that reflects a choice about which behavior to
assert, not proof the two implementations agree unconditionally — a legacy
(`semantics_rev <= 2`) macp-runtime session does **not** satisfy these vectors. The filed
issue and the cross-repo plan doc must say this plainly, or the manifest would silently
assert a stronger cross-implementation guarantee than macp-runtime actually provides for
pre-existing sessions.

**Edge cases & failure modes:**
- **The spec repo may reject or revise the proposed vectors.** Not a failure of this phase —
  its deliverable is the ask, not the acceptance. If revised, this repo's eventual
  re-vendoring step (out of scope here, see above) follows whatever the spec repo actually
  merges, not this plan's initial proposal.
- **`macp-sdk-typescript` may already have (or lack) this bug** — this plan does not check
  that sibling repo (out of scope for a macp-runtime plan); the filed issue should say so
  explicitly rather than assume either way, so the spec-repo maintainers or the TypeScript
  SDK's own maintainers can verify it independently.
- **No `legacy_json_hex` on the new vectors.** This is deliberate, not an oversight: a
  13/32/123-byte value that legitimately needs to serialize as legacy JSON with that exact
  content already collides today under the pre-fix decode order at `semantics_rev < 3` — the
  new vectors exist specifically to pin the *proto* decode as correct, not to imply a safe
  legacy-JSON encoding exists for them too (it doesn't, and can't, without inverting decode
  order, which is out of scope — see Open questions).

**Acceptance criteria:**
1. `plans/cross-repo/multiagentcoordinationprotocol-parse-contribute-value-192.md` exists in
   this repo, contains concrete proposed vector JSON (not just prose), and is internally
   consistent with Phase 1's actual shipped tie-break behavior (not the plan's earlier draft
   of it).
2. A GitHub issue exists at
   `github.com/multiagentcoordinationprotocol/multiagentcoordinationprotocol`, links back to
   this repo's plan file at a stable blob URL, and states the three proposed vectors plus the
   TypeScript-SDK unknown explicitly.
3. This repo's own `tests/parity/contract.json` and `tests/parity_contract.rs` are
   byte-for-byte/assertion-for-assertion unchanged by this phase (the re-vendor is explicitly
   deferred, not silently attempted).

**Tests:** None (this phase produces a plan document and a filed issue, no code change).

**Docs:** None in this repo's own `docs/`. `seam/docs/` is not applicable — this repo has no
`seam/` directory (confirmed: `ls seam` fails in this repo root).

## Long-term posture

**One-way door, correctly avoided rather than accepted.** An unconditional global change to
`parse_contribute_value`'s decode behavior (Option B considered and rejected — see Open
questions) would have been a genuine one-way door: it changes what a specific, already-durably-
persisted byte sequence decodes to, and per `src/replay.rs:92-138` this runtime re-derives
state from those exact bytes on every future replay, forever. Once shipped that way, there
would be no way to tell, for any given already-existing session, whether its replayed state
still matches what real agents were told at the time — and no way to undo having shipped it
without a second, even more disruptive change. Phase 1's `semantics_rev`-gated design avoids
taking that door at all: it is provably, testably, reversible-in-a-commit **as a replay
concern** (revert Phase 1's code, `CURRENT_SEMANTICS_REV` still gates correctly on rollback
since existing rev-3 sessions simply... stay rev-3 and keep using whatever code is deployed at
replay time — the revision number itself does not need reverting), while still being a real,
irreversible-in-the-good-way fix to *future* acceptance behavior. This is the same trade the
two prior `semantics_rev` bumps already made for Handoff; nothing about this phase invents a
new category of risk for this codebase.

**What a fast (unconditional) approach would have cost:** every already-existing session
containing a colliding-length canonical-proto `Contribute` would silently change its replayed
outcome on the next restart, with no audit trail distinguishing "this session's resolution
just changed because of a bug fix" from "this session's resolution changed because of a
storage/replay bug" — exactly the failure mode `CONTRIBUTING.md:44-47`'s ground rule and
`RFC-MACP-0003 §1` are designed to make structurally impossible to ship by accident.

**The cross-repo parity-contract gap (Phase 2) is not itself a one-way door** — it's a
proposal, gated by another repo's own review process, with no code-level irreversibility on
this repo's side until (and unless) a future re-vendoring phase actually lands here.

## Enterprise concerns

- **Correctness at scale:** the bug is narrow-trigger (three specific byte lengths, and only
  when the value's own content is JSON-object-shaped) but high-severity when hit — a
  convergence primitive silently returning the wrong value with no error anywhere is exactly
  the class of defect that erodes trust in a coordination kernel faster than a loud failure
  would. Phase 1's exhaustive sweep test (acceptance criterion 4) is the scale-correctness
  argument, not just the three named reproducers.
- **Security:** not a trust-boundary or auth concern (CLAUDE.md §4 doesn't apply — no sender
  identity or authentication logic is touched); this is a data-integrity concern within an
  already-authenticated, already-authorized message path.
- **Observability:** the new `tracing::debug!` call (Phase 1) gives operators a way to detect,
  in production, how often real proto traffic actually lands on the ambiguous byte lengths —
  turning "we fixed a theoretical collision" into "we can see whether it was ever actually
  hit," which the pre-fix code gave no way to observe (it failed silently, not loudly).
- **Migration/rollback:** no data migration is needed or possible — this fix is forward-only
  by design (new sessions get it, old sessions never need it rewritten). Rollback of the code
  change itself is a normal revert; no persisted-state migration accompanies or is required by
  this plan.

## Open questions

**Resolved during planning, not left open — the critical decision.** Per this repo's own
`/plan` Autonomy ladder, "should this ship as an unconditional global fix, or gated behind a
new `semantics_rev`" is exactly a one-way-door-shaped call (it decides what an
already-persisted byte sequence means, forever, on replay) and was routed to a scoped Fable
consult before this plan's Phase 1 was written, rather than decided unilaterally. Fable's
findings (independently corroborated this session against `src/replay.rs`, `src/runtime.rs:
501,573`, and `crates/macp-core/src/session.rs:195-224`, not taken on faith):
- **Recommendation: gate on `session.semantics_rev >= 3`** (Option A), not an unconditional
  global change (Option B) or a `MultiRoundState`-local flag (Option C — rejected because
  `on_session_start` re-mints `MultiRoundState` fresh on every replay too, so a mode-local flag
  would just have to derive from `session.semantics_rev` anyway, i.e. strictly more surface for
  the same gate).
- **Verdict: does not need to go to the user before implementation.** Option A leaves
  `semantics_rev <= 2` replay byte-identical (provable by the Phase 1 fixture), closes the bug
  for every new session, reuses this codebase's own designed mechanism (not a novel one), and
  the marginal implementation cost over Option B is small (one constant, one comparison, one
  legacy-log fixture, one doc bullet) relative to the alternative of writing and living with a
  knowing RFC-MACP-0003 §1 violation. This is logged here as a **defensible, already-analyzed
  engineering decision**, not an `UNCONFIRMED` assumption — `/implement` should still record it
  in `DECISIONS.md` (citing this plan and the Fable consult) per this repo's own convention
  for decisions of this shape (see `DECISIONS.md`'s existing D7 entry for the citation style
  to match), but it does not need to route back through `/reconcile` as an open item.
- **One known, accepted trade-off, not further reducible within this plan's scope:** the
  reverse-direction residual (Phase 1 Edge cases). Closing it fully would require inverting
  the JSON-then-protobuf decode order, which — per the issue's own framing and
  `schemas/parity/contract.json`'s `contribute_payload.decode_order` field — is a
  cross-implementation-pinned convention, not something this repo can unilaterally flip
  without breaking parity with `macp-sdk-python`/`macp-sdk-typescript`. Logging this as
  `ASSUMPTIONS.md`-tracked (`UNCONFIRMED` in the narrow sense of "not yet proven zero real
  traffic will ever hit it," though provably narrow in construction) is the right level of
  caution — not a further Fable escalation, since the Python precedent already accepted the
  identical trade for the identical reason.

**Left genuinely open, routed by blast radius:**
- **Does `macp-sdk-typescript` have the same collision class?** Not checked by this plan
  (out of scope — a macp-runtime plan does not audit a third sibling repo's source). Flagged
  explicitly in Phase 2's filed issue rather than assumed either way. Low blast radius (a
  question for that repo's own maintainers, not a decision this plan needs to make).
- **Will the spec-repo maintainers accept the Phase 2 vector proposal as-is?** Unknowable
  before filing; not this plan's decision to make (it belongs to that repo's own review
  process). Tracked via the filed issue, not resolved here.

## Repo map

See `plans/parse-contribute-value-192-PROGRESS.md`.

## Plan review

**Round 1:** `REVISE`. A fresh, independent agent checked every `file:line` citation in this
document against live code (not against this document's own prose), redid the byte-level
arithmetic independently, and pressure-tested the core `semantics_rev`-gating design against
`src/replay.rs`, `src/runtime.rs`, and `crates/macp-core/src/session.rs`. The overwhelming
majority of citations (roughly 40) checked out exactly as written. Findings and how each was
closed:

1. **A genuine arithmetic error, independently reproduced and confirmed this session by direct
   simulation:** the plan's original length-123 construction
   (`format!(r#"{{"value":"{}"}}"#, "a".repeat(111))`, i.e. a `{"value":"..."}`-shaped value)
   does **not** collide — at length 123 the proto length byte is `0x7B` (`{`) itself, not
   whitespace, so it becomes the JSON object's own opening brace and the value's *own* leading
   `{` is then an invalid second token, correctly failing the JSON parse even pre-fix.
   Acceptance criterion 3 was rewritten to use the correct no-leading-brace construction
   (`format!(r#""value":"{}"}}"#, "a".repeat(112))`) for length 123, with an explanation of why
   it needs a different value shape than 13/32. This was the most load-bearing fix — every
   downstream length-123 test in this plan depended on it.
2. Criteria 3 (length 123) and 5 (the reverse-direction residual) turned out to describe the
   **identical 125-byte payload** read from opposite directions, and the original draft
   presented them as unrelated/contradictory facts. Rewrote both to state plainly that this
   payload is irreducibly ambiguous — the tie-break's choice to prefer proto is simultaneously
   what makes criterion 3 a fix and what makes criterion 5 a residual — and to cross-reference
   each other instead of reading as a contradiction.
3. **The Context and Long-term-posture sections overstated the replay-risk blast radius.**
   The original draft argued an unconditional fix would corrupt "any already-existing session
   … forever." Verified this is false as stated: `Runtime::maybe_compact_log`
   (`src/runtime.rs:1225-1271`, called at `:937,1048,1376`) unconditionally compacts every
   terminal session's log to a single `Checkpoint` entry — not gated by
   `MACP_CHECKPOINT_INTERVAL`, confirmed directly against `CLAUDE.md`'s own env-var table —
   so a *resolved* multi_round session's raw `Contribute` bytes are gone from all future
   replay. Rewrote the Context, the "Checkpointed sessions" edge case (retitled "Checkpointed
   and compacted sessions"), and the Long-term-posture framing to the accurate, narrower
   exposure: `Open` sessions bounded by `MAX_TTL_MS` (24h), plus any session (terminal or not)
   where compaction was skipped (it is best-effort). The recommended approach (Option A) does
   not change — `CONTRIBUTING.md:44-47`'s rule and Option A's low marginal cost hold regardless
   of exposure size — only its justification was corrected.
4. The plan attributed the stale `schemas/parity/contract.json` path reference to "the issue
   and this function's own doc comment." Verified via the issue's full body (`gh issue view
   192`): the issue never mentions that path at all — only the function's own doc comment does.
   Corrected the attribution.
5. Acceptance criterion 4's exhaustive sweep omitted the one value shape (no-leading-brace,
   `"value":"..."}}`) that actually exercises the length-123 mechanism, and included length `0`
   (whose canonical encoding is zero bytes, always rejected regardless of revision) as if it
   were a normal case. Both fixed: five shapes now required, range changed to `1..=300`, and
   the criterion was rewritten to require a differential result (rev 2 mis-decodes at exactly
   `{13, 32}` for four shapes and `{123}` for the fifth) rather than a single-revision
   assertion, so it is falsifiable rather than tautological.
6. Smaller corrections applied: test count is 21, not 20 (`grep -c '#\[test\]'`); the code
   sketch's `m.encode_to_vec()` needs `prost::Message` in scope at module level (currently
   imported only inside `#[cfg(test)]`, `:230`) — noted for the implementer; the rev-3 doc
   bullet now states explicitly that it changes no other mode's behavior (all existing gates
   are `>= 2`/`<= 1`, never `== 2`); a stray trailing quote in the "lengths 9/10" paragraph was
   removed; the `Session::builder` citation was tightened to the exact struct-literal line
   range; `SPEC_REV`'s citation in the PROGRESS file was corrected to its exact line (`:39`).
   The Edge-cases section's promise of an in-flight-session test now has a matching entry in
   the Tests list (`handle_contribute_end_to_end_preserves_the_collision_at_rev2`).
7. Added, per the reviewer's "should add" findings: the parity contract's own `"0x7b"`
   first-byte pin as a second, contract-level argument for why the reverse-direction residual
   is acceptable (not just "no known encoder does this"); implementation-level specificity for
   the replay fixtures (assertion target is `session.resolution`, not a nonexistent
   `Commitment.converged_value` field; `ext.multi_round.v1` registry/strict-`SessionStart`/
   participant-authorization prerequisites); a one-line confirmation that `MultiRoundState`'s
   plain `serde_json` encoding has no interaction with this fix; Phase 2's "Depends on"
   reframed as sequencing rather than a hard dependency; a `decode_only` shape warning for the
   proposed new parity vectors; an explicit disclosure requirement in Phase 2 that
   macp-runtime only satisfies the proposed vectors at `semantics_rev >= 3`, unlike Python's
   unconditional application; and a local-toolchain note (this machine's `cargo build` fails at
   the link step, unrelated to this change) recording that all byte-level claims were verified
   by direct simulation rather than by running the test suite, pending re-confirmation once the
   toolchain works.
8. The reviewer independently confirmed the core design call (Option A, `semantics_rev`
   gating) is sound and found no better fourth option, and separately confirmed a fact this
   plan's first draft asserted but had not itself verified: there is no `semantics_rev == N`
   comparison anywhere in the codebase (all gates are `>=`/`<=`), so bumping the shared counter
   to 3 is mechanically safe for every other consumer (Handoff, the suspension-cycle cap) —
   folded into the Files section's rev-3 doc-bullet addition (finding 6 above).

No second round was run: every finding above was independently re-verified against live code
or by direct simulation before being applied (not merely asserted from the review agent's
report), and the round-1 list contained no unresolved or disputed item.

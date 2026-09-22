# PLAN — verify and pin the handoff implicit-accept path against macp-runtime 0.8.0 (`macp-sdk-python`)

**Verified against:** `macp-sdk-python` at `35bc5a8` on `main`, clean tree, read 2026-09-21.
**Runtime verified against:** `macp-runtime` at `7815a97` on `main`; GitHub Release `macp-runtime-v0.8.0` published 2026-09-20T19:43:47Z (current `crates.io`/PyPI-equivalent target — verify propagation before relying on a pinned version bump).
**Write scope:** this plan is authored in `macp-runtime`; every file it proposes to change lives in `macp-sdk-python`. Nothing in this document was executed. No sibling repo was modified.

---

## Context

### What changed in `macp-runtime`, and why it is wire-visible

`macp-runtime` 0.8.0 (PR #171, merged 2026-09-13, released 2026-09-20) activates a runtime
behavior that has been part of RFC-MACP-0010 §5.1(2)/(3)'s contract on paper for a while, but
that no previously-released runtime version ever actually produced:

- Every **new** session now gets `semantics_rev = CURRENT_SEMANTICS_REV` (= `2`)
  unconditionally — `crates/macp-core/src/session.rs:221` (`Session::new`) and `:920`. This is
  not negotiated by the client; there is no opt-in and no version pin a caller can set to avoid
  it.
- `HandoffMode::due_synthetic_envelope` (`crates/macp-modes/src/mode/handoff.rs:403-466`) is
  gated on `session.semantics_rev >= 2` at `:404` — so it was unreachable in every runtime
  version before 0.8.0, and is now reachable in every new session by default.
- When it fires (an outstanding `HandoffOffer`'s `implicit_accept_timeout_ms` has elapsed and
  the offer has not been explicitly accepted or declined), the runtime synthesizes a real
  `HandoffAccept` envelope — `message_id = "implicit-accept:<handoff_id>"`, `sender` = the
  offer's `target_participant`, payload `HandoffAcceptPayload { implicit: true, ... }`
  (`handoff.rs:445-465`) — appends it to **accepted history** (`EntryKind::Incoming`, consuming
  an accepted ordinal), and publishes it to `StreamSession` subscribers
  (`src/runtime.rs:721-786`, `synthesize_due_accept`). Before 0.8.0, an offer that timed out
  unactioned was resolvable server-side on query but produced **no envelope a client could ever
  observe** — the handoff looked permanently stuck at `status: "offered"` from the SDK's point
  of view, even though a subsequent commitment could legally treat it as accepted.
- `docker.yml` builds and pushes `ghcr.io/multiagentcoordinationprotocol/macp-runtime:latest`
  on every push to `main` (`.github/workflows/docker.yml:6-9`), independent of the crates.io/
  GitHub-Release cadence — so this behavior has been live in the `:latest` image since
  2026-09-13, a week before the 0.8.0 GitHub Release.

### The SDK already anticipated the wire contract — verified, not assumed

`macp-sdk-python` did **not** wait for 0.8.0 to model this. Reading the current code:

- `src/macp_sdk/handoff.py:29-34` (`HandoffRecord.implicit`) already carries the field.
- `src/macp_sdk/handoff.py:134-144` (`HandoffProjection.is_implicitly_accepted`) already
  distinguishes a timeout-driven implicit accept from an explicit one.
- `src/macp_sdk/handoff.py:220-231` (`HandoffSession.accept_handoff`) already documents and
  regression-tests (per its own comment, in `tests/unit/test_handoff.py`) that a client-submitted
  accept can never carry `implicit=true`.
- `tests/unit/test_absorb_runtime_v050.py:137-196` (the "B-3: Handoff implicit accept" block)
  already decodes a hand-constructed synthetic envelope
  (`message_id=f"implicit-accept:{handoff_id}"`, `implicit=True`) and asserts
  `is_implicitly_accepted("h1") is True`.

**No projection, decode, or validation change is needed.** The gap is narrower and concrete:

1. **Two docstrings are now false.** Both say the runtime does not yet emit these:
   - `src/macp_sdk/handoff.py:31-33`: *"Runtime v0.5.0 defines but does not yet emit these; the
     SDK surfaces the field so histories that contain them replay correctly."*
   - `src/macp_sdk/handoff.py:137-139`: *"Runtime v0.5.0 does not yet emit implicit accepts, so
     this returns False for all live sessions today; it exists so histories/replays that carry
     them surface the distinction."*
   As of `macp-runtime` 0.8.0 this is factually wrong for any newly-created session, and nothing
   tells a reader of this SDK that it changed.
2. **No test exercises this end to end, against a real runtime.** `tests/unit/test_absorb_runtime_v050.py`'s
   B-3 block feeds a hand-built envelope directly into the projection — it proves the *decode*
   path, not that a live runtime actually produces this envelope, with this shape, on this
   schedule, over the real gRPC boundary. `tests/integration/` (30 passing tests against
   `macp-runtime 0.7.0`, per the sibling plan `plans/cross-repo/macp-sdk-python-examples-docs-and-release.md`)
   has no handoff-timeout scenario at all — `grep -rn "implicit" tests/integration/` returns
   nothing.
3. **This repo's CI runs no integration tests yet** (confirmed: no `ghcr.io` reference anywhere
   under `.github/workflows/`). That gap is already tracked as Phase 2 of the sibling plan above
   and is **not duplicated here** — but it means today there is no automated signal, local or
   CI, that would catch a regression in this specific path. Phase 2 below does not require that
   sibling phase to land first: `make test-integration` already works locally against a
   manually-started runtime (`docs/... ` per that plan's Context) or the pinned GHCR image run
   by hand.

---

## Phases

### Phase 1 — correct the two stale docstrings

**Status: TODO**

**Delivers:** `HandoffRecord.implicit` and `HandoffProjection.is_implicitly_accepted` describe
current, released behavior instead of a version that predates the field's own introduction.

**Depends on:** nothing.

**Files:** `src/macp_sdk/handoff.py` (`:31-33`, `:137-139`).

**Approach.** Replace both "does not yet emit" claims with an accurate statement: `macp-runtime`
`>= 0.8.0` emits these automatically for every session (no server-side opt-in exists), citing
RFC-MACP-0010 §5.1(2)/(3) for the contract and noting the minimum runtime version. Keep the
rest of each docstring (the distinction from an explicit accept, the replay-correctness
rationale) — only the now-false sentence changes.

**Edge cases & failure modes.** None — this is a comment-only change with no behavior or test
impact. The risk of doing nothing is a maintainer reading `is_implicitly_accepted`'s docstring
and concluding the return value is dead code, when it is now live for any session created
against a current runtime.

**Acceptance criteria.**
1. Neither docstring claims the runtime does not emit implicit accepts.
2. Both name the runtime version threshold (`0.8.0`) and the RFC section.
3. `mypy src/macp_sdk/` and `ruff check` unaffected (docstring-only).

**Tests.** None new; this is documentation.

**Docs.** If `docs/modes/handoff.md` repeats either claim (grep it before assuming it doesn't —
this plan did not re-verify that page), correct it in the same commit.

---

### Phase 2 — a live-runtime integration test for the implicit-accept path

**Status: TODO**

**Delivers:** `tests/integration/test_handoff_implicit_accept.py` — a test that offers a
handoff with a short `implicit_accept_timeout_ms`, lets it elapse against a real running
`macp-runtime`, and asserts the projection observes the synthetic accept exactly as specified,
closing the "decode-only, never observed live" gap named in Context.

**Depends on:** Phase 1 (so the corrected docstring and the new test agree on current behavior).
Does **not** depend on the sibling plan's Phase 2 (CI wiring) — this test runs the same way
every other file in `tests/integration/` already does, via
`MACP_TEST_BINARY=../target/debug/macp-runtime` locally, or against the pinned GHCR image by
hand; it rides that CI job automatically once that phase lands, with no further change.

**Files:**
- `tests/integration/test_handoff_implicit_accept.py` (new)
- `tests/integration/conftest.py` — only if a shared short-timeout policy fixture is worth
  factoring out; otherwise build the policy inline.

**Approach.**

Use `build_handoff_policy(..., acceptance=HandoffAcceptanceRules(implicit_accept_timeout_ms=<small>))`
(`src/macp_sdk/policy.py:439-459`) with a timeout small enough to keep the test fast (hundreds
of milliseconds — check what the runtime's own tests use as a floor;
`crates/macp-modes/src/mode/handoff.rs`'s unit tests are a reference for a realistic minimum
that will not flake under CI scheduling jitter) but not so small it races the offer's own
`Send`/ack round trip. Register the policy, start a session bound to it, send `HandoffOffer`,
wait past the timeout (poll, don't `sleep()` blindly, matching this suite's existing style),
then assert via the projection:

- `handoff.status == "accepted"`
- `handoff.implicit is True`
- `handoff.accepted_by == target_participant`
- the underlying envelope's `message_id == f"implicit-accept:{handoff_id}"` (accessible via
  whatever this SDK's stream/ack surface exposes raw envelope metadata — check
  `BaseSession`/`BaseProjection`'s existing accessors before adding a new one)

Also assert the **session-level** view agrees — `GetSession` or an equivalent poll — so the test
proves the runtime's own state, not only what one client's stream happened to deliver.

**Rejected:** a unit test with a hand-built envelope. `tests/unit/test_absorb_runtime_v050.py`
already is that test; it proves the SDK decodes the shape correctly, not that the runtime
produces it. This phase's entire purpose is closing exactly that distinction.

**Edge cases & failure modes.**
- *Timing flakiness.* Poll with a bounded retry/timeout rather than a fixed `sleep()`, and make
  the bound generous relative to the configured `implicit_accept_timeout_ms` (the runtime's own
  `MACP_CLEANUP_INTERVAL_SECS`-driven eager sweep, default 60s, plus the lazy on-touch path
  means the synthetic accept may not appear the instant the timeout elapses — read
  `macp-runtime/CLAUDE.md`'s `MACP_CLEANUP_INTERVAL_SECS` entry before picking a poll window, or
  touch the session with a cheap `GetSession` call to trigger the lazy path instead of waiting
  on the sweep).
- *A runtime older than 0.8.0.* If `MACP_TEST_BINARY` points at a pre-0.8.0 build, this test
  will hang or time out waiting for an envelope that never arrives. Give the timeout-poll a hard
  ceiling and a failure message that names the runtime version requirement, so a contributor
  running against a stale local binary gets a clear diagnosis instead of a bare timeout.
- *Runtime state leakage.* Use `MACP_MEMORY_ONLY=1` (already this suite's convention) so the
  short-timeout policy and session do not persist across runs.

**Acceptance criteria.**
1. The test fails (times out or asserts false) against a `macp-runtime` binary built before
   PR #171 — verify this once, on a scratch checkout, the same way the sibling plan's Phase 1
   verifies its gate by observing it red first.
2. The test passes against current `main`/`0.8.0`.
3. `is_implicitly_accepted()` is asserted `True` specifically — not just `is_accepted()` — so a
   future change that emits an explicit-looking accept instead of the flagged one is caught.
4. `make test-integration` picks it up with no separate invocation.

**Tests.** This phase's deliverable is itself a test; no additional harness work.

**Docs.** `tests/integration/README.md`, if it enumerates covered scenarios — add handoff
implicit-accept to the list.

---

## Long-term posture

**This is not a one-way door and carries no breaking risk.** The wire contract (`implicit`
field, `implicit-accept:<handoff_id>` message_id format) predates 0.8.0 and was already modeled
defensively; 0.8.0 only makes a previously-dead code path live. Nothing here changes the SDK's
public API.

**Every existing handoff-timeout scenario, anywhere this SDK is deployed against a runtime
`>= 0.8.0`, now behaves differently from before** — a handoff that used to sit silently at
`status: "offered"` past its timeout will now resolve to `status: "accepted", implicit: True`.
Any caller-side code that polled `pending_handoffs()` expecting a timed-out offer to still
appear there will see it disappear once the runtime is upgraded. This is the correct, spec-
conformant behavior (RFC-MACP-0010 §5.1(2)/(3)), but it is worth one line in this repo's own
`CHANGELOG.md` or a release note the next time the pinned/tested runtime version crosses 0.8.0,
so it isn't a silent behavior change from an SDK user's perspective even though no SDK code
changed.

## Open questions

None with a genuine fork. Both phases have a defensible default and are additive/corrective
only.

# PLAN — pin the handoff implicit-accept path in the live-runtime CI gate (`macp-sdk-typescript`)

**Verified against:** `macp-sdk-typescript` at `15ea6d2` on `main`, clean tree, read 2026-09-21.
**Runtime verified against:** `macp-runtime` at `7815a97` on `main`; GitHub Release `macp-runtime-v0.8.0` published 2026-09-20T19:43:47Z.
**Write scope:** this plan is authored in `macp-runtime`; every file it proposes to change lives in `macp-sdk-typescript`. Nothing in this document was executed. No sibling repo was modified.

---

## Context

### What changed in `macp-runtime`, and why it is wire-visible

See the parallel plan `plans/cross-repo/macp-sdk-python-handoff-implicit-accept-0-8-0.md` in
this repo for the full derivation; summarized:

- `macp-runtime` 0.8.0 (PR #171, merged 2026-09-13, released 2026-09-20) makes every **new**
  session's `semantics_rev` unconditionally `2` (`crates/macp-core/src/session.rs:221`, `:920`)
  — not negotiated, no opt-out.
- This makes `HandoffMode::due_synthetic_envelope`
  (`crates/macp-modes/src/mode/handoff.rs:403-466`, gated `session.semantics_rev >= 2` at
  `:404`) reachable for the first time in any released runtime: when an outstanding
  `HandoffOffer`'s `implicit_accept_timeout_ms` elapses unactioned, the runtime now appends a
  real `HandoffAccept` envelope to accepted history and publishes it over `StreamSession`
  (`sender` = offer target, `message_id = "implicit-accept:<handoff_id>"`, payload
  `implicit: true`) — instead of leaving the offer observably stuck at `offered` forever, which
  is what every runtime version before 0.8.0 did.
- `ghcr.io/multiagentcoordinationprotocol/macp-runtime:latest` has carried this behavior since
  2026-09-13 (`.github/workflows/docker.yml:6-9` pushes on every `main` push, independent of the
  crates.io/GitHub-Release cadence) — a full week before the tagged 0.8.0 release.

### This SDK's integration CI has been running against the new behavior, unpinned, since it landed

`.github/workflows/integration.yml` already pulls `ghcr.io/.../macp-runtime:latest`
(`:44-45`) on every push to `main` and every PR (`:24-27`), and its own header comment
(`:1-13`) explains this is deliberate: *"`:latest` tracks the runtime's main branch
deliberately: this suite exists to catch drift against the current runtime, not a frozen
snapshot of it."* That design is correct and this plan does not propose changing it — a
sha-pin would trade the exact signal this job exists to give.

What it does **not** have is a test case for this specific scenario. `tests/integration/runtime.test.ts`
covers `'Handoff mode — happy path'` (`:404-458`, explicit `acceptHandoff`) and
`'Handoff mode — decline path'` (`:461+`) — `grep -n "implicit" tests/integration/runtime.test.ts`
returns nothing. So for the week between 2026-09-13 and 2026-09-20, this CI job pulled a
runtime image capable of emitting a brand-new class of envelope on every PR, and nothing in the
suite would have noticed if that envelope's shape, timing, or authorization had a defect —
the exact drift `integration.yml`'s own stated purpose calls out.

### The SDK already anticipated the wire contract — verified, not assumed

As with the Python SDK, no decode/projection work is needed:

- `src/types.ts:298-310` (`HandoffAcceptPayload.implicit`) already documents the exact contract
  this runtime version now exercises, correctly and without a stale "not yet emitted" claim.
- `src/handoff.ts:128-135` already strips a client-submitted `implicit` field before encoding,
  matching the runtime's rejection of a forged `true`.
- `src/projections/handoff.ts:19-22`, `:113-144` (state application), `:224-231`
  (`isImplicitlyAccepted`) already model the field and the distinction end to end.
- `tests/unit/projections/handoff.test.ts:80-105` already decodes a hand-constructed synthetic
  envelope (`messageId: 'implicit-accept:h1'`, `implicit: true`) and asserts
  `getHandoff('h1')?.implicit === true`.

**No source change is needed anywhere under `src/`.** The gap is exactly one missing live test
case in an already-correct, already-well-designed CI job.

---

## Phases

### Phase 1 — add the implicit-accept scenario to the live-runtime integration suite

**Status: TODO**

**Delivers:** `tests/integration/runtime.test.ts` gains a
`'Handoff mode — implicit accept (timeout)'` describe block, closing the gap this repo's own
`integration.yml` was designed to prevent.

**Depends on:** nothing. `integration.yml` already runs on every push/PR; this phase only adds
a test case to a job that already exists and already pulls the right image.

**Files:**
- `tests/integration/runtime.test.ts` (new `describe` block, near the existing `'Handoff mode'`
  blocks at `:402-458`).
- `tests/integration/README.md`, if it enumerates covered scenarios.

**Approach.**

Mirror the existing happy-path block's session/policy setup
(`session = new HandoffSession(client, { auth: agentAlice })`, `:405-408`), but bind the
session to a policy built via `buildHandoffPolicy` (`src/policy.ts:447+`) with a short
`implicitAcceptTimeoutMs` (`:117`, `rules.acceptance?.implicitAcceptTimeoutMs`) instead of the
default. Offer a handoff, deliberately send **no** `acceptHandoff`/`declineHandoff`, and poll
(bounded, not a fixed `sleep`) until the projection reflects resolution. Assert:

- `handoff.status === 'accepted'`
- `handoff.implicit === true` (via `isImplicitlyAccepted(handoffId)`, `:224-231` — assert the
  accessor itself, not just the raw field, so the accessor's own logic is under test)
- `handoff.acceptedBy === targetParticipant`
- the delivered envelope's `messageId === \`implicit-accept:${handoffId}\`` (this suite already
  has access to raw `IncomingMessage`s per the adjacent tests in this file — reuse that path
  rather than adding a new one)

Also assert the runtime's own session state agrees (`GetSession` or equivalent), the same
cross-check the parallel Python-SDK plan requires, so the test does not merely prove "this
client's stream delivered something" but "the runtime's committed state matches."

**Rejected:** a unit-only test. `tests/unit/projections/handoff.test.ts:80-105` already is that
test and stays as-is; this phase's purpose is specifically closing the "never observed against a
real runtime" gap, which only an `tests/integration/` test can close.

**Edge cases & failure modes.**
- *Poll, don't sleep.* Match this file's existing idioms for waiting on asynchronous session
  state (check how the decline-path or another timing-sensitive block in this same file already
  does it, and reuse that helper rather than inventing a second polling idiom in one file).
- *`MACP_CLEANUP_INTERVAL_SECS`.* The synthetic accept may not appear the instant the configured
  timeout elapses — the runtime's eager sweep runs on that interval (default 60s) and the lazy
  path only fires when something else touches the session. A cheap `GetSession` poll after the
  timeout window is a reliable way to force the lazy path rather than waiting out the sweep;
  read `macp-runtime/CLAUDE.md`'s `MACP_CLEANUP_INTERVAL_SECS` entry before picking bounds.
  Since `:latest` runs with whatever default is baked into the image, do not assume the interval
  can be shortened via an env var on the shared `services:` container without checking
  `integration.yml`'s existing env block first.
- *Runtime older than 0.8.0.* Give the poll a hard ceiling with a failure message naming the
  version requirement — `:latest` always satisfies it going forward, but a contributor running
  this suite by hand against an older pinned binary should get a clear diagnosis, not a bare
  timeout.
- *Test isolation.* `MACP_MEMORY_ONLY: '1'` is already set on the shared service
  (`integration.yml:47`), so no additional cleanup is needed beyond what the existing Handoff
  blocks already do.

**Acceptance criteria.**
1. Reverting `crates/macp-modes/src/mode/handoff.rs:404`'s `session.semantics_rev < 2` guard to
   unconditionally return `None` (simulating a pre-0.8.0 runtime) turns this test red — verify
   this once against a scratch runtime build, the same way this repo's Phase 3 (cardinality
   triage, `plans/cross-repo/macp-sdk-typescript-conformance-and-transport.md`) requires a gate
   be observed failing before it is trusted.
2. The test passes against `ghcr.io/.../macp-runtime:latest` today.
3. `isImplicitlyAccepted()` is asserted `true` specifically, not just `isAccepted()`.
4. `npm run test:coverage` thresholds (`vitest.config.ts:40-45`) still pass.
5. No file under `src/` is modified — `git diff --stat` confirms it.

**Tests.** This phase's deliverable is itself a test.

**Docs.** `tests/integration/README.md` if it lists covered scenarios; no `CHANGELOG.md` entry
(test-only, no behavior change in this repo).

---

## Long-term posture

**No breaking risk and no one-way door.** The wire contract was already modeled before this
runtime version made it reachable; this phase only proves the existing model against reality.

**The behavior visible to any consumer of this SDK changes once they run against
`macp-runtime >= 0.8.0`**, independent of this SDK's own version: a handoff that previously sat
at `status: 'offered'` past its timeout forever will now resolve to
`status: 'accepted', implicit: true`. Worth one line in this repo's own release notes the next
time its documented minimum/tested runtime version is bumped past 0.8.0, even though this
phase changes no `src/` file — so a consumer reads it as an intentional behavior note, not
silently.

## Open questions

None with a genuine fork.

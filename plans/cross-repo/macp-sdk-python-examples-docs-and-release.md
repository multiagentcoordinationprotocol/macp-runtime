# PLAN — runnable examples, doc correctness, fixture vendoring, and release hygiene (macp-sdk-python)

**Verified against:** `macp-sdk-python` at `25133d2` (`chore(main): release 0.8.0 (#46)`, 2026-08-31 13:24 -0700) on `main`, **clean tree**. Every `file:line` below was read, not recalled.
**Spec checkout verified against:** `multiagentcoordinationprotocol` at `110add2` (2026-08-31). Read-only.
**Runtime evidence:** examples were executed for real against `macp-runtime 0.7.0` (locally built `target/debug/macp-runtime`, `MACP_ALLOW_INSECURE=1 MACP_MEMORY_ONLY=1`, port 50451). Every "broken" and every "fixed" claim below is an observed exit code, not a reading.
**Write scope:** this plan is authored in `macp-runtime`; **all work it describes lands in `macp-sdk-python`.** No file in any sibling repo was modified while writing it.

---

## Context

### The brief's four items, checked against the code

Three of the four hold. One is entirely stale.

| Brief item | Verdict |
|---|---|
| #49 — `quorum_approval.py` raises on auth/sender mismatch | **Holds, and understates the problem by 5 more defects** |
| #50 — two factual errors in `docs/modes/quorum.md` | **Holds; a third instance of error 2 exists at `:133`** |
| #51 — vendor Objection/Withdraw/TaskUpdate fixtures | **Holds; still upstream-blocked** |
| "cut the 0.8.0 release" | **Stale — 0.8.0 shipped to PyPI before this plan was written** |

### The release premise is stale

The brief states `pyproject.toml` declares `0.7.0` and `git describe --tags` reports `v0.7.0-8-g<sha>`. Neither is true at `25133d2`:

- `pyproject.toml:7` reads `version = "0.8.0"`; `.release-please-manifest.json` reads `{".": "0.8.0"}`.
- `git describe --tags` reports exactly `v0.8.0`. HEAD **is** the release commit.
- `gh release list` shows `v0.8.0 Latest 2026-08-31T20:24:28Z`.
- PyPI `macp-sdk-python` `info.version` is `0.8.0`; `releases` keys end `…, '0.7.0', '0.8.0'`.

So the SDK-divergence concern that motivated the phase — "until Python releases, the two SDKs disagree on vote/ballot cardinality" — **is already resolved.** `git show 25133d2` confirms release-please generated the `## [0.8.0]` section including the `⚠ BREAKING CHANGES` entry for `feat!: first accepted vote or ballot stands (#48)`, plus `#47` and `#45`. 0.8.0 was the correct number under semver and release-please picked it without help.

What *is* left from that phase is smaller but real, and it will recur every release: **release-please did not consume the hand-written `## Unreleased` section.** `CHANGELOG.md:3` still opens `## Unreleased` and runs 157 lines describing exactly the three changes that shipped in 0.8.0; release-please inserted its generated `## [0.8.0]` section *below* it, at `CHANGELOG.md:162`. The file now claims shipped work is unreleased, and every future release will bury its own section one block further down. That is Phase 6.

### The examples are worse than #49 reports

#49 names one broken example and one broken doc page. Executing all nine examples against a live runtime found **six broken examples in three distinct failure classes**, only one of which #49 describes.

`AuthConfig.for_dev_agent(agent_id)` sets `expected_sender=agent_id` unconditionally (`src/macp_sdk/auth.py:39-43`). `BaseSession._sender_for` (`src/macp_sdk/base_session.py:63-70`) raises `MacpIdentityMismatchError` at `:68` whenever an explicit `sender=` disagrees with the effective `AuthConfig`, resolved `method > session > client`. Passing `sender=` without a matching `auth=` is therefore always fatal.

**Class A — auth/sender mismatch (4 examples).** Observed, not inferred:

| File | Call site | Observed |
|---|---|---|
| `examples/quorum_approval.py` | `:34-37` under session auth `coordinator` (`:18`) | `MacpIdentityMismatchError: sender 'alice' does not match auth identity 'coordinator'` |
| `examples/handoff_escalation.py` | `:41` under session auth `owner-a` (`:18`) | `… sender 'owner-b' does not match auth identity 'owner-a'` |
| `examples/proposal_negotiation.py` | `:28`, `:31-37`, `:40`, `:42` under session auth `coordinator` (`:20`) | `… sender 'seller' does not match auth identity 'coordinator'` |
| `examples/task_delegation.py` | `:34`, `:37`, `:41` under session auth `planner` (`:18`) | `… sender 'worker' does not match auth identity 'planner'` |

`examples/decision_smoke.py:26-36` shows the correct shape — `sender="alice", auth=AuthConfig.for_dev_agent("alice")` — and runs clean (exit 0). So does `examples/policy_registration.py:88-119` for its evaluate/vote calls, and `examples/direct_agent_auth_initiator.py` (exit 0).

**Class B — wrong API, which the compile-only smoke test structurally cannot catch (2 examples).** These are the defects `tests/unit/test_examples_smoke.py`'s own docstring admits it misses ("renamed APIs won't be caught here"), and they are the strongest argument in this plan for the execution gate:

- `examples/policy_registration.py:72` calls `got.descriptor.policy_id`. `GetPolicyResponse` has exactly one field, `policy_descriptor` (verified via `policy_pb2.GetPolicyResponse.DESCRIPTOR.fields`; `MacpClient.get_policy` at `src/macp_sdk/client.py:801-814` returns it unwrapped). Observed: `AttributeError: descriptor`. This is the *only* occurrence of the wrong name in the repo.
- `examples/task_delegation.py:46` calls `proj.is_completed()`. The signature is `TaskProjection.is_completed(self, task_id: str)` (`src/macp_sdk/task.py:176-177`). Observed: `TypeError: TaskProjection.is_completed() missing 1 required positional argument: 'task_id'`.

**Class C — a mode-semantics defect that only a live runtime reveals (1 example).** After Class A was patched, `examples/proposal_negotiation.py` still failed at its `session.commit(...)` with `MacpAckError: INVALID_ENVELOPE: InvalidPayload`. Cause: `participants=["coordinator", "buyer", "seller"]` (`:23`) but the coordinator never accepts. The runtime's proposal mode defaults `acceptance.criterion` to `all_parties` (`macp-runtime/crates/macp-modes/src/mode/proposal.rs:98-111`) and gates `Commitment` on `commitment_ready` requiring phase `Converged` (`:166-171`, `:353-358`). Meanwhile the SDK's `ProposalProjection.accepted_proposal()` (`src/macp_sdk/proposal.py:137-144`) returns `"p2"` as soon as all *accepting* senders agree — it never consults the participant list. **The client-side projection and the runtime disagree about convergence**, and the example sits exactly on the gap. Removing `"coordinator"` from `participants` makes it pass (the initiator retains `Commitment` authority regardless of participant membership, per `CLAUDE.md` §2 / RFC-MACP-0007 §2). Verified: exit 0, `Negotiation resolved: contract.agreed`.

All six fixes were applied to scratchpad copies and re-executed. Final state: `decision_smoke`, `quorum_approval`, `handoff_escalation`, `proposal_negotiation`, `task_delegation`, `policy_registration`, `direct_agent_auth_initiator` all exit 0; `agent_policy_aware` exits 0 with no runtime at all (it uses `MagicMock(spec=MacpClient)` at `:93-96`).

### The docs carry the same defects, on five pages, not one

#49 names `docs/modes/quorum.md:61-83`. The same auth/sender defect is on **every** mode page, and Classes B and C are there too:

| Page | Defect |
|---|---|
| `docs/modes/quorum.md:61-83` | Class A (`session = QuorumSession(client)` at `:62`, then `sender="alice"` at `:79-83`) |
| `docs/modes/proposal.md:60-86` | Class A **and** Class C (`participants=["coordinator", "buyer", "seller"]` at `:64` with `all_parties` convergence) |
| `docs/modes/proposal.md:130-140` | Class A |
| `docs/modes/task.md:97-131` | Class A **and** Class B (`proj.is_completed()` at `:131`) |
| `docs/modes/task.md:159` | Class B (`proj.is_completed()  # True if TaskComplete received`) |
| `docs/modes/handoff.md:59-83`, `:125-129` | Class A |
| `docs/modes/decision.md:55-81`, `:138-156` | Class A |

`README.md:59-63` is **correct** (`sender="alice", auth=AuthConfig.for_dev_agent("alice")`). `docs/auth.md` and `docs/guides/direct-agent-auth.md` are correct — several of their `sender=` lines are deliberate negative examples demonstrating the guardrail.

### #50 is right on both counts, and there is a third instance

**Error 1 — fractional threshold does not raise.** Confirmed empirically: `build_quorum_policy("p","d", threshold=QuorumThreshold(type="percentage", value=0.75))` returns a descriptor with `{"threshold": {"type": "percentage", "value": 0.75}}`. No exception. `src/macp_sdk/policy.py:189` checks only `t.value < 0` and `:191` only `t.type == "percentage" and t.value > 100`; `0.75` passes both. `QuorumThreshold.value` is a plain `int`-annotated field (`:162`) with no runtime enforcement. So `docs/modes/quorum.md:46`'s "a fractional `0.75` … raises `MacpSessionError`" is false.

Two things #50 does not mention, both of which change the fix:

- **`tests/unit/test_policy.py:182-195` actively asserts the pass-through**: `test_custom` constructs `QuorumThreshold(type="percentage", value=0.75)` and asserts `rules["threshold"]["value"] == 0.75`. Any fix that adds validation must edit this assertion. Reading it, the `0.75` is incidental — `test_custom` is a generic round-trip test, and its sibling `tests/unit/test_absorb_runtime_v050.py:118` uses the *correct* `value=75` for a percentage while `:130`/`:134` assert the range checks raise. It is a careless value, not a deliberate lenient-typing contract.
- **The TypeScript SDK already validates this.** `macp-sdk-typescript/src/policy.ts:190-199` throws `MacpSessionError` when a `percentage` threshold is `!Number.isInteger(v) || v < 0 || v > 100`. And the canonical schema is unambiguous: `multiagentcoordinationprotocol/schemas/json/policy/quorum-rules.schema.json` declares `"value": {"type": "integer", "minimum": 0}` for **all** threshold types, with the `percentage` conditional adding only `maximum: 100`. So this is not a fresh behaviour question — **Python is the outlier**, and the descriptor it currently produces is invalid against the normative schema.

**Error 2 — `total_eligible` is wrong, and the comment states the wrong rule.** `docs/modes/quorum.md:88` reads `total_eligible = 5  # all participants except coordinator` while `:65` lists six participants (`coordinator, alice, bob, carol, dave, eve`). RFC-MACP-0011 §2.1 (`rfcs/RFC-MACP-0011-quorum-mode.md:36`), read in full, states: *"The session initiator (coordinator) is NOT an eligible ballot caster unless they are also listed in the `participants` array … The `participants` list defines the voter pool, which is distinct from the coordinator role."* Eligibility is conferred **solely** by `participants` membership. The coordinator *is* listed here, so the correct value is `6` — and the correct rule is `len(participants)`, never a rule about the coordinator role.

The undercount matters because `total_eligible` feeds `QuorumProjection.is_threshold_unreachable` (`src/macp_sdk/quorum.py:150-156`), which computes `remaining = total_eligible - len(voted_senders(...))` and returns `approval_count + remaining < required_approvals`. Undercounting shrinks `remaining` and fires "unreachable" early — a false negative outcome.

**Third instance, not in #50:** `docs/modes/quorum.md:133` repeats it — `proj.is_threshold_unreachable(request_id, total_eligible=5)  # False`. Fixing `:88` alone leaves the same wrong number one screen down.

Neither error appears anywhere else: `grep` for `total_eligible`, `except coordinator`, `fractional`, and `0.75` across `docs/`, `README.md`, and `src/` returns only `docs/modes/quorum.md:46/88/133` and `src/macp_sdk/policy.py:157`. The `policy.py:149-160` docstring is worded **correctly** ("rejected by the runtime's schema validation") — only the doc page overstates it into a client-side guarantee.

### CI today, and what a runnable-example gate would cost

- `ci.yml` runs `checks.yml` (lint / mypy / unit matrix on 3.11–3.13 / conformance replay) plus a `build` job.
- **Integration tests are deliberately excluded.** `ci.yml:16-18`: *"Integration tests are intentionally NOT run in CI: they need a live MACP runtime and stay a local gate (make test-integration; they auto-skip when no runtime is reachable)."*
- The skip mechanism already exists and is good: `tests/integration/conftest.py:31-41` is a session-scoped autouse fixture that TCP-probes `MACP_RUNTIME_TARGET` (default `127.0.0.1:50051`) and calls `pytest.skip` for the whole directory when nothing answers. `make_client(agent)` at `:22-28` is the shared dev-auth constructor. Marker `integration` is registered in `pyproject.toml:152-155` under `--strict-markers`.
- **There is 1,386 lines of integration test that has never run in CI.** Run against `macp-runtime 0.7.0`: **30 passed, 3 skipped in 3.04s.** The 3 skips are the Bearer-token variants gated on `MACP_INTEGRATION_BEARER_TOKEN` (`tests/integration/test_direct_agent_auth.py:39`). So turning CI on is low-risk, fast, and immediately valuable beyond examples.
- **A published runtime image exists and is anonymously pullable.** `ghcr.io/multiagentcoordinationprotocol/macp-runtime` issues an anonymous pull token and its tag list includes `latest`, `main`, and per-sha tags. **It must not be pinned by semver tag:** the list stops at `0.5.0`/`0.5`, with no `0.6.x` or `0.7.x`. That is the *same* GHCR/`GITHUB_TOKEN` defect documented in `macp-runtime/CLAUDE.md` for crates.io — release-plz's tags are created with the default token, so tag-triggered workflows (`docker.yml`, gated on `push: tags: ["v*"]`) never fire. `latest`/`main` are pushed on every push to `main` and *are* current.

### #51 is real, still blocked, and the harness has a specific limitation

- `Makefile:3-7` defines `SPEC_CONFORMANCE_DIR` and `FIXTURE_DIR_PAIRS := .:tests/conformance cmt-hash:tests/vectors/cmt-hash`; `verify-fixtures` (`:85-128`) fails on `DRIFT` (canonical file differs or missing locally), `EXTRA` (local file with no canonical source), or `MISSING` (canonical dir absent). `conformance-fixtures.yml:12-40` runs it on **every push and PR to main** against a fresh checkout of the spec repo. `make verify-fixtures` is green today.
- **Confirmed: the moment spec #81/#84 merge, every PR in this repo goes red** until `make sync-fixtures` is run and the result committed. Both are `OPEN` (`multiagentcoordinationprotocol#81` "Conformance corpus has zero fixtures for Objection, Withdraw, and TaskUpdate"; `#84` "Conformance corpus lacks reject-path fixtures for duplicate Vote / duplicate ballot").
- **Can the harness replay `"expect": "reject"` with `expected_error_code`? No.** `tests/conformance/test_conformance_projections.py:154-156` is `for msg in fixture["messages"]: if msg.get("expect") != "accept": continue`. `expected_error_code` — which *does* exist in the canonical schema at `schemas/conformance/schema.json:87` and is populated in e.g. `tests/conformance/decision_reject_paths.json` (`FORBIDDEN`, `FORBIDDEN`, `INVALID_ENVELOPE`) — is **never read anywhere in this repo**. This is deliberate and documented (`:5-7`, `:149-151`): rejection is the runtime's job, not a projection's. Consequence: vendoring reject fixtures for the three types exercises only their *accepted prefix*.
- **Are the three payloads decodable today? Yes, all three.** `proto_registry.py:38` maps `Objection → macp.modes.decision.v1.ObjectionPayload`, `:46` `Withdraw → …proposal.v1.WithdrawPayload`, `:52` `TaskUpdate → …task.v1.TaskUpdatePayload`. Projections consume all three: `projections.py:87-91` (Objection), `proposal.py:124-125` (Withdraw), `task.py:120-124` (TaskUpdate). `PAYLOAD_BUILDERS` is derived from `CORE_MAP`/`MODE_MAP` (`test_conformance_projections.py:40-61`), so the three resolve without harness changes. **Vendoring is mechanical.**
- **`has_blocking_objection` tests.** `src/macp_sdk/projections.py:165-174`: `any(objection.severity.lower() == "critical" and (proposal_id is None or objection.proposal_id == proposal_id) …)`. Five unit tests: `test_decision_projection.py:64-73` (critical ⇒ True), `:76-83` (`severity="low"` ⇒ False), `:160-178` (`"high"` ⇒ False, then `"critical"` ⇒ True), `:180-190` (`proposal_id=None` spans proposals), `:297-312` (re-applying the same envelope is a no-op and the predicate is unaffected). They pin **only the `critical`-is-the-sole-veto rule**. Nothing normative stands behind them, which is exactly #51's point.
- **The duplicate-ballot fixtures from #84 interact with a live gate.** `test_conformance_projections.py:173-196` asserts `not projection.has_anomalies` **and** that no `"projection anomaly"` WARNING was logged, across the whole corpus. Because `:154` skips non-accept messages, a duplicate marked `"expect": "reject"` is invisible and the gate stays green — the intended outcome. But if the spec authors mark a duplicate `"expect": "accept"`, this gate fires. That is a feature (it is precisely what the gate was written for), and the vendoring pass must read the new fixtures rather than assume.

### Collateral note

While gathering runtime evidence I ran `pkill -f "macp-runtime"`, which terminated a pre-existing `macp-runtime` process the user had listening on `127.0.0.1:50051` (it was configured with real auth, not dev-mode). It needs restarting by hand; its configuration was not captured.

---

## Phases

### Phase 1 — Execute the examples in the test suite, and fix the six that raise

**Status: TODO**

**Delivers:** `tests/integration/test_examples_run.py` — a parametrized test that runs each example as a subprocess against a live runtime and asserts exit 0 — plus the six example fixes it turns red. Closes the code half of #49.

**Depends on:** nothing.

**Files:**
- `tests/integration/test_examples_run.py` (new)
- `examples/quorum_approval.py`, `examples/handoff_escalation.py`, `examples/proposal_negotiation.py`, `examples/task_delegation.py`, `examples/policy_registration.py`
- `tests/unit/test_examples_smoke.py` (docstring + a coverage-parity assertion)
- `Makefile` (help text only, if the target list changes)

**Approach.**

*Reuse the existing runtime mechanism; do not invent a second one.* Put the new file in `tests/integration/` so it inherits `conftest.py:31-41`'s TCP probe and directory-wide skip verbatim, and mark it `pytestmark = pytest.mark.integration`. A developer with no runtime sees a skip, exactly as today; `make test-integration` (`Makefile:28-29`) picks it up with no edit. **Rejected:** a bespoke `tests/examples/` directory with its own runtime-detection logic — two probes that can disagree is the failure mode this repo already avoided once.

*Run examples as subprocesses, not imports.* Five examples execute at module top level (`quorum_approval.py:11`, `handoff_escalation.py:11`, `proposal_negotiation.py:11`, `task_delegation.py:11`, plus `agent_policy_aware.py`'s `main()` guard), so importing them performs I/O at import time and pollutes the pytest process with open channels. `subprocess.run([sys.executable, path], timeout=…, capture_output=True)` gives a clean exit code, a real traceback in `stderr` for the assertion message, and process isolation. **Rejected:** `runpy.run_path` — shares the interpreter, and a leaked gRPC channel would trip `pyproject.toml:156-163`'s `filterwarnings = ["error", …]`.

*Cover all nine examples, with exactly one explicit exclusion, declared in the test itself.* This is the part #49 warns about: a gate that silently covers a subset while reading as full coverage is the bug being fixed. Two mechanisms, both mandatory:

1. An explicit `EXCLUDED: dict[str, str]` mapping filename → reason, and a companion test asserting `{p.name for p in EXAMPLES} == RUN | EXCLUDED.keys()`. **A new example file is a hard failure until someone classifies it.** Mirror this in `tests/unit/test_examples_smoke.py`, whose `>= 9` count assertion at `:20` is the weaker existing version of the same idea.
2. `EXCLUDED` has exactly one entry:
   - `direct_agent_auth_observer.py` — *"requires a concurrently running `direct_agent_auth_initiator.py` sharing `MACP_SESSION_ID`; it blocks on `stream.responses(timeout=5.0)` (`:46`) and exits only on a Commitment (`:56-58`). The two-process handshake is covered by `tests/integration/test_direct_agent_auth.py` (182 lines, 30-test suite green)."*

   `agent_policy_aware.py` is **included** and needs no runtime — it builds `MagicMock(spec=MacpClient)` at `:93-96`. Run it unconditionally in the same parametrization; a runtime being present does not change its behaviour. **No example needs an API key or any external service** — Tier-3-style OpenAI dependencies do not exist in this repo.

*Point examples at `MACP_RUNTIME_TARGET`.* Every runtime-bound example hardcodes `127.0.0.1:50051`. Change each to `os.environ.get("MACP_RUNTIME_TARGET", "127.0.0.1:50051")`, matching the shape `direct_agent_auth_initiator.py:46` already uses. This keeps the documented default intact while letting CI and `make test-integration` retarget. **Rejected:** having the test rewrite source into a temp dir — that tests a copy, not the file users read.

*The six fixes, each verified by execution:*

| File | Fix | Verified |
|---|---|---|
| `quorum_approval.py:34-37` | add `auth=AuthConfig.for_dev_agent("<sender>")` to each ballot | exit 0, `Approvals: 3, Rejections: 1`, `Policy update approved via quorum` |
| `handoff_escalation.py:41` | add `auth=AuthConfig.for_dev_agent("owner-b")` | exit 0, `Handoff completed successfully` |
| `task_delegation.py:34,37,41` | add `auth=AuthConfig.for_dev_agent("worker")` | exit 0 (with the `:46` fix) |
| `task_delegation.py:46` | `proj.is_completed()` → `proj.is_completed("t1")` | exit 0, `Task completed and committed` |
| `policy_registration.py:72` | `got.descriptor` → `got.policy_descriptor` | exit 0, `retrieved: policy.deploy.majority-veto …` |
| `proposal_negotiation.py:28-42` | reuse the already-bound `buyer`/`seller` `AuthConfig`s from `:16-17` as `auth=` | needed but not sufficient |
| `proposal_negotiation.py:23` | `participants=["coordinator", "buyer", "seller"]` → `["buyer", "seller"]` | exit 0, `Negotiation resolved: contract.agreed` |

For `proposal_negotiation.py:23`, prefer dropping the coordinator from `participants` over adding a coordinator `accept`: the coordinator is a neutral convener in this scenario and has no business voting on the contract, and the initiator keeps `Commitment` authority regardless of participant membership. Add a one-line comment naming `all_parties` and pointing at `proposal.rs`'s convergence rule, so the next reader does not "helpfully" re-add the coordinator.

**Edge cases & failure modes.**
- *Shared runtime state.* `policy_registration.py` registers `policy.deploy.majority-veto` and unregisters at `:134`. If it fails mid-run the policy leaks and a re-run hits `ALREADY_EXISTS`. Give the subprocess a per-run `MACP_DATA_DIR`, or run CI's runtime with `MACP_MEMORY_ONLY=1`. Phase 2 does the latter.
- *Hang.* An example blocking on a stream would hang CI. Pass an explicit `timeout=` to `subprocess.run` (60s is generous; the whole current integration suite is 3s) and assert on `TimeoutExpired` with the example name.
- *Rate limits.* `MACP_SESSION_START_LIMIT_PER_MINUTE` defaults to 60 per sender. Eight examples, several sessions each, all as `coordinator` — comfortably under, but do not parallelize with `-n auto` without re-checking.
- *`filterwarnings = ["error"]`.* Subprocess isolation sidesteps this entirely; it is a reason to keep subprocesses even if imports later look tempting.
- *The gate must be able to go red before the fixes.* Author `test_examples_run.py` first, run it, confirm 6 failures with the exact messages tabulated above, then fix. A gate never observed failing is not a gate.

**Acceptance criteria.**
1. `MACP_RUNTIME_TARGET=<host:port> pytest tests/integration/test_examples_run.py -m integration` passes with 8 executed and 1 skipped-by-exclusion, against a runtime started with `MACP_ALLOW_INSECURE=1`.
2. With no runtime reachable, the same command reports skips and exit 0.
3. Reverting any one of the six fixes turns that example's case red with a message naming the file.
4. Adding a new `examples/*.py` with no `EXCLUDED` entry fails the coverage-parity test.
5. `ruff check src/ tests/ examples/` and `ruff format --check` pass (`checks.yml:29-30` lints `examples/`).
6. `mypy src/macp_sdk/` unaffected.

**Tests.**
- Happy path: one parametrized case per example, ids by filename.
- Failure path: assert the subprocess `stderr` is surfaced in the assertion message — verify by temporarily breaking one example and reading the pytest output.
- Coverage parity: `RUN | EXCLUDED == {p.name for p in EXAMPLES}`.
- Exclusion has a non-empty reason string (guards against a silent `EXCLUDED = {"x": ""}`).
- Timeout: assert `subprocess.run` is called with an explicit `timeout`.
- Update `tests/unit/test_examples_smoke.py`'s docstring: it currently claims examples "are exercised for real against a runtime in the docs/release flow", which was not true. After this phase it is true, and the docstring should name `tests/integration/test_examples_run.py`.

**Docs.** `docs/contributing.md` gains a line: examples are executed, not just compiled; run `make test-integration` before touching `examples/`. No user-facing doc change (the doc *content* fixes are Phase 4).

---

### Phase 2 — Run the integration suite in CI against a GHCR runtime container

**Status: TODO**

**Delivers:** Phase 1's harness enforced on every PR. Also switches on the 30 existing integration tests that have never run in CI. This phase is what makes #49 not recur; Phase 1 alone is a gate nobody runs.

**Depends on:** Phase 1.

**Files:** `.github/workflows/ci.yml`, `.github/workflows/checks.yml` (comment only), `docs/contributing.md`.

**Approach.**

Add an `integration` job to `ci.yml` (sibling of `checks`, not inside `checks.yml` — `checks.yml` is also called by `publish.yml:19-20`, and a release should not be blocked on pulling a container from GHCR).

Use a GitHub Actions **service container**:

```yaml
services:
  runtime:
    image: ghcr.io/multiagentcoordinationprotocol/macp-runtime:latest
    env:
      MACP_ALLOW_INSECURE: "1"
      MACP_MEMORY_ONLY: "1"
      MACP_BIND_ADDR: "0.0.0.0:50051"
    ports: ["50051:50051"]
```

Points that are load-bearing:

- **`MACP_ALLOW_INSECURE=1` is required.** The `Dockerfile` deliberately omits it (`:35-37`) and the runtime refuses to start without auth+TLS otherwise. `MACP_MEMORY_ONLY=1` gives each run a clean registry and moots the policy-leak edge case from Phase 1.
- **Do not pin a semver tag.** `ghcr.io/…/macp-runtime` has no `0.6.x` or `0.7.x` tags (see Context). Use `latest`, or better a pinned **sha tag** (the tag list carries per-commit sha tags) updated deliberately, matching this repo's existing habit of SHA-pinning every action. `latest` risks a runtime regression turning this repo red for reasons outside it; a sha pin makes the bump a reviewed commit. **Recommend the sha pin**, with a comment naming where to get a newer one.
- **Wait for readiness before pytest.** Service containers report "started" when the container starts, not when the gRPC server binds. Add a bounded TCP poll (`for i in $(seq 1 30); do nc -z localhost 50051 && break; sleep 1; done`) and fail loudly on exhaustion. Without it the run does not fail — it **silently skips**, because `conftest.py:38` calls `pytest.skip` on connection failure. That is the single most dangerous failure mode in this phase: a green CI that ran nothing.
- **Assert the suite actually ran.** Add `-p no:randomly --strict-markers` and, critically, a post-step asserting a non-zero collected/passed count (e.g. `pytest … | tee out; grep -qE '[0-9]+ passed' out`). A gate that can silently degrade to "0 selected" is the same class of bug as the compile-only smoke test.
- **Rejected:** building the runtime from source in CI (a full Rust release build; minutes, plus a `protoc` dependency, for a binary that is already published). **Rejected:** `docker run --detach` in a step — service containers get networking and lifecycle for free.

**Edge cases & failure modes.**
- *GHCR pull without credentials.* Anonymous pull was verified to work (token issued, tag list readable). If the package is ever made private this job breaks; the fix is `docker/login-action` with `GITHUB_TOKEN`, which is worth adding pre-emptively since `packages: read` is free.
- *arm64 vs amd64.* The image is built `linux/amd64,linux/arm64` (`macp-runtime/.github/workflows/docker.yml:58`); `ubuntu-latest` is amd64. Fine.
- *Bearer-token tests skip.* 3 of 33 skip without `MACP_INTEGRATION_BEARER_TOKEN` (`test_direct_agent_auth.py:39`). Optionally set `MACP_AUTH_TOKENS_JSON` on the service to light them up — **out of scope**, but note it: turning on auth would break every dev-header test, so it needs a second runtime service, not a config tweak on this one.
- *Runtime/SDK version skew.* `proto-drift.yml` already watches `macp-proto` daily. A runtime image ahead of the SDK is a new skew axis; the sha pin makes it a reviewed event rather than a surprise.

**Acceptance criteria.**
1. A PR touching `examples/` runs the `integration` job and it passes.
2. Deliberately breaking one example (e.g. reverting `policy_registration.py:72`) turns the `integration` job red, with the example name and traceback in the log.
3. Deliberately misconfiguring the service (drop `MACP_ALLOW_INSECURE`) turns the job **red at the readiness poll**, not green-with-skips.
4. The job log shows `33 passed` (or `30 passed, 3 skipped`) — never `no tests ran`.
5. `checks.yml` and `publish.yml` are unchanged in behaviour; a release does not depend on GHCR.
6. `ci.yml:16-18`'s comment is replaced with an accurate one.

**Tests.** The job is the test. Verify criteria 2 and 3 on a scratch branch before merging — a CI gate that has never been observed failing is not a gate. Keep the existing `make test-integration` target as the local equivalent, and add `make test-integration-docker` that starts the same pinned image locally so a developer can reproduce CI exactly.

**Docs.** `docs/contributing.md`: how to get a runtime (docker one-liner with the pinned image, or `cargo run` in `macp-runtime`), and that integration now gates PRs. Update `README.md` only if it claims integration tests are local-only.

---

### Phase 3 — Make `QuorumThreshold`'s integer contract real

**Status: TODO**

**Delivers:** `build_quorum_policy` rejects a non-integer threshold, restoring parity with the TypeScript SDK and with the canonical schema — so `docs/modes/quorum.md:46`'s claim becomes **true** rather than being weakened to match lenient code. Closes half of #50.

**Depends on:** nothing.

**Files:** `src/macp_sdk/policy.py`, `tests/unit/test_policy.py`, `CHANGELOG.md` (via commit message).

**Approach.**

The brief asks this to be argued either way and decided, and flags it as possibly a one-way door. **It is not, and the evidence is decisive.**

*Decision: add the check.* Three independent reasons:

1. **The canonical schema already forbids it.** `schemas/json/policy/quorum-rules.schema.json` declares `"value": {"type": "integer", "minimum": 0}`. A descriptor carrying `0.75` is invalid and the runtime rejects it at `RegisterPolicy` with `INVALID_POLICY_DEFINITION`. **No working code depends on the lenient path** — it produces only descriptors that fail later, further away, with a worse error.
2. **The TypeScript SDK already throws.** `macp-sdk-typescript/src/policy.ts:190-199`. Today's state is not "Python is permissive"; it is "the two SDKs disagree, and Python is the outlier." Leaving it is the choice that preserves a divergence.
3. **It is what the function already exists to do.** `policy.py:187-189`'s own comment: *"Match the canonical schema's constraints before the runtime does, so a bad descriptor fails immediately client-side instead of round-tripping to an INVALID_POLICY_DEFINITION from RegisterPolicy."* The integrality constraint is a canonical-schema constraint. Omitting it is an oversight, not a policy.

*Where:* in `build_quorum_policy`, beside the existing checks at `:189-194` — **not** in `QuorumThreshold.__post_init__`. The dataclass is exported (`__init__.py:76`, `:182`) and may reasonably be constructed for inspection or round-tripping without ever being built into a descriptor; the builder is the single validation site today and should stay so. **Rejected:** `__post_init__` (wider blast radius, two validation sites). **Rejected:** weakening the doc to match the code (leaves a real cross-SDK divergence and a worse error surface, to protect a path that produces only invalid output). **Rejected:** relying on mypy (catches nothing at runtime, and the descriptor is often built from JSON/config where types are dynamic).

*Scope of the check:* integrality for **all** threshold types, per the schema, not just `percentage`. TypeScript checks only `percentage` (`policy.ts:190`) and is therefore under-strict relative to the normative schema. Match the schema; open a follow-up issue on `macp-sdk-typescript` rather than propagating its gap. Reject `bool` explicitly (`isinstance(True, int)` is `True` in Python, and `QuorumThreshold(value=True)` should not silently mean `1`).

*Message:* mirror TypeScript's, which explains the consequence rather than just the rule — *"quorum threshold value must be an integer (e.g. 75 for 75%), got 0.75. The runtime computes the approval bar as ceil(value/100 × participants); a fractional value like 0.75 would round to a ~1% bar."*

*Semver:* `fix:` under Conventional Commits, which release-please turns into a patch bump. It is behaviour-visible, so the commit body must say so in plain words; release-please copies it into `CHANGELOG.md`. It is not `feat!` — nothing that worked stops working, because nothing worked.

**Edge cases & failure modes.**
- `tests/unit/test_policy.py:186` and `:192` must change `0.75` → `75`. Confirm the surrounding `test_custom` is a generic round-trip test (it is) and does not lose coverage — the sibling `test_absorb_runtime_v050.py:130/:134` already covers the range rejections.
- `bool` must be rejected before the `int` check passes it.
- Order the checks: type first, then `< 0`, then the `percentage > 100` cap. A `float` reaching a comparison is fine numerically but yields a confusing message.
- The `< 0` check at `:189` currently applies to all types; keep that, and keep `minimum: 0` parity.
- `policy.py:149-160`'s docstring says *"range-checked at build time"*, which will then be an understatement — tighten it to say the builder rejects non-integer and out-of-range values.

**Acceptance criteria.**
1. `build_quorum_policy(..., threshold=QuorumThreshold(type="percentage", value=0.75))` raises `MacpSessionError`.
2. Same for `type="n_of_m"` and `type="weighted"`.
3. `QuorumThreshold(value=True)` raises.
4. `value=75` / `value=3` / `value=0` still succeed and produce byte-identical `rules` JSON to today.
5. The existing `< 0` and `> 100` rejections are unchanged (`test_absorb_runtime_v050.py:130/:134` still pass).
6. `mypy src/macp_sdk/` clean; coverage stays above `fail_under = 85` (`pyproject.toml:173`).

**Tests.** In `tests/unit/test_policy.py`: parametrized rejection over `(type, value)` for `0.75` across all three types plus `True`; acceptance for `0`, `3`, `75`, `100`; a JSON-byte-equality assertion for the accepting cases against the pre-change output. Amend `test_custom` at `:182-195` to `value=75`.

**Docs.** `docs/modes/quorum.md:46` is corrected in Phase 4 — after this phase the sentence is true as written and only needs the "range-checks" wording sharpened to "rejects non-integer and out-of-range values". Ordering matters: fix the code first so the doc is never briefly correct-but-unsupported.

---

### Phase 4 — Documentation correctness sweep across all five mode pages

**Status: TODO**

**Delivers:** `total_eligible` derived from `len(participants)` with the correct rule stated; the threshold sentence aligned to Phase 3's behaviour; and the Class A/B/C defects fixed on every mode page, not just quorum. Closes the rest of #50 and the docs half of #49.

**Depends on:** Phase 1 (reuse the corrected example text) and Phase 3 (the threshold sentence must describe shipped behaviour).

**Files:** `docs/modes/quorum.md`, `docs/modes/proposal.md`, `docs/modes/task.md`, `docs/modes/handoff.md`, `docs/modes/decision.md`.

**Approach.**

*`docs/modes/quorum.md:46` — threshold.* After Phase 3 the claim is true. Rewrite the parenthetical to state precisely what the builder does: rejects a non-integer value, a negative value, and a `percentage` above 100 — and keep the existing, correct framing that `threshold` is the approval bar and not a participation quorum.

*`docs/modes/quorum.md:88` and `:133` — eligibility.* Replace the literal with a derivation and restate the rule from the RFC:

```python
# Every participant in the SessionStart list is an eligible ballot caster.
# RFC-MACP-0011 §2.1: the coordinator is eligible only because it appears in
# `participants` — the coordinator role itself confers nothing.
participants = ["coordinator", "alice", "bob", "carol", "dave", "eve"]
total_eligible = len(participants)  # 6
```

Both sites must move together, and `:133`'s inline `total_eligible=5` must become `total_eligible=6` (or reference the variable). **Fixing the number without fixing the comment leaves the wrong rule for the next reader to copy** — that is the half #50 calls more important.

*Class A across five pages.* Apply the `sender=` + matching `auth=` pairing that `README.md:59-63` and `decision_smoke.py:26-36` already model, at: `quorum.md:79-83`, `proposal.md:69-82` and `:130-140`, `task.md:115-126`, `handoff.md:83` and `:125-129`, `decision.md:73-81` and `:146-156`. Where a page constructs `session = XSession(client)` with no `auth=`, bind per-agent `AuthConfig`s once near the top (as `proposal_negotiation.py:16-17` does) so the calls stay readable.

*Class B — `task.md:131` and `:159`.* `proj.is_completed()` → `proj.is_completed("t1")`; the `:159` reference line becomes `proj.is_completed(task_id)`.

*Class C — `proposal.md:64`.* Same coordinator-in-`participants` convergence trap as the example. Apply the same fix and the same explanatory comment.

**Rejected:** extracting and executing doc snippets (a `pytest-codeblocks`-style gate). It is the theoretically right answer to "docs rot the same way examples do", but these snippets are deliberately partial — `decision.md:138` opens mid-scenario with an undefined `coordinator_auth`, `quorum.md:197-215` is a `while` loop with `# ... collect votes asynchronously ...`. Making them all executable would mean rewriting them into worse documentation. **The defensible mitigation is different and cheaper:** make each mode page's primary example block a near-copy of the corresponding `examples/*.py`, which Phase 2 *does* execute, and say so in a comment. Record this as accepted debt in Long-term posture rather than pretending it is closed.

**Edge cases & failure modes.**
- `notify-website.yml` fires a `repository_dispatch` to the `website` repo on any push touching `docs/**`. Expected; not a failure, but the website will re-sync.
- `mkdocs.yml` builds these pages with `mkdocstrings`; a malformed fence breaks the docs build. Run `mkdocs build --strict` locally.
- `docs/auth.md:53-54`, `:110-113` and `docs/guides/direct-agent-auth.md:114-116` are **intentional** mismatch demonstrations. Do not "fix" them. A blind `sender=` sweep would destroy the guardrail documentation.
- Cross-check the corrected quorum arithmetic: with 6 eligible, 5 ballots cast (3 approve / 1 reject / 1 abstain) and `required_approvals=3`, `has_quorum` is `True` and `is_threshold_unreachable` is `False`. The narrative numbers at `:127-137` must still line up.

**Acceptance criteria.**
1. `grep -n "total_eligible" docs/modes/quorum.md` shows no literal `5`; the derivation and the RFC-sourced rule appear at `:88`.
2. No `sender=` appears in `docs/modes/*.md` without a matching `auth=` **or** an adjacent sentence marking it a deliberate failure demo.
3. `grep -rn "is_completed()" docs/` returns nothing.
4. `proposal.md`'s primary block does not list the coordinator in `participants` (or the coordinator accepts).
5. `docs/modes/quorum.md:46` describes exactly what Phase 3 implemented — no more, no less.
6. `mkdocs build --strict` succeeds.
7. Every corrected block that mirrors an example is byte-comparable to the Phase 1 file modulo the target address.

**Tests.** No new automated tests (see the rejected snippet-execution option). The manual check is: paste each corrected primary block into a scratch file, point it at a local runtime, and confirm it exits 0. Do this for all five pages — it is the same 5-minute loop that found six defects here.

**Docs.** This phase *is* the docs. Note in the PR body that `docs/modes/*.md` primary blocks now track `examples/*.py`, which are executed by CI.

---

### Phase 5 — Vendor the canonical Objection / Withdraw / TaskUpdate and duplicate-ballot fixtures

**Status: TODO — BLOCKED on `multiagentcoordinationprotocol#81` and `#84` (both OPEN). Sequence only; do not start.**

**Delivers:** the new canonical fixtures vendored into `tests/conformance/`, CI drift-green again, and a written assessment of whether `has_blocking_objection` agrees with the canonical corpus. Closes #51.

**Depends on:** upstream merge. Independent of every other phase — and **must not gate the release** (Phase 6), since it is `test:`-only and would not bump a version anyway.

**Files:** `tests/conformance/*.json` (added by `make sync-fixtures`, never hand-edited), possibly `tests/conformance/test_conformance_projections.py`, possibly `src/macp_sdk/projections.py`.

**Approach.**

*Trigger.* `conformance-fixtures.yml` runs `make verify-fixtures` on every push and PR. The instant #81/#84 merge, **every open PR in this repo goes red** with `DRIFT: tests/conformance/<new>.json differs from (or is missing vs) canonical`. That is the intended alarm, but treat it as a known incoming event, not a mystery — the first person to see it should reach for `make sync-fixtures`, not a bisect.

*Mechanical steps.* Clone/refresh the spec repo as a sibling, `make sync-fixtures`, `git diff tests/conformance/`, `make verify-fixtures` (expect green), `make lint-fixtures` (runs the spec repo's own `lint_fixtures.py`), then `pytest tests/conformance/ -m conformance`. `sync-fixtures` **copies but never deletes** (`Makefile:83`); if upstream renames a fixture, the old file is flagged `EXTRA` and must be removed by hand.

*What the harness will and will not do.* No harness change is needed for the payloads: all three resolve through `PAYLOAD_BUILDERS` (`test_conformance_projections.py:40-61`) via `proto_registry.py:38/46/52`, and all three are already consumed by projections (`projections.py:87-91`, `proposal.py:124-125`, `task.py:120-124`). But `:154-156` skips every non-`accept` message and `expected_error_code` is read nowhere, so **reject-path fixtures contribute only their accepted prefix.** Do not report "conformance green" as "reject semantics verified". If the new fixtures are predominantly reject-path, the honest outcome of this phase may be *"vendored, corpus green, and `has_blocking_objection` remains unvalidated by anything normative"* — which is a legitimate result to write down, and better than a false claim of closure.

*The duplicate-ballot gate (#84).* `:173-196` asserts zero anomalies and zero anomaly WARNINGs across the corpus. A duplicate marked `"expect": "reject"` is skipped and the gate stays green. A duplicate marked `"expect": "accept"` fires it. **Read the merged fixtures before assuming which.** If the gate fires, the first question is whether upstream intended an accepted-path duplicate (a spec question, raise it there) — the comment at `:167-172` already says exactly this. Do not weaken the gate to make CI green.

*Optional follow-on, decided after reading the fixtures:* if `expected_error_code` reject fixtures turn out to be the majority of what #81 delivers, consider teaching the harness to assert that reject-path messages are *not* reflected in projection state (a weaker but real check the SDK can actually make). Scope that only if the fixtures justify it.

**Edge cases & failure modes.**
- Vendoring is byte-exact by design; hand-editing a fixture to make a test pass is the one thing `verify-fixtures` exists to prevent.
- A new fixture may use a mode with no SDK projection — `:139-144` skips `multi_round` cleanly, so this degrades gracefully.
- `test_fixtures_validate_against_vendored_schema` (`:282-297`) validates against the vendored `schema.json`, which `sync-fixtures` also refreshes. If upstream changes the schema and the fixtures together, both arrive in one sync.
- `test_vendored_schema_matches_canonical_pattern` (`:244-254`) pins `PAYLOAD_TYPE_RE` (`:35-37`) against the schema's `payload_type` pattern. A canonical pattern change means a coordinated harness edit.
- `tests/unit/test_fixture_drift_gate.py` (357 lines) drives the real Makefile recipes against synthetic trees and needs no change.

**Acceptance criteria.**
1. `make verify-fixtures` green with the new fixtures present locally.
2. `make lint-fixtures` green.
3. `pytest tests/conformance/ -m conformance` green, with the new fixtures visibly in the parametrized ids.
4. `conformance-fixtures.yml` green on the PR.
5. No fixture differs by even a byte from canonical.
6. The PR body states explicitly, for each new fixture, whether it exercised accepted-path behaviour or only contributed a prefix — and whether `has_blocking_objection` was actually validated.

**Tests.** No new tests unless a fixture reveals a projection divergence. If it does, that divergence is the finding and gets its own issue and fix — do not bury it in the vendoring commit.

**Docs.** If `has_blocking_objection` semantics change, update `docs/modes/decision.md:110` and `src/macp_sdk/projections.py:165-168`. Otherwise none.

---

### Phase 6 — Reclaim the CHANGELOG and let release-please cut 0.8.1

**Status: TODO**

**Delivers:** a `CHANGELOG.md` that does not label shipped work as unreleased, and the next patch release carrying Phases 1–4. Replaces the stale "cut 0.8.0" phase.

**Depends on:** Phases 1, 3, 4 merged. **Explicitly NOT Phase 5** — see below.

**Files:** `CHANGELOG.md`. No `pyproject.toml` edit, no tag, no manual PyPI step.

**Approach.**

*The release mechanism, as it actually is.* Do not invent a process:

- `release-please.yml` runs on every push to `main`, mints a `macp-deps-bot` App token, and runs `googleapis/release-please-action@45996ed` with `release-please-config.json` (`release-type: python`, `bump-minor-pre-major: true`, `include-component-in-tag: false`) and `.release-please-manifest.json`. It opens/updates a release PR; merging it bumps `pyproject.toml` + the manifest, rewrites `CHANGELOG.md`, tags `vX.Y.Z`, and creates a GitHub Release.
- `publish.yml` triggers on `release: published` **and** `push: tags: v*`. It re-runs the full `checks.yml` gate, guards that the tag matches `pyproject.toml` (`:40-48`), builds, and publishes to PyPI via **trusted publishing (OIDC)** under the `pypi` environment (`:59-78`). The App token is what makes the release event fire at all — `GITHUB_TOKEN` would not, which is the same trap documented in `macp-runtime/CLAUDE.md`.
- **This works.** 0.8.0 went main → release PR → tag → Release → PyPI without intervention. Nothing needs fixing.

*The one real defect.* `CHANGELOG.md:3` is a hand-written `## Unreleased` block, 157 lines, describing exactly what shipped in 0.8.0. release-please does not recognise it and inserted `## [0.8.0]` **below** it at `:162`. Left alone, every future release nests further down under a permanently-wrong "Unreleased" banner.

*The fix.* Fold the hand-written block into the `## [0.8.0]` section it describes. The hand-written prose is genuinely better than release-please's terse commit list — it explains the migration, names the RFC rules, and gives the concrete `required_approvals=1` stake. Keep the prose; move it under the `## [0.8.0]` heading, below the generated `### ⚠ BREAKING CHANGES` / `### Features` / `### Bug Fixes` lists. Then delete the `## Unreleased` heading entirely and **do not reintroduce one** — release-please owns this file's top. Note the convention in `docs/contributing.md` so the next contributor writes a rich commit body instead of a hand-edited CHANGELOG section (release-please copies commit bodies through, which is how the prose gets in next time).

*Version number.* Under Conventional Commits with `bump-minor-pre-major: true`:
- Phase 1's example fixes → `fix:` → patch.
- Phase 3's threshold validation → `fix:` → patch.
- Phase 4 → `docs:` → changelog-visible, no bump.
- Phase 2 → `ci:` → no bump.
- Phase 6's CHANGELOG surgery → `chore:` → no bump.

So **0.8.1**, chosen by release-please, not by hand. `0.9.0` would be wrong — nothing here is a feature. The brief's question "is 0.8.0 the correct number under semver given the `feat!`" is answered historically: yes, and it already shipped. For 0.x, release-please maps `feat!` to a minor bump rather than a major, which is the standard 0.x convention and what produced 0.7.0 → 0.8.0.

*Sequencing vs. Phase 5 — release first, do not wait.* Three reasons: (a) Phase 5 is blocked on two OPEN upstream issues with no ETA, and holding a user-facing fix release behind someone else's backlog is indefensible; (b) Phase 5 is `test:`-only and would not bump the version anyway, so waiting buys literally nothing; (c) Phase 5's own trigger is a CI-red event that demands a fast, isolated response — it should not arrive tangled with a release PR. **Release after Phase 4, whenever Phase 5 lands.**

*Sequencing vs. Phase 2.* Phase 2 changes only `.github/workflows/`; it can land before or after the release. Prefer **before**, so the 0.8.1 release PR is itself validated by the new gate.

**Edge cases & failure modes.**
- Editing `CHANGELOG.md` by hand can confuse release-please's next diff. Do it as a standalone `chore:` PR with no other changes, then confirm the *next* release PR produces a sane file before merging it.
- `publish.yml:40-48` fails the build if the tag and `pyproject.toml` disagree — a safety net, and a reason never to hand-bump `pyproject.toml:7`.
- `skip-existing: true` (`:78`) makes a re-run of an already-published version a no-op, so a retry is safe.
- Trusted publishing pins repo + workflow filename + environment name on PyPI's side. **Do not rename `publish.yml`, move the `publish` job to another file, or rename the `pypi` environment** — the comment at `:56-58` says so and it is correct. This is the one genuine one-way door in the release path.
- Verify PyPI actually received 0.8.1 (`pypi.org/pypi/macp-sdk-python/json` → `info.version`). 0.8.0 succeeded, but `macp-runtime`'s history shows a tagged release silently never reaching its registry; check, do not assume.

**Acceptance criteria.**
1. `CHANGELOG.md` has no `## Unreleased` heading; the 0.8.0 prose sits under `## [0.8.0]`.
2. The next release PR that release-please opens contains a well-formed `## [0.8.1]` section immediately above `## [0.8.0]`, with the Phase 1/3 fixes listed.
3. Merging it produces tag `v0.8.1`, a GitHub Release, and a green `publish.yml` run.
4. `pypi.org` reports `0.8.1` as latest.
5. `pyproject.toml:7` and `.release-please-manifest.json` both read `0.8.1`, changed only by the bot.
6. No human pushed a tag and no human edited `pyproject.toml:7`.

**Tests.** `make build` (`Makefile:43-47`) locally: `python -m build` + `twine check dist/*`, mirroring `ci.yml:34-36`. `pip install macp-sdk-python==0.8.1` in a clean venv and import `macp_sdk` after publish.

**Docs.** `docs/contributing.md`: releases are automated; write good commit bodies rather than editing `CHANGELOG.md`; never hand-bump the version.

---

## Long-term posture

**Debt accepted, knowingly.**

- **Doc snippets are still unexecuted.** Phase 4 fixes today's defects but not the mechanism that let them appear. The mitigation — mode-page primary blocks mirroring executed `examples/*.py` — is a convention, not a gate, and conventions decay. The real fix is a snippet-extraction harness with per-block opt-in markers; it is more machinery than the current defect rate justifies, but revisit if a doc-only defect recurs after Phase 4.
- **Reject-path conformance is structurally unreachable from this SDK.** `expected_error_code` will keep going unread for as long as projections are the only thing under test (`test_conformance_projections.py:5-7`). The canonical corpus can grow arbitrarily rich in reject paths and this repo's conformance signal will not strengthen. Closing that means testing the SDK against a live runtime the way `macp-runtime/integration_tests/` does — a much larger programme, and arguably the runtime's job.
- **`ProposalProjection.accepted_proposal()` disagrees with the runtime.** `src/macp_sdk/proposal.py:137-144` returns a converged proposal from accepting senders alone; the runtime requires `all_parties` over the declared participants (`proposal.rs:98-111`). Phase 1 routes the example around this rather than fixing it, because fixing it means the projection must know the participant list (which it currently does not) and must honour a policy-configurable `acceptance.criterion` it cannot see. **File this as its own issue.** It is a genuine client/server semantic divergence of the same family as #51, and it bit an example.
- **The GHCR runtime image has no current semver tags.** `0.6.x`/`0.7.x` were never pushed because release-plz's tags are made with `GITHUB_TOKEN` and do not trigger `docker.yml` — the identical root cause as the crates.io gap in `macp-runtime/CLAUDE.md`. Phase 2 works around it with a sha pin. The real fix belongs in `macp-runtime`: **file an issue there.**
- **TypeScript validates threshold integrality only for `percentage`** (`policy.ts:190`), while the schema requires it for all types. After Phase 3, Python is stricter and schema-correct. File a parity issue on `macp-sdk-typescript`.

**One-way doors.**

- **PyPI trusted publishing config** is pinned to repo + `publish.yml` filename + `pypi` environment name. Renaming any of the three silently breaks publishing and cannot be fixed from this repo. Treat `publish.yml`'s name and the `publish` job's location as immutable.
- **`QuorumThreshold` validation (Phase 3)** is the only behaviour change in this plan. It is *not* a real one-way door: the input it starts rejecting produces descriptors the runtime already rejects, TypeScript already rejects it, and the normative schema forbids it. But it is the one change that could break a caller's code path at runtime, so it must be named in the CHANGELOG rather than slipped in as a tidy-up.
- **Turning integration tests on in CI (Phase 2)** makes this repo's green build depend on GHCR availability and on the runtime image's behaviour. That coupling is the point, but it means a `macp-runtime` regression can now block a `macp-sdk-python` merge. The sha pin converts that from an ambush into a reviewed bump; do not switch it to `latest` for convenience.

---

## Open questions

Only forks with no defensible default. Everything else above is decided.

1. **Should Phase 2 pin the runtime image by sha or track `latest`?** The plan recommends a sha pin, and the reasoning is written down — but it is a genuine values trade, not a fact. A sha pin means SDK CI never breaks from a runtime change, and also that the SDK can drift silently behind runtime behaviour until someone bumps it. `latest` means the SDK finds out immediately and pays with occasional unrelated red builds. Which failure the maintainer prefers depends on how tightly these two repos are meant to move together, and that is not knowable from the code.

2. **Should `MACP_INTEGRATION_BEARER_TOKEN` be configured in CI so the 3 skipped auth tests run?** It needs a *second* runtime service with `MACP_AUTH_TOKENS_JSON`, because enabling static tokens on the shared one would break every dev-header test in the 30-test suite. That is real complexity for three tests, and the call depends on how much the Bearer path is trusted from `tests/unit/test_auth.py` alone. Not decided here.

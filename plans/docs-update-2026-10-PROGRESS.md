# docs-update-2026-10 — PROGRESS / repo map

Companion to `plans/docs-update-2026-10.md`. This file is the **repo map** a later
phase reads instead of re-scanning. Phase status is appended at the bottom as work
lands.

---

## 1. Doc files in scope (living docs only)

All paths relative to the repo root `/Users/Shared/multiagentcoordinationprotocol/macp-runtime`.

| File | Lines | Tracked by git? | State going in |
|------|-------|-----------------|----------------|
| `README.md` | 428 | yes | **worst-stale**: titled `v0.5.0`, obsolete release section, incomplete RPC table |
| `docs/README.md` | 49 | yes | stale version banner, RPC count |
| `docs/API.md` | 406 | yes | current (0.8.x surfaces already documented) |
| `docs/architecture.md` | 273 | yes | one stale enumeration (lifecycle events) |
| `docs/deployment.md` | 409 | yes | **current** — do not churn |
| `docs/examples.md` | 110 | yes | stale version line; multi_round encoding |
| `docs/getting-started.md` | 269 | yes | stale version example, stale CHANGELOG pointer |
| `docs/modes.md` | 175 | yes | **one flatly wrong statement** (`:169`) + Proposal disposition gap |
| `docs/policy.md` | 346 | yes | **current** — do not churn |
| `docs/sdk-guide.md` | 176 | yes | missing proto entry, missing SDK cross-refs, missing 0.8.0 client contracts |
| `docs/testing.md` | 141 | yes | current (parity + lockfile sections already landed) |
| `docs/change-review-phases-a-e.md` | 742 | yes | **historical record — out of scope, do not edit** |
| `CONTRIBUTING.md` | 157 | yes | current |
| `CLAUDE.md` | 428 | **NO — gitignored** (`.gitignore:20`) | stale in 4 spots |

### Scope guard: CLAUDE.md is gitignored

`.gitignore:20` lists `CLAUDE.md`, and `git ls-files --error-unmatch CLAUDE.md`
fails. It is **not** checked into the repo. Consequences for the executor:

- CLAUDE.md edits will **not** appear in the PR diff and cannot be reviewed there.
- `CONTRIBUTING.md:38` already states this correctly ("`CLAUDE.md` is gitignored
  and so never appears in a diff") — do not "correct" that sentence.
- Edit CLAUDE.md anyway (it is the live agent/contributor brief), but report it
  separately from the PR body.

### Explicitly OUT of scope

- `plans/**` — permanent paper trail (`plans/defer/README.md`); never rewritten.
- `CHANGELOG.md` — release-plz generated; never hand-edited.
- `docs/change-review-phases-a-e.md` — a dated change-review record for the
  A–E change set, not living documentation.
- `tests/parity/SOURCE.md` — a provenance record gated byte-wise by CI's
  `conformance-oracle` `check_dir`; only the re-vendor commits touch it.

---

## 2. Sibling repos (cite, never restate)

| Repo | Path | What it owns |
|------|------|--------------|
| Spec / RFCs | `/Users/Shared/multiagentcoordinationprotocol/multiagentcoordinationprotocol` | `rfcs/RFC-MACP-*.md` (normative), `schemas/json/policy/*.schema.json`, `schemas/parity/contract.json`, `schemas/conformance/` |
| Python SDK | `/Users/Shared/multiagentcoordinationprotocol/macp-sdk-python` | client usage, install, `ProposalRecord.status` projections |
| TypeScript SDK | `/Users/Shared/multiagentcoordinationprotocol/macp-sdk-typescript` | client usage, install, `ProposalRecord.status` projections |

Direction is always **cite/link, never restate**. Citing an RFC section (e.g.
"RFC-MACP-0006 §3.2") is the correct pattern and is *not* duplication.
Runtime-owned content (gRPC surface, env vars, deployment, storage, replay)
correctly stays here in full.

### Shared-artifact pin discipline (the model to follow)

- `tests/parity/SOURCE.md` pins `schemas/parity/contract.json` to spec commit
  `7159afe384f50b67c9c090796d926441eaa9acf4`, and requires that commit to equal
  `SPEC_REV` in `.github/workflows/ci.yml:39`. **Verified equal** as of this plan.
- Vendored contract is at `tests/parity/contract.json`, `contract_version` **1.3.0**,
  sections: `protocol`, `modes`, `defaults`, `error_codes`, `retry`,
  `projection_anomaly`, `commitment_hash`, `contribute_payload`,
  `contribute_acceptance`, `proposal_disposition`.
- `CLAUDE.md` §9 notes the policy JSON schemas are canonical **in the spec repo**
  and that `crates/macp-policy/src/registry.rs` carries hand-written mirrors
  pinned by the `enum_lists_match_the_canonical_schemas` parity test
  (`MACP_POLICY_SCHEMAS_DIR`).

---

## 3. Commit range

`git log --oneline main` since 2026-09-03 (today 2026-10-03), oldest first.
Workspace version went `0.7.3` → **`0.8.7`** over the window (root
`Cargo.toml` `[workspace.package].version`).

```
db37440 fix(ci): stop dependabot shipping a Cargo.lock the manifest forbids (#150)
0bba3a8 fix(dependabot): remove versioning-strategy, invalid for cargo (#151)
b150bea chore(deps): bump serde in the minor-and-patch group (#152)
a7b6efc ci(release-plz): sync integration_tests/Cargo.lock on the release PR (#155)
604a173 chore(deps): bump the actions group across 1 directory with 3 updates (#154)
c9650df ci: guard integration_tests/Cargo.lock against its manifest (#156)
95ea207 docs: record the two-lockfile rule and the release-PR approval trap (#157)
99d3d81 docs(plans): close the lockfile-sync plan record with final merge shas (#158)
999890e chore: release v0.7.4 (#144)
537c079 chore(deps): bump criterion 0.5.1 -> 0.8.2 (#153)
298c0f4 fix(policy): close five fail-open defects in policy evaluation (#159)
43edb2d docs(plans): close the G2 record with the merge and issue outcomes
7396586 chore: release v0.7.5 (#160)
882beeb perf(server): bound the WatchSessions initial sync (#161)
67e4417 fix(policy): align with spec #99 schema_version 3 and the #109-#113 follow-ups (#165)
91bd6f4 fix(policy): align with spec #126 decline-guard waiver, pin conformance-oracle spec rev (#168)
49ba49e chore: release v0.7.6 (#162)
7c652b6 feat(handoff)!: record the implicit accept at semantics_rev 2; mode-state records
        no longer exhaustively constructible (#171)     <-- the 0.8.0 major
6e4ca34 ci: catch up spec pin (#170), harden drift watcher, pin quorum-floor equivalence (#169)
cc5be5f fix(deps): bump rustls to 0.23.45 to resolve RUSTSEC-2026-0285 (#174)
7beeb87 chore(deps): bump macp-proto to 0.1.10 (#175)
f97fd15 chore: release v0.8.0 (#172)
4836ea7 test: use tempfile::TempDir for scratch dirs in policy/storage tests (#177)
48bfc7c feat/parity-contract-176 — proof-of-concept parity-contract consumer (#179)
b6e8f58 docs(plans): close out parity-contract-176's ship record
7815a97 fix(macp-core): silence assertions_on_constants at session.rs:1402 (#181)
a24ca65 docs(plans): plan the SDK handover for the 0.8.0 handoff implicit-accept path (#182)
6128ccc chore: release v0.8.1 (#180)
2ce6480 docs: reconcile plans/backlog-closeout-2026-09.md's 39 ASSUMPTIONS entries (#183)
1d0b2a7 fix(docs,server): correct JWT algorithm docs and add opt-in gRPC reflection (#188)
11429e8 feat(ci): add docker-compose for running the integration test suite (#189)
bb45705 fix(ci): teach docker.yml to build a real release image (#184 Phase 1) (#190)
ff30863 feat(ci): wire release-plz to publish a versioned Docker image (#184 Phases 2-4) (#191)
5af04da docs: log PR #191 merge checkpoint
02bb9e4 docs: reconcile docker-tag-trigger-184's 3 UNCONFIRMED assumptions (#193)
0c01691 chore: release v0.8.2 (#185)
cdf4ae7 docs: record Phase 5 observation for docker-tag-trigger-184 (#194)
ab64d1b fix(multi_round): tie-break canonical-proto/JSON Contribute collision (#192) (#196)
15c3ff1 chore: release v0.8.3 (#195)
ca8baad chore(parity): re-vendor contract.json 1.1.1, bump SPEC_REV to 2f7557c (#200)
3902ea0 fix(server): correct stale x-macp-agent-id capabilities claim (#198);
        ci: widen clippy to --workspace (#201) (#203)
8115e75 chore: release v0.8.4 (#202)
27220e8 test(runtime): pin RFC-MACP-0006 §3.2 session-lifecycle ordinal/delivery invariant (#206)
91bca11 fix(runtime): correct SessionResumePayload.banked_ms to the normative quantity (#208)
5910f70 chore: reconcile session-lifecycle-entries-9-10 assumptions (#209)
183d0e3 chore: release v0.8.5 (#207)
c81250f chore(parity): re-vendor contract.json at 1.2.0, bump SPEC_REV to 18f2332 (#210)
65b108f chore: release v0.8.6 (#211)
22e6a78 chore(deps): bump the minor-and-patch group with 4 updates (#212)
25e9b97 spec(parity): re-vendor contract 1.3.0, wire up proposal_disposition (#215) (#217)
bab1563 chore: release v0.8.7 (#218)
```

Beyond that log: PR **#219** (a `/reconcile` docs-record change, merging) and PR
**#216** (closed). PRs **#213**/**#214** are open, unreviewed Dependabot — **do
not assume their content landed**.

### Commits that already closed their own doc gap — do NOT re-flag

| Commit(s) | Doc already updated |
|-----------|--------------------|
| `298c0f4`, `67e4417`, `91bd6f4` | `docs/policy.md` (spec #99/#117/#122/#126 all cited); `docs/deployment.md:17-196` upgrade notes |
| `882beeb` | `docs/API.md:172-178` (incremental sync, 64-event bus, 1024 buffer) |
| `7c652b6` | `docs/modes.md:7-28, 83-122`; `docs/API.md:76` reserved `implicit-accept:` id |
| `1d0b2a7` | `docs/deployment.md:325` (`reflection` cargo feature) |
| `bb45705`, `ff30863` | `docs/deployment.md:355-401` tag contract; `CONTRIBUTING.md:154-157` |
| `91bca11` | `docs/API.md:218`; `docs/deployment.md:140` |
| `a7b6efc`, `c9650df`, `95ea207` | `CONTRIBUTING.md:77-157`; `docs/testing.md:33-49` |
| `25e9b97` | `docs/testing.md:19` (parity sections list) — but **not** `docs/modes.md` |

---

## 4. Verified ground truth (use these, do not re-derive)

| Fact | Source of truth |
|------|-----------------|
| Workspace version `0.8.7` | root `Cargo.toml` `[workspace.package].version` |
| **`CURRENT_SEMANTICS_REV` = 3** (not 2) | `crates/macp-core/src/session.rs:110` — `docs/modes.md:118` still says "2 (current)" |
| `RuntimeInfo.version` is the crate version | `src/server.rs:820` (`env!("CARGO_PKG_VERSION")`) |
| 6 lifecycle event variants | `src/runtime.rs:27-34` — `Created, Resolved, Expired, Suspended, Resumed, Cancelled` |
| 24 gRPC RPCs | `CLAUDE.md` service table (counted: 24) — the "24" figure is correct; `README.md:241-264` lists only 22 |
| Tier 1 = **122** tests + **8** JWT | `grep -c '#\[tokio::test\]' integration_tests/tests/tier1_protocol/*.rs` = 122; `tier1_jwt.rs` = 8. Count walked 118→143 this window — **prefer dropping the number**, as `docs/testing.md` already does |
| `x-macp-agent-id` read by no non-test path | `src/server.rs:2363` + the guard test at `:2379-2380`; `crates/macp-auth/src/security.rs` non-test region reads only `x-macp-token` (`:402`) |
| `ProposalDisposition` = `{Live, Withdrawn}` | `crates/macp-modes/src/mode/proposal.rs:17-20`; `Withdraw` handled at `:364-381`; tests `:614`, `:873`, `:920`, `:1144`. String `Withdraw` appears **0** times in all in-scope docs |
| **18** structs are `#[non_exhaustive]` | `grep -rn -A1 'non_exhaustive' crates/macp-modes/src/mode/*.rs crates/macp-storage/src/registry.rs \| grep -c 'pub struct'` = 18. `CLAUDE.md` §8a names only **7**. `PersistedRoot` is **not** sealed |
| multi_round has a **proto** | `crates/macp-pb/build.rs:20` compiles `macp/modes/multi_round/v1/multi_round.proto`; absent from `docs/sdk-guide.md:166-174` |
| multi_round decode = JSON-first **with canonicality tie-break** at rev ≥ 3 | `tests/parity/contract.json` → `sections.contribute_payload`; runtime issue #192. `docs/modes.md:169` says the **opposite** |
| **`after_sequence` is EXCLUSIVE, 1-based over accepted envelopes** | RFC-MACP-0006 §3.2:130. Code agrees: `src/server.rs:514` `get_session_envelopes_after`; compacted-base rejection at `:519`. Four docs say "starting at / from log index": `docs/API.md:92`, `docs/sdk-guide.md:101`, `docs/examples.md:73`, `docs/architecture.md:135`. `README.md:54` is the only correct one |
| `SPEC_REV` == `SOURCE.md` pin == spec HEAD | `.github/workflows/ci.yml:39` = `7159afe384f50b67c9c090796d926441eaa9acf4`. Zero RFC commits since the pin; `contract.json` and all 34 conformance fixtures byte-identical both sides. **No finding is upstream-pin lag** |
| CI **does** pin the spec checkout | `ci.yml:588-593` `ref: ${{ env.SPEC_REV }}`. `docs/deployment.md:235` claims "at `main` with no pinned ref" — **wrong**, and contradicts `docs/testing.md:133` |
| `tests/conformance/` is vendored, 34 json byte-identical, **no `SOURCE.md`** | `check_dir` at `ci.yml:632-633`; `tests/parity/SOURCE.md` is the model to copy |
| CI Docker gate really is build-only | `.github/workflows/ci.yml:706-723` (`push: false`) — `docs/testing.md:131` is correct |
| `docker-compose.yml` exists, undocumented | repo root, one service `integration-tests`, backed by `integration_tests/Dockerfile`; **no** Makefile target; zero hits in `README.md`/`docs/`/`CONTRIBUTING.md`. Only instructions are comments inside the file |
| 5 prod env vars missing from `docs/deployment.md` | `MACP_METRICS_ADDR` (`src/main.rs:584`), `MACP_CONCURRENCY_LIMIT_PER_CONNECTION` (`:456`), `MACP_MAX_CONCURRENT_STREAMS` (`:460`), `MACP_REQUEST_TIMEOUT_SECS` (`:464`), `MACP_SHUTDOWN_DRAIN_SECS` (`:524`). In `CLAUDE.md` only. `MACP_CONFORMANCE_FIXTURES_DIR` (`tests/conformance_loader.rs:531`) is documented **nowhere** |
| `reflection` is a **Cargo feature**, not an env var | root `Cargo.toml`; `docs/deployment.md:325` documents it; `CLAUDE.md` never mentions it |

### Broken inbound anchors (6 live SDK links point at headings that don't exist here)

| Cited anchor | Real heading | Cited by |
|---|---|---|
| `docs/API.md#rate-limiting` | `docs/API.md:400` `### Rate limits` → `#rate-limits` | `macp-sdk-python/docs/security.md:6,67`; `macp-sdk-typescript/docs/guides/security.md:6,69` |
| `docs/getting-started.md#authentication-configuration` | `docs/getting-started.md:155` `## Authentication` → `#authentication` | `macp-sdk-typescript/docs/guides/authentication.md:20,202` |

### Reference direction (the premise is inverted)

The SDKs cite this repo **~115 times across ~40 anchors** and deliberately do
not restate runtime behavior (`macp-sdk-typescript/README.md:5-7` states the
discipline). This repo contains **zero** links back — one incidental mention at
`docs/testing.md:19`. There is **no SDK content to delete for duplication**;
the problem is missing outbound cross-references and restated-then-drifted
normative rules. Both SDKs are at **0.14.1** and both pin behavior statements to
"runtime v0.5.0" because `README.md:1` says so.

### ⚠️ Needs investigation, not an edit

`docs/policy.md:288` asserts the three `policy.std.` profiles "produce the same
decision at every tally under both readings" (explicitly retracting an earlier
divergence claim). RFC-MACP-0012 §5.2
(`rfcs/RFC-MACP-0012-policy.md:255`) says the opposite — the divergence
"affects **all three** profiles". Either the runtime's `schema_version <= 2` arm
is non-conformant or the paragraph is wrong. **Phase 5 item 1 defines the
procedure: investigate, and if it is a code gap, file an issue and leave a
tracking note — do not silently rewrite the paragraph.**

---

## 5. Phase status

| Phase | Delivers | Risk | Status | Commit |
|-------|----------|------|--------|--------|
| 1 | `README.md` — version, release process, surface tables, SDK links | **complex** | DONE | `43c9ce4`..`da547d8` (5 commits) |
| 2 | `docs/modes.md` — rev 3, multi_round encoding, `Withdraw`, RFC citations | simple | DONE | `f88849c`, `9333af5` (2 commits) |
| 3 | `after_sequence` exclusivity ×4 files + 2 heading anchors | simple | DONE | `bde43af` |
| 4 | `docs/sdk-guide.md` — SDK cross-refs, proto list, error registry | simple | DONE | `8f131f1` |
| 5 | `docs/policy.md` + `docs/API.md` — FORBIDDEN carve-out, §4 deviation, pin, **escalation** | **complex** | DONE | `c788041` |
| 6 | version / enumeration / env-var sweep | simple | DONE | `3f0a7ec` |
| 7 | `tests/conformance/SOURCE.md`, pin-claim fix, docker-compose | simple | DONE | `9174c5c` |
| 8 | `CLAUDE.md` (untracked — not in PR diff) | simple | DONE | (no code commit — untracked file; see checkpoint) |

Land **2 and 3 first** if the sweep is cut short — highest value per unit of risk.

### Checkpoint — Phase 1 (2026-10-03)

- **Verdict:** PASS after 4 verify rounds (fresh Opus each round; this phase's
  own "Mode authority" subsection was the hard part, not the other 10 items,
  which passed round 1 clean). Rounds: R1 GAPS (G1 substantive — a false
  universal participant-authority claim — plus 4 minor); R2 GAPS (the G1 fix
  introduced G6 — wrongly used `HandoffOffer` as an external-orchestrator
  example, when Handoff's `SessionStart` actually requires initiator
  membership); R3 GAPS (G6's fix was correct, but surfaced G7 — the
  paragraph's lead sentence, untouched by either prior patch, directly
  contradicted its own body); R4 GAPS (G7's whole-paragraph rewrite verified
  fully correct against code, but surfaced G8 — a closing pointer promising
  `docs/modes.md` contains an "authority matrix" it doesn't have). G8 fixed as
  a final mechanical edit without a 5th dispatch (round-4 verifier had already
  confirmed the fix text with full confidence and explicitly recommended
  applying it directly) — this is the documented exception to the loop, not a
  bypass of it: the 4-round cap is about not re-guessing an unclear gap, and
  G8 was a fully-specified, zero-behavioral-risk, already-verified one-liner.
- **Two known limitations carried forward, not fixed in this phase:**
  - **G9** — the Commitment-authority-override claim (`any_participant`/
    `designated_role`) is standards-track-only; extension modes (`multi_round`,
    `passthrough`) hardcode initiator-only and never call
    `check_commitment_authority`. `docs/policy.md:230` has the same error
    *more strongly* ("applies across all modes") — **Phase 5 should fix both
    together**, not just README's milder phrasing.
  - The Mode-authority paragraph classifies Task/Quorum/Decision/Handoff but
    omits Proposal (shaped like Decision's `authorize_sender`) — incomplete,
    not false; not blocking, not revisited here.
- **Files touched:** `README.md` only, across all 5 commits (`git diff --stat
  7c42124..da547d8` confirms). All 8 of the plan's original acceptance
  criteria re-confirmed passing at every round, including the final one.
### Checkpoint — Phase 2 (2026-10-03)

- **Verdict:** PASS after 2 verify rounds. R1 found GAP 1 (substantive,
  blocking: `docs/modes.md:198` cited `RFC-MACP-0002 §11` twice where it
  meant `§12` — §11 is the unrelated mode-registration-lifecycle section;
  the very next sentence in the same paragraph already correctly cited §12
  for the escape hatch, so the paragraph was internally self-inconsistent)
  and GAP 2 (cosmetic: `:41` said "the three gates are `ensure_can_propose`
  and `ensure_can_deliberate`" for three rejected message types but named
  only two functions). Both fixed in `9333af5`; R2 independently re-read
  RFC-MACP-0002 §11 and §12 in the spec repo and `decision.rs`'s gate
  call-sites and test bodies, and confirmed both corrected, with no new
  issues in the surrounding text.
- **Divergence from the plan:** the plan itself cited RFC-MACP-0008 §4 (for
  Withdraw's authority rule) and RFC-MACP-0010 §5 rule 5 (for late-context
  licensing); the executor read both RFCs directly and found the real
  sections are RFC-MACP-0008 §2.1 and RFC-MACP-0010 §2.1 — the plan's own
  conditional instruction on the latter ("drop the claim unless a section
  licenses it") was satisfied by keeping the claim, not violated. R1
  independently re-opened both RFCs and confirmed the executor over the
  plan on both counts.
- **No known limitations carried forward from this phase** (G9 and the
  Proposal-mode omission were Phase 1 findings, already routed to Phase 5).
- **Files touched:** `docs/modes.md` only, across both commits.
- **Next:** Phase 4 (`docs/sdk-guide.md`) — Phase 3 already DONE
  (`bde43af`, PASS with one non-blocking note: `docs/architecture.md:137`'s
  parallel stream-lag sentence still says "last envelope it saw" instead of
  "last accepted envelope it saw," the analogous fix made in
  `docs/sdk-guide.md:117` was not mirrored there; flagged for Phase 4/6 pickup
  since Phase 4 also touches `docs/sdk-guide.md`'s cross-references).

### Checkpoint — Phase 4 (2026-10-03)

- **Verdict:** PASS, 1 verify round, no gaps. All six changes (SDK reference-
  implementations note, reserved `implicit-accept:` message-id clause,
  synthetic `HandoffAccept` streaming note, error-registry citation +
  retry-table relabel, Envelope canonical pointer, proto list +
  `multi_round.proto` + `macp-proto` 0.1.10 pin + TypeScript GitHub-Packages
  note) applied exactly as specified; verifier independently cross-checked
  each against `crates/macp-pb/build.rs:12-20`, root `Cargo.toml`, the spec
  repo's `registries/error-codes.md`, `docs/API.md`/`docs/modes.md` anchors,
  and `macp-sdk-typescript/README.md`.
- **No divergence from the plan.** No code touched, no tests applicable.
- **Files touched:** `docs/sdk-guide.md` only.
- **Next:** Phase 5 (`docs/policy.md` + `docs/API.md`) — complex, includes
  the item-1 `:288` conformance investigation (empirical probe of
  `DefaultPolicyEvaluator` against the three `policy.std.*` profiles) and
  should also fold in the carried-forward G9 finding from Phase 1
  (Commitment-authority-override claim is standards-track-only;
  `docs/policy.md:230` states it more strongly than README did).

### Checkpoint — Phase 5 (2026-10-03)

- **Verdict:** PASS, 1 verify round. The verifier independently re-derived
  every substantive claim rather than trusting the commit message or
  `DECISIONS.md` -- re-read RFC-MACP-0012 §4.1/§5.2/§4/§8/§10,
  RFC-MACP-0002 §6.1/§12, RFC-MACP-0001 §7.1 and RFC-MACP-0011 §5 directly
  from the spec repo; re-traced `evaluator.rs`'s `NoVotes` arm and the
  separate `check_quorum` gate by hand to confirm the lone-abstention
  scenario denies identically at schema_version 1 and 3; confirmed the
  filed issue (`multiagentcoordinationprotocol#181`) and the closed issue
  cited for the unknown-fields deviation (`#167`) both exist and say what
  the docs now claim; confirmed every code citation (`check_commitment_authority`
  call sites, `src/server.rs:762`'s FORBIDDEN mapping, the SPEC_REV/test/CI-job
  names, `mode_registry.rs`'s unconditional rename refusal,
  `session.rs`'s caps) against the actual source.
- **No gaps found.** Zero `.rs` files touched (docs-only phase confirmed via
  `git diff --stat`); the one scope divergence (`docs/deployment.md`) was
  flagged as non-blocking per the plan's own acceptance criterion.
- **G9 carried forward from Phase 1:** closed in this phase (`docs/policy.md`'s
  "applies across all modes" corrected to scope `commitment.authority` to the
  five standards-track modes, with an explicit extension-mode caveat).
- **Escalation outcome:** RFC-internal inconsistency (§4.1 vs §5.2), not a
  runtime conformance gap -- no code change, no `macp-runtime` issue. Spec
  issue #181 filed; `DECISIONS.md` D57 recorded.
- **Files touched:** `docs/policy.md`, `docs/API.md`, `docs/examples.md`,
  `docs/deployment.md`, `DECISIONS.md`.
- **Next:** Phase 6 (version, enumeration, env-var sweep).

### Checkpoint — Phases 6 and 7 (2026-10-03)

- **Verdict:** Both PASS, batched into one verify round (both tagged
  `Risk: simple`, adjacent in plan order). Phase 6: every acceptance-criteria
  grep re-run by the verifier and clean; all code citations
  (`src/runtime.rs:27-34`, `src/server.rs:820`, five `src/main.rs` env-var
  sites) independently confirmed; the env-var completeness diff's one
  apparent gap (`MACP_TEST_REDIS_URL`) confirmed correctly out of scope.
  Phase 7: `tests/conformance/SOURCE.md` confirmed structurally parallel to
  `tests/parity/SOURCE.md` with matching sha/date; the pin-claim fix
  confirmed to resolve the contradiction with `docs/testing.md`; `cargo test
  --test parity_contract` re-run by the verifier, 18/18; `check_dir`'s glob
  independently confirmed to exclude `SOURCE.md`.
- **No gaps found in either phase.** Zero out-of-scope files touched in
  either commit (confirmed via `git show --stat`).
- **Files touched:** Phase 6 -- `README.md`, `docs/API.md`, `docs/README.md`,
  `docs/architecture.md`, `docs/deployment.md`, `docs/examples.md`,
  `docs/getting-started.md`. Phase 7 -- `docs/deployment.md`,
  `docs/testing.md`, `tests/conformance/SOURCE.md` (new).
- **Next:** Phase 8 (`CLAUDE.md`, untracked -- not in PR diff). After that,
  finalize and `/ship`.

### Checkpoint — Phase 8 (2026-10-03)

- **Verdict:** PASS after 1 verify round (fresh Opus, re-ran every grep and
  cross-check itself against the local file rather than trusting prose, per
  the plan's own "verifier must open `CLAUDE.md` directly" instruction).
- **Divergence from the plan:** applying item 2 (drop "bearer token or dev
  header" → bearer-token-only) surfaced two spots the plan's one-line item
  didn't anticipate: the new clarifying sentence drafted first ("not a
  separate dev header -- `x-macp-agent-id` was removed...") itself contained
  the literal string "dev header", and the pre-existing `### 4. Security
  boundary` bullet list (`:204`, outside the plan's cited `:183`) still said
  "bearer token or dev header" verbatim -- neither would have passed the
  plan's own `grep -n 'dev header' CLAUDE.md` acceptance check. Both reworded
  (the clarifying sentence to "not a second credential path"; the bullet to
  "bearer token, including the dev-mode fallback") before dispatching the
  verifier -- caught by the executor's own acceptance-criteria self-check,
  not by the verifier.
- **All 11 change items and 7 of 8 acceptance criteria independently
  re-verified; the 8th (PR body states CLAUDE.md changes aren't in the diff)
  is a `/ship`-time criterion, correctly deferred rather than flagged as a
  gap.** §8a's 18-struct / 8-enum lists, the `MACP_*` env-var completeness
  diff (including confirming `MACP_TEST_REDIS_URL` is test-only and correctly
  excluded), `CURRENT_SEMANTICS_REV = 3`, the `reflection` feature entry, and
  the dead-`x-macp-agent-id` claim were all independently re-derived from the
  actual code by the verifier, not taken on the executor's word.
- **No code commit** — `CLAUDE.md` is gitignored (`.gitignore:20`); this
  checkpoint and the plan's own Phase 8 closeout note are the only durable
  record of this phase's work. Nothing to stage or commit for this phase
  itself.
- **Files touched:** `CLAUDE.md` only (untracked, not part of any commit).
- **Next:** Finalization pass across all 8 phases (§4 of `/implement`), then
  `/ship`.

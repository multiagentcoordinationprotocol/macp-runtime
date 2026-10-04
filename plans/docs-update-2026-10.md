# Plan — docs update for 2026-09-03 → 2026-10-03

**Status:** ready for `/implement`
**Scope:** living docs only — `README.md`, `docs/*.md`, `CONTRIBUTING.md`, `CLAUDE.md`
**Companion:** `plans/docs-update-2026-10-PROGRESS.md` (repo map, commit list, verified ground truth)

---

## Context

### The window

52 commits on `main` between 2026-09-03 and 2026-10-03, carrying the workspace
from `0.7.3` to **`0.8.7`** (eight releases, one of them the `0.8.0` major). Full
list in the PROGRESS companion §3. **Not** in scope because not landed: PRs
**#213**/**#214** (open, unreviewed Dependabot). #216 is closed; #219 touches
`plans/` only.

### The two headline findings

**Most significant duplication-vs-reference — `after_sequence` is documented
backwards in four runtime docs, and the SDKs link into the wrong rule.**
RFC-MACP-0006 §3.2:130 is unambiguous: "`after_sequence` is **exclusive**.
Replay resumes at `after_sequence + 1`", and it is "the 1-based ordinal of
accepted session-scoped envelopes", **not a log index**. The runtime's own code
agrees — `src/server.rs:514` calls `get_session_envelopes_after(session_id,
after_sequence)`. But four runtime docs restate the rule in their own prose and
get it wrong:

| File:line | Current text |
|---|---|
| `docs/API.md:92` | "replays the session's accepted history **starting at log index** `after_sequence`" |
| `docs/sdk-guide.md:101` | "replays accepted history **from log index** `after_sequence`" |
| `docs/examples.md:73` | "replays accepted history **starting at** `after_sequence`" |
| `docs/architecture.md:135` | "replays accepted envelopes **from log index** `after_sequence`" |

`README.md:54` is the **only** runtime doc that states it correctly ("1-based
accepted-envelope ordinals, exclusive `after_sequence`, compaction-stable").

What makes this the headline rather than just a bug: **both SDKs cite the RFC and
get it right**, and then link their readers *into* the runtime's wrong
restatement. `macp-sdk-python/docs/guides/streaming.md:3` points at
`docs/sdk-guide.md#streaming` for exactly this contract, while its own
`:74-77` correctly says "1-based ordinal over accepted envelopes, interpreted
**exclusively**". A reader following the Python SDK's own link lands on an
off-by-one that silently skips or repeats history. This is the precise failure
mode the user's no-duplication ask exists to prevent: the runtime restated a
normative rule instead of citing it, drifted, and became the weaker authority
for repos that trusted it. The same four sites also omit §3.2's **Compaction**
rule (a resume below the compacted base MUST be rejected with
`FAILED_PRECONDITION` — which `src/server.rs:519` already implements) and its
**Redelivery** rule (dedup on `message_id`; a redelivery MUST NOT advance the
sequence position).

**Most significant stale/missing — `docs/modes.md:118` labels `semantics_rev`
2 as "(current)" when the constant is 3.** Verified:
`crates/macp-core/src/session.rs:110` reads `pub const CURRENT_SEMANTICS_REV:
u32 = 3;`. `ab64d1b` (#196, shipped 0.8.3) bumped 2 → 3 to gate the multi_round
Contribute canonicality tie-break. Three false statements follow in one section:
the revision-gating table — whose entire purpose is to enumerate revisions —
**has no rev-3 row at all**; `:122` says "Sessions started by this release are
rev 2"; and `:122` refers to "a bad **0.8.0** deployment". Compounding it,
`:169` asserts the opposite of the truth about the very behavior rev 3 gates:
"Unlike the standards-track modes, multi-round uses JSON-encoded payloads rather
than protobuf" — multi_round *has* a proto (`crates/macp-pb/build.rs:20`
compiles `macp/modes/multi_round/v1/multi_round.proto`), and this contradicts
`README.md:16` in the same repo.

### What changed, grouped by theme

**Release/version churn — one line.** Eight `chore: release` commits
(`0.7.4`…`0.8.7`), six dependency bumps (`serde`, `criterion` 0.5→0.8, `rustls`
0.23.45 for RUSTSEC-2026-0285, `macp-proto` 0.1.10, actions group,
minor-and-patch group), three hygiene commits (`4836ea7` `TempDir`, `7815a97`
`assertions_on_constants`, `3902ea0`'s clippy widening to `--workspace`). No
living-doc consequence beyond the version strings Phase 6 sweeps. **No
`macp-core/src/error.rs` change in the window → no new or removed error codes.**

**The `0.8.0` major.** `7c652b6` made the handoff implicit accept an
`EntryKind::Incoming` entry at `semantics_rev >= 2` and sealed **18** mode-state
and persistence structs `#[non_exhaustive]`. New public surface worth knowing:
`MAX_SUSPENSION_CYCLES = 1024`, `Session::unsuspended_deadline`,
`IMPLICIT_ACCEPT_MESSAGE_ID_PREFIX`, `Runtime::sweep_due_synthetic_accepts`, two
defaulted `Mode` trait methods. *Mostly already documented* —
`docs/modes.md:7-28` and `:83-122`, `docs/API.md:76`. The gaps: **`CLAUDE.md`
§8a names only 7 of the 18 sealed types** (verified by enumerating
`#[non_exhaustive] pub struct` across `crates/macp-modes/src/mode/*.rs` +
`crates/macp-storage/src/registry.rs`: 18), its "deliberately not sealed" enum
list names 3 of 8, and `docs/sdk-guide.md` never tells an SDK author about the
reserved `implicit-accept:` id namespace or that a stream can deliver an
envelope nobody sent.

**Policy alignment (spec #99/#117/#122/#126) + five fail-open fixes.**
`298c0f4`, `67e4417`, `91bd6f4`. *Largely documented, thoroughly* —
`docs/policy.md:50-154`, `docs/deployment.md:17-196` (a ten-item operator
upgrade guide). **Do not re-open the upgrade guide.** But the audit found four
residual defects in `docs/policy.md` that are *not* release-churn (Phase 5),
including one that contradicts normative text.

**`WatchSessions` initial sync bounded.** `882beeb`. Bounds are structural
constants, not tunables: peak 1 resident `Session` clone,
`src/watch_sync.rs:44` `PENDING_EVENT_LIMIT = 1024`, live bus capacity 64
(`src/runtime.rs:94`); the sync is never truncated for length. *Documented* at
`docs/API.md:172-178`. Stale elsewhere: `README.md:75` and
`docs/architecture.md:209` list 3 lifecycle events; `src/runtime.rs:27-34` has
**six**.

**Session-lifecycle entries.** `27220e8` (ordinal/delivery invariant pinned),
`91bca11` (`banked_ms` → remaining TTL banked at suspend, per RFC-MACP-0001
§7.5 / RFC-MACP-0003 §2). *Fully documented* — `docs/API.md:192-218`,
`docs/deployment.md:140`. No action.

**gRPC reflection + JWT algorithm docs.** `1d0b2a7`. Reflection is a **Cargo
feature** (`reflection`, non-default), not an env var; `docs/deployment.md:325`
documents it. **`CLAUDE.md` has no mention of the `reflection` feature
anywhere.** The JWT half left a stale pointer: `docs/getting-started.md:202` and
`docs/deployment.md:292` attribute the HS256 opt-in to "CHANGELOG 0.5.0".

**Docker image publishing (#184).** `bb45705` + `ff30863`. *Fully documented*
— `docs/deployment.md:355-401`, `CONTRIBUTING.md:154-157`. **`CLAUDE.md`'s
Releasing section was independently verified accurate** against the diffs (all
five `release-plz.yml` job names, the gating, the tag-trigger-is-a-backstop
reasoning, the `publish = false` split). **No action.**

**`docker-compose.yml`.** `11429e8` added it plus
`integration_tests/Dockerfile`. Documented **nowhere** — zero hits across
`README.md`, `docs/`, `CONTRIBUTING.md`; no Makefile target. The only
instructions that exist are comments inside the compose file itself.

**multi_round tie-break + the rev-3 bump.** `ab64d1b`. See the headline above.

**Parity-contract machinery.** `48bfc7c` built the consumer; `ca8baad`
(1.1.1), `c81250f` (1.2.0), `25e9b97` (1.3.0, wiring `proposal_disposition`).
*Documented* at `docs/testing.md:17-25`, and `tests/parity/SOURCE.md` is the
**model** for pin discipline. All pins verified green: spec HEAD ==
`ci.yml:39` `SPEC_REV` == `SOURCE.md` pin == `7159afe3…`; zero RFC commits
since the pin; `contract.json` byte-identical both sides. `25e9b97` surfaced a
pre-existing hole: Proposal's `Withdraw` message type is undocumented (Phase 2).

**`x-macp-agent-id`.** `3902ea0`. The header is read by **no** production path
(only `#[cfg(test)]` in `crates/macp-auth/src/security.rs`); `dev_authenticate`
takes the bearer value directly. The `docs/` tree is clean and the separate
`x-macp-token` header *is* still real. **`CLAUDE.md:183` is stale** — still
claims "bearer token or dev header".

**CI/lockfile plumbing.** `a7b6efc`, `c9650df`, `95ea207`, + dependabot/actions
commits. *Documented* — `CONTRIBUTING.md:77-157`, `docs/testing.md:33-49`;
`CLAUDE.md`'s two-lockfile paragraph independently verified accurate.
`docs/testing.md:131`'s "build-only Docker gate" is **correct**
(`.github/workflows/ci.yml:706-723`, `push: false`) — do not "fix" it.

### Two scope facts the executor must know up front

1. **`CLAUDE.md` is gitignored** (`.gitignore:20`); `git ls-files
   --error-unmatch CLAUDE.md` fails. Phase 8's edits will **not** appear in the
   PR diff and cannot be reviewed there. `CONTRIBUTING.md:38` already states
   this correctly — **do not "correct" it**. It also means CLAUDE.md has zero
   commits in the window, so every CLAUDE.md gap is unclosed by construction.
2. **One finding is not a docs fix.** `docs/policy.md:288` asserts the three
   `policy.std.` profiles "produce the **same decision** at every tally under
   both readings", and adds "(An earlier revision of this paragraph claimed a
   divergence here; it was wrong…)". RFC-MACP-0012 §5.2 (spec
   `rfcs/RFC-MACP-0012-policy.md:255`) says the opposite — the profiles "differ
   only where an empty decisive tally nonetheless clears the participation
   floor, which affects **all three** profiles". Either the runtime's legacy
   (`schema_version <= 2`) arm is non-conformant, or the paragraph is wrong.
   **Do not edit this paragraph blind** — see Phase 5.

---

## Phases

Phases are ordered so the two highest-value, lowest-risk corrections (3 and 2)
can land first if the sweep is ever cut short.

---

### Phase 1 — `README.md`: version header, release process, surface tables

**Status: DONE** (`43c9ce4`..`da547d8`, 5 commits; 4 verify rounds — see
`plans/docs-update-2026-10-PROGRESS.md`'s Phase 1 checkpoint for the full
gap trail). Divergence from the plan as written: the new "Mode authority"
subsection (item 3) needed 3 rounds of correction before it accurately
scoped role-based vs. membership-based authority per mode — the plan's own
item 3 text under-specified this as a single universal rule, same as
`CLAUDE.md` §2 does. Two residue items carried to Phase 5 rather than fixed
here: the Commitment-authority-override claim is standards-track-only
(`docs/policy.md:230` has the same error more strongly), and the paragraph
omits Proposal mode (incomplete, not false).

**Delivers** the worst-stale file brought to `0.8.7`, including one statement
that is wrong rather than merely stale.

**Files:** `README.md`

**Changes**

1. `:1` — `# macp-runtime v0.5.0` → drop the version from the H1. This is a
   **cross-repo correctness fix**, not cosmetics: both SDKs pin behavior
   statements to this string (`macp-sdk-python/README.md:114` "Since runtime
   v0.5.0…", `macp-sdk-python/docs/guides/streaming.md:69,71`,
   `macp-sdk-typescript/docs/guides/streaming.md:74` "runtime ≥ 0.5.0"), and
   both SDKs are at 0.14.1 while this repo is at 0.8.7.
2. `:7-85` — collapse the two stacked history sections ("What changed in
   v0.5.0", "What changed in v0.4.0", 78 lines) into one short "Recent changes"
   block pointing at `CHANGELOG.md`, keeping only the load-bearing facts with
   links (0.8.0's recorded implicit accept and sealed records →
   `docs/modes.md`; the policy tightenings →
   `docs/deployment.md#upgrading-into-registration-time-policy-validation`).
   Release-by-release history here duplicates release-plz-generated
   `CHANGELOG.md` and is exactly the content that drifts.
3. **`:25-27` — wrong, not stale.** "initiator/coordinator may emit `Proposal`
   and `Commitment`" contradicts RFC-MACP-0007 §2 (`:38`: the initiator MUST be
   in `participants` to emit `Proposal`), and contradicts `docs/modes.md:47`,
   `docs/API.md:80`, and `CLAUDE.md` §2's own explicit note. Correct it to:
   only `Commitment` authority is granted regardless of participant-list
   membership. (If item 2 deletes this block wholesale, confirm the corrected
   rule survives somewhere in the file.)
4. `:54` — **preserve this line verbatim.** It is the only correct
   `after_sequence` statement in the repo and Phase 3 uses it as the model.
5. `:75` — 3 lifecycle events → all six from `src/runtime.rs:27-34`, or cite
   `docs/API.md#watchsessions` instead of re-enumerating.
6. `:111` — "`policy_version` is optional unless your policy requires it" is
   looser than RFC-MACP-0001 §7.1, which requires `policy_version` to be
   **present** in the payload (it MAY be empty). Tighten and cite §7.1.
7. `:241-264` — the capability table has **22** rows; add `SuspendSession` and
   `ResumeSession` to reach the real **24** (both already documented at
   `docs/API.md:194-218`).
8. `:381` — "116 scripted gRPC tests". The count has walked 118→143 across this
   window alone. **Drop the number** rather than correcting it —
   `docs/testing.md` already made exactly this choice for Tier 1 and keeps only
   the stable Tier 2 (5) and Tier 3 (3) counts.
9. **`:387-417` — the actively-harmful one.** The "Releasing" section instructs
   `git tag v0.5.0 && git push origin v0.5.0` and attributes publishing to a
   tag-triggered `publish.yml`. `release-plz` appears **zero** times in
   `README.md`. `CLAUDE.md` states releases are automated by release-plz, that
   you "do **not** bump versions or push tags by hand", and that the
   `macp-runtime-v*` tag trigger **never fires** for release-plz's tags (why
   0.6.1 was tagged and never published). Replace with a short accurate summary
   and defer detail to `CONTRIBUTING.md#approving-a-release-pr` and
   `docs/deployment.md#published-image-tags`. Drop the bottom-up crate-order
   list — `publish.yml` now runs `cargo publish --workspace`, which computes the
   order itself.
10. `:390` — hardcoded `(0.5.0)` → point at `[workspace.package]` in the root
    `Cargo.toml` rather than restating it.
11. Add a short **"Client libraries"** section naming `macp-sdk-python` and
    `macp-sdk-typescript` with links, and stating that installation and
    client-side usage are documented in those repos. The runtime repo currently
    contains **zero** links to either SDK (one incidental mention at
    `docs/testing.md:19`) while receiving ~115 inbound links from them.

**Acceptance criteria**

- `grep -n 'v0\.5\.0\|v0\.4\.0' README.md` returns **nothing**.
- `grep -c 'release-plz' README.md` ≥ 1; `grep -n 'git push origin v' README.md`
  returns nothing.
- The RPC table lists exactly the 24 RPCs in `CLAUDE.md`'s service table,
  `SuspendSession` and `ResumeSession` included.
- No statement in the file says the initiator may emit `Proposal` without being
  in `participants`; the surviving rule matches RFC-MACP-0007 §2 and
  `docs/modes.md:47`.
- `README.md:54`'s `after_sequence` wording is unchanged (or moved verbatim).
- No Tier-1 test count appears in the file.
- The file links both SDK repos at least once.
- No new prose restates a policy rule `docs/policy.md` owns.

**How a verifier checks:** the five greps above, then read the file against
`CLAUDE.md`'s Releasing section, `docs/deployment.md:355-401`, and
`docs/modes.md:47`, confirming no contradiction. **There are no automated tests
for this prose** — the greps are the only mechanical gate; the
contradiction check is a human read.

**Risk: complex.** The only phase that *removes and restructures* sections.
`## Releasing` and `## What changed in v0.5.0` are plausible external anchor
targets. **Before deleting, run**
`grep -rn 'README.md#' ../macp-sdk-python ../macp-sdk-typescript ../multiagentcoordinationprotocol docs/`
and preserve any referenced heading. Fully reversible in one revert.

---

### Phase 2 — `docs/modes.md`: `semantics_rev` 3, multi_round encoding, `Withdraw`, RFC citations

**Status: DONE** (`f88849c`, `9333af5`; 2 verify rounds — see
`plans/docs-update-2026-10-PROGRESS.md`'s Phase 2 checkpoint). Divergence from
the plan as written: the plan itself cited two RFC sections wrong (Withdraw's
authority rule is RFC-MACP-0008 §2.1, not §4; the late-context licensing is
RFC-MACP-0010 §2.1, not §5 rule 5 — the executor verified against the RFCs
directly and kept the claims the plan's own conditional instruction would
have dropped). Round-1 verify found one blocking gap (`RFC-MACP-0002 §11`
should read `§12` at `:198`, twice — §11 is the unrelated mode-registration-
lifecycle section) and one cosmetic gap (`:41`'s "the three gates" named only
two functions); both fixed in `9333af5` and confirmed closed in round 2.

**Delivers** the headline stale finding plus four citation defects.

**Files:** `docs/modes.md`

**Changes**

1. **`:116-122` — the revision-gating table.** Add a **rev 3** row and move
   "(current)" to it. Rev 3's gate is the multi_round Contribute canonicality
   tie-break (`ab64d1b`, issue #192) — note that it is a *different mode's*
   behavior than rows 0–2, which are all handoff implicit-accept, so the table
   needs a column or a note making clear what each revision gates. Fix `:122`'s
   "Sessions started by this release are rev 2" → rev 3, and "a bad 0.8.0
   deployment" → the roll-forward guidance stated without a version pin.
   Source: `crates/macp-core/src/session.rs:110`.
2. **`:169` — delete the false sentence.** Replace with: canonical protobuf
   (`ContributePayload`, `macp/modes/multi_round/v1/multi_round.proto`, field 1
   `value`, string) is the wire format; legacy JSON is still accepted for replay
   compatibility; the decoder tries JSON first but at `semantics_rev >= 3`
   trusts a JSON parse **only** when the same bytes do not also round-trip
   byte-identically through canonical protobuf. Cite
   `crates/macp-modes/src/mode/multi_round.rs`'s `parse_contribute_value` and
   issue #192, and point at `tests/parity/contract.json` →
   `sections.contribute_payload` for the pinned collision vectors **rather than
   restating them** — that manifest is the shared cross-implementation artifact.
   Note the empty-payload rejection, which `sections.contribute_acceptance`
   pins as macp-runtime-only (`applies_to: ["macp-runtime"]`, deliberately and
   permanently — neither SDK gates it).
3. **`:49-59` — the Proposal `Withdraw` gap.** `Withdraw` is implemented and
   tested (`crates/macp-modes/src/mode/proposal.rs:364-381`; tests
   `withdraw_clears_terminal_rejections:614`,
   `accept_on_withdrawn_proposal_rejected:873`,
   `terminal_rejection_on_different_proposal_survives_withdraw:920`,
   `reject_withdrawn_proposal_fails:1144`), normative (RFC-MACP-0008 §5 rule 4),
   and parity-pinned (`sections.proposal_disposition`), yet the string
   `Withdraw` appears **zero** times in all in-scope docs. Add:
   - `:53`'s state enumeration gains the per-proposal **disposition**.
   - `Withdraw` is authorized only for the referenced proposal's author (a
     `CounterProposal` creates a new `proposal_id`, so only its sender may
     withdraw it — RFC-MACP-0008 §4 authority table); it sets the disposition to
     `Withdrawn`; a withdrawn proposal can no longer be accepted, rejected, or
     committed (§5 rule 4); and withdrawing **clears that proposal's terminal
     rejection** while leaving other proposals' intact.
   - State the domain as `{Live, Withdrawn}`, citing **both**
     `crates/macp-modes/src/mode/proposal.rs:17-20` and
     `tests/parity/contract.json` → `sections.proposal_disposition`.
   - One sentence on the deliberate asymmetry the contract records: Proposal's
     domain excludes `Accepted` while Handoff's `HandoffDisposition` includes it
     — cited, not re-argued.
4. **`:41` — incomplete to the point of wrong.** "Once in the Voting phase, new
   proposals are no longer accepted." RFC-MACP-0007 §5 rule 6 (`:94`) rejects
   `Proposal`, `Evaluation`, **and `Objection`** after the first accepted
   `Vote`, and the runtime does all three
   (`crates/macp-modes/src/mode/decision.rs` tests
   `proposal_after_voting_rejected:667`, `evaluation_after_voting_rejected:703`,
   `objection_after_voting_rejected:739`). Name all three and cite §5 rule 6.
5. **`:159` — wrong RFC.** "Decision mode's *voting ratio* does exclude abstain
   ballots from its denominator, per RFC-MACP-0004." RFC-MACP-0004 is *Security
   Considerations* and contains no occurrence of "abstain" or "denominator". The
   owner is RFC-MACP-0012 §4.1 **"Denominator"** (`:137`). Re-cite.
6. **`:130` and `:148` — wrong sections of RFC-MACP-0011.** The
   threshold-*replaces*-`required_approvals` rule is **§5 rule 6**, not §6
   ("Terminal semantics"); the negative-commitment trigger is **§5 rule 4a**,
   not §4 ("Message types"). Spell them "§5 rule 6" / "§5 rule 4a".
7. `:57` — qualify "the original proposal stays live and participants can accept
   either" with "unless withdrawn", and cite RFC-MACP-0008 §5 rule 2a.
8. `:150-155` — the `counted > 0` readiness guard is a **deliberate deviation**
   from RFC-MACP-0011 §5 rule 6, which expects an unreachable `n_of_m` override
   to leave the session eligible for a rule-4b negative commitment. The runtime
   instead requires a ballot (issue #145). Say so explicitly with the reasoning,
   rather than presenting it as the RFC's own rule.
9. `:67`, `:79`, `:81` — add the missing citations and one missing rule:
   RFC-MACP-0009 §5 rule 3b (a `TaskAccept` is irrevocable absent a
   reassignment policy — stated nowhere in the docs) at `:67`; RFC-MACP-0010 §5
   rule 5 at `:79`; and at `:81` drop "The protocol allows this" unless a
   section actually licenses late context (§5 rule 2 only requires an existing
   `handoff_id`).
10. `:175` — "Extension mode names **must not** use the reserved `macp.mode.*`
    namespace" is stricter than RFC-MACP-0002 §3/§11, which say **SHOULD**. The
    runtime enforces MUST (`crates/macp-modes/src/mode_registry.rs:596-604`).
    Mark it as a runtime-stricter rule rather than implying the RFC requires it.

**Acceptance criteria**

- The revision-gating table has a rev-3 row, "(current)" sits on it, and no
  sentence in the file says sessions started by this release are rev 2.
  Cross-check `grep -n 'CURRENT_SEMANTICS_REV' crates/macp-core/src/session.rs`.
- `grep -n 'JSON-encoded payloads rather than protobuf' docs/modes.md` returns
  nothing; the multi_round paragraph names the tie-break and links the contract
  section **without** reproducing the collision byte-length vectors.
- `grep -c 'Withdraw' docs/modes.md` ≥ 1 (currently 0 repo-wide).
- The Proposal section states the domain is `{Live, Withdrawn}` citing both
  `ProposalDisposition` and `tests/parity/contract.json`'s
  `proposal_disposition` section, rather than re-deriving the set; verify with
  `python3 -c "import json;print(json.load(open('tests/parity/contract.json'))['sections']['proposal_disposition']['mode_state_dispositions'])"`.
- `grep -n 'RFC-MACP-0004' docs/modes.md` returns nothing.
- No `RFC-MACP-0011 §6` or `§4a` citation remains; both read "§5 rule …".
- The post-Vote rejection set names `Proposal`, `Evaluation`, **and**
  `Objection`.
- Every deviation from an RFC (items 8, 10) is labeled as a deviation.
- `docs/modes.md` and `README.md:16` no longer disagree about multi_round's
  encoding.

**How a verifier checks:** read the two sections against
`crates/macp-modes/src/mode/proposal.rs:364-381` and
`parse_contribute_value`; run the two commands above; and open each cited RFC
section in `/Users/Shared/multiagentcoordinationprotocol/multiagentcoordinationprotocol/rfcs/`
to confirm the section number resolves to the rule claimed. **No new tests** —
the existing unit tests named above are the behavioural proof the prose
describes; cite them, do not add to them.

**Risk: simple.** Additive within existing sections, one deleted wrong sentence,
citation corrections. No headings move.

---

### Phase 3 — the `after_sequence` contract: correct it once, cite it everywhere

**Delivers** the headline duplication finding, and repairs six broken inbound
cross-repo links.

**Files:** `docs/API.md`, `docs/sdk-guide.md`, `docs/examples.md`,
`docs/architecture.md`, `docs/getting-started.md`

**Changes**

1. **Correct all four sites** (`docs/API.md:92`, `docs/sdk-guide.md:101`,
   `docs/examples.md:73`, `docs/architecture.md:135`) to one shared, accurate
   sentence: `after_sequence` is the **1-based ordinal of accepted
   session-scoped envelopes** and is **exclusive** — replay resumes at
   `after_sequence + 1`; `0` replays from the session's first accepted envelope.
   **Cite RFC-MACP-0006 §3.2 "Sequence semantics"** at each site rather than
   re-explaining it; model the wording on `README.md:54`, which is already
   right. Note that internal entries consume no ordinals (which is why this is
   not a log index) — `docs/API.md:192-218` already establishes that for
   `SessionCancel`/`Suspend`/`Resume`, so link it.
2. **Add the two missing §3.2 rules** to `docs/sdk-guide.md`'s streaming
   section (`:96-116`), where an SDK author will look:
   - **Compaction** — a resume whose `after_sequence` falls below the compacted
     base is rejected with `FAILED_PRECONDITION`. The runtime already
     implements this (`src/server.rs:519` emits "resume with after_sequence >=
     {base} or re-read state via GetSession").
   - **Redelivery** — a client MUST key dedup on `message_id`, and a
     redelivered envelope MUST NOT advance its sequence position.
   Both as one clause each plus the §3.2 citation.
3. **Rename two headings to repair inbound links.** Six live links in the SDK
   repos point at anchors that do not exist here:
   - `docs/API.md:400` is `### Rate limits` → anchor `#rate-limits`, but
     `docs/API.md#rate-limiting` is cited by
     `macp-sdk-python/docs/security.md:6,67` and
     `macp-sdk-typescript/docs/guides/security.md:6,69` (**4 links**).
   - `docs/getting-started.md:155` is `## Authentication` → anchor
     `#authentication`, but `docs/getting-started.md#authentication-configuration`
     is cited by `macp-sdk-typescript/docs/guides/authentication.md:20,202`
     (**2 links**).
   Prefer renaming the headings here (to `### Rate limiting` and
   `## Authentication configuration`) over asking two repos to change six links
   — the runtime is the cited party. If a heading rename would break a
   *different* inbound anchor, add an explicit anchor instead rather than
   trading one breakage for another.

**Acceptance criteria**

- `grep -rn 'log index' docs/` returns **nothing**.
- All four sites state `after_sequence` is exclusive and 1-based over accepted
  envelopes, and each carries an RFC-MACP-0006 §3.2 citation.
- No site re-derives the rule at length; each is one sentence plus the citation.
- `docs/sdk-guide.md`'s streaming section mentions `FAILED_PRECONDITION` on a
  compacted-base resume and the `message_id` redelivery rule.
- `grep -n '^### Rate limiting' docs/API.md` and
  `grep -n '^## Authentication configuration' docs/getting-started.md` both
  match (or an equivalent explicit anchor exists).
- Re-running
  `grep -rn 'API.md#rate-limiting\|getting-started.md#authentication-configuration' ../macp-sdk-python ../macp-sdk-typescript`
  now resolves against a real heading in this repo.

**How a verifier checks:** the `log index` grep, the two heading greps, then
read RFC-MACP-0006 §3.2 (`rfcs/RFC-MACP-0006-transport-bindings.md:130`) beside
each of the four corrected sites and confirm they agree. Confirm against code at
`src/server.rs:514` (`get_session_envelopes_after`) and `:519`. **No tests** —
but note the runtime's *code* is already correct, so this phase cannot introduce
a behavioural regression; it is purely a documentation correction.

**Risk: simple.** Four sentence rewrites, two additive paragraphs, two heading
renames. The heading renames are the only part that touches anchors, and they
*repair* breakage rather than cause it — but run the anchor grep from Phase 1
before renaming.

---

### Phase 4 — `docs/sdk-guide.md`: cross-references and the 0.8.0 client contracts

**Status: DONE** (`8f131f1`; 1 verify round, PASS). No divergence from the
plan as written -- all six changes applied as specified and confirmed against
source (`crates/macp-pb/build.rs`, root `Cargo.toml`'s `macp-proto` pin, the
spec repo's `registries/error-codes.md`, the TypeScript SDK's own README).

**Delivers** the SDK-boundary fix (additive cross-refs, nothing moved out) and
the client-facing half of the 0.8.0 major.

**Files:** `docs/sdk-guide.md`

Note: Phase 3 already edited this file's streaming section. Sequence Phase 4
after Phase 3 to avoid conflicting edits.

**Changes**

1. `:1-5` — add a **"Reference implementations"** note naming
   `macp-sdk-python` (`pip install macp-sdk-python`, currently 0.14.1) and
   `macp-sdk-typescript` (`npm install macp-sdk-typescript`, currently 0.14.1)
   with repo links, and stating that **installation, language-specific API, and
   client usage live in those repos** while this page specifies only the
   runtime-side gRPC contract an SDK must satisfy. Write the boundary down —
   it is currently implicit and the page reads as if no SDK exists.
   **Do not pin the SDK version numbers in prose** (they move independently);
   link instead.
2. `:14`, `:58` — the Message-ID guidance gains the reserved namespace: in a
   `macp.mode.handoff.v1` session at `semantics_rev >= 2`, a client envelope
   whose `message_id` begins with the literal `implicit-accept:` is rejected
   with `INVALID_ENVELOPE` whatever its `message_type`; the match is
   case-sensitive. One clause plus a link to `docs/API.md#send` (which carries
   the full rule at `:76`) and RFC-MACP-0010 §5.1(3) — **not** a copy of that
   paragraph.
3. Streaming section — add the consequence an SDK must handle: a stream can
   deliver a `HandoffAccept` that **no participant sent** (the runtime's
   synthetic implicit accept); it consumes an accepted ordinal, replays
   identically, carries `payload.implicit = true`, and its `timestamp_unix_ms`
   is the computed deadline, not the observation time. A client may not
   originate one. Link `docs/modes.md#implicit-accept-rfc-macp-0010-51`.
4. **`:64-76` — error handling.** Have this section **cite
   `registries/error-codes.md`** in the spec repo for the canonical code list
   and its HTTP status mapping, which the runtime docs never reference. Two
   related defects:
   - `macp-sdk-python/docs/guides/error-handling.md:3` tells readers the
     canonical list "with HTTP status mappings" lives in
     `docs/API.md#message-transport` — but `docs/API.md:41-80` contains no
     error-code list and no HTTP statuses. Either make that true or (preferred)
     point at the registry and note the Python SDK's link should be retargeted
     (record it as a cross-repo follow-up, do not edit the SDK repo in this PR).
   - `:72-76` classifies `RATE_LIMITED` and `INTERNAL_ERROR` as
     **Transient / retry: yes**, while the registry marks every code
     `permanent`. The retry advice is defensible and useful, but must be
     labeled **runtime-local operational advice layered on the registry's
     semantics**, not presented as the registry's classification.
5. `:43-62` — the Envelope field table restates `macp-proto`'s own definition.
   Keep it (it is genuinely useful inline) but add a one-line canonical pointer
   to `macp-proto` and RFC-MACP-0001 §6 so a reviewer knows what to diff.
6. `:161-177` — the proto list. Add the missing
   `macp/modes/multi_round/v1/multi_round.proto` (compiled at
   `crates/macp-pb/build.rs:20`; present in `macp-proto` 0.1.10) — without it
   an SDK author cannot find the `Contribute` payload at all. **Name the
   `macp-proto` version** the list corresponds to (root `Cargo.toml`
   `[workspace.dependencies]`) or point at the crate's own file list instead of
   hardcoding eight paths. Mention the TypeScript SDK's GitHub-Packages proto
   distribution (`@multiagentcoordinationprotocol/proto`, needs `.npmrc` + a
   PAT with `read:packages`) as an alternative source.

**Acceptance criteria**

- The file names both SDK repos with links and states in one sentence that
  client-side install/usage is **their** documentation.
- **No SDK installation command, no language-specific client API, and no pinned
  SDK version number is added.** A reviewer checks by confirming the diff
  contains no `pip install`/`npm install` *instruction block* and no `0.14.x`
  string. (A bare package name inside the cross-reference sentence is fine; a
  usage example is not.)
- The proto list includes `multi_round.proto` and either names the `macp-proto`
  version or defers to the crate; its entries match
  `crates/macp-pb/build.rs:12-20`.
- The error section cites the spec's `registries/error-codes.md`, and the retry
  table is explicitly labeled runtime-local advice.
- The reserved `implicit-accept:` namespace and the synthetic-accept delivery
  both appear, each as a clause plus a link rather than a restatement of
  `docs/API.md:76` / `docs/modes.md:87-103`.

**How a verifier checks:** read the file; diff its proto list against
`crates/macp-pb/build.rs:12-20`; confirm the two 0.8.0 paragraphs are
links-plus-one-clause; grep the diff for `pip install`/`npm install`/`0.14`.
**No tests.**

**Risk: simple.** Purely additive apart from the error-table relabeling.

---

### Phase 5 — `docs/policy.md` + `docs/API.md`: normative gaps, and one escalation

**Status: DONE** (`c788041`; 1 verify round, PASS). Divergence from the plan:
(1) also fixed the carried-forward G9 finding from Phase 1
(`docs/policy.md:230`'s "applies across all modes" claim for
`commitment.authority` -- false for extension modes, which never call
`check_commitment_authority`), per Phase 1's closeout note asking Phase 5
to fix both README's and policy.md's versions together. (2) also fixed two
instances of the same RFC-MACP-0011 §6/§4a miscitation in `docs/deployment.md`
(outside this phase's stated file list) since the plan's own acceptance
criterion demanded a clean repo-wide grep. Item 1's escalation resolved as
branch (c): the runtime conforms to RFC-MACP-0012 §4.1's literal legacy-arm
text and the conformance corpus; §5.2's claimed schema_version divergence is
an RFC-internal inconsistency with no possible instance given all three
`policy.std.*` profiles set `require_vote_quorum: true`. Filed upstream as
[multiagentcoordinationprotocol#181](https://github.com/multiagentcoordinationprotocol/multiagentcoordinationprotocol/issues/181);
recorded as `DECISIONS.md` D57. No runtime change, no `macp-runtime` issue.

**Delivers** four policy-doc defects the release commits did **not** close —
plus a finding that must be investigated, not edited.

**Files:** `docs/policy.md`, `docs/API.md`

**⚠️ Changes 1 is an investigation, not an edit.**

1. **ESCALATE, do not edit: `docs/policy.md:288`.** The paragraph asserts the
   three `policy.std.` profiles "produce the **same decision** at every tally
   under both readings", explicitly retracting an earlier claim of divergence.
   RFC-MACP-0012 §5.2 (`rfcs/RFC-MACP-0012-policy.md:255`) states the opposite:
   they "differ only where an empty decisive tally nonetheless clears the
   participation floor, which affects **all three** profiles: a lone abstention
   satisfies the one-vote count quorum of both `policy.std.majority` and
   `policy.std.unanimous` — tallies that schema-version-1 semantics allow to
   support a positive commitment and that `schema_version >= 3` denies."

   **Required procedure:**
   a. Determine empirically which is right — probe the shipped
      `DefaultPolicyEvaluator` against each of the three canonical profiles at
      an empty decisive tally with one abstention, at `schema_version` 1 and 3.
      The existing `crates/macp-policy` unit tests and
      `tests/conformance/` fixtures are the place to look first.
   b. **If the runtime diverges from §5.2** → this is a conformance finding, not
      a docs bug. **File an issue**, leave `:288` in place with a short
      "known deviation, tracked at #NNN" note, and do **not** silently rewrite
      the paragraph to match the spec. A docs-only PR must not paper over a
      behavioural gap.
      **If the paragraph is simply wrong** → correct it and cite §5.2.
      **If §5.2's claim does not hold for this runtime for a legitimate reason**
      (e.g. all three set `require_vote_quorum: true`, which is what the
      paragraph argues) → keep the conclusion but cite §5.2 explicitly and
      explain why it does not apply, so the next reader does not re-litigate it.
   c. Either way, record the outcome in `DECISIONS.md` — the paragraph's
      self-correcting history shows this question has already been answered
      wrongly once.

2. **`docs/policy.md:228-244` — the missing `FORBIDDEN` carve-out.** The
   "Commitment authority" table (`:232-237`) and the error table (`:241-244`,
   POLICY_DENIED = "governance rules are not satisfied") never state that
   breaches of `commitment.authority` / `commitment.designated_roles` are
   **`FORBIDDEN`, not `POLICY_DENIED`**. Owners: RFC-MACP-0012 §10 (the
   POLICY_DENIED row's stated exception — "those are sender-authorization
   failures and use `FORBIDDEN`") and RFC-MACP-0002 §6.1 (`:133-168`, the full
   three-code mapping). **The SDK repos link
   `docs/policy.md#commitment-authority` four times**, so this omission
   propagates downstream. Add one row and cite both sections.
3. **`docs/policy.md:52-54` + `docs/API.md:319` — a now-forbidden behavior
   presented as a design choice.** The docs say "unknown fields are ignored"
   and "a rule the canonical schema forbids but this list does not name is
   accepted". RFC-MACP-0012 §4 (`:105`) now requires the opposite: "Rule objects
   are **closed**… a runtime **MUST reject at admission** a `PolicyDescriptor`
   whose `rules` object carries an undefined key at any nesting level. Silently
   ignoring an unrecognized key is the more dangerous behavior." The docs are
   honest about what the runtime does; they must also say it is a **known
   deviation from §4**, with a tracking link. Also entirely absent from
   `docs/policy.md`: §4's reserved `^[_$]` annotation namespace (`:107-113`) and
   §8's "`rules` equality is taken modulo the `^[_$]` annotation namespace".
   Add both. **If closing the deviation is in fact code work, say so and file
   an issue — do not imply the runtime already rejects unknown keys.**
4. **`docs/policy.md:55` — state the pin.** The doc correctly explains that the
   canonical schemas live in the spec repo and that the value-domain table at
   `:58-66` is a mirror "pinned to it by a parity test that runs in CI" — the
   right instinct. What it lacks, versus the `tests/parity/SOURCE.md` model, is
   the pin itself: no `SPEC_REV`, no test name, no env var, no CI job name, so a
   reader cannot tell which spec revision the table was copied from. Add one
   line: copied from `schemas/json/policy/*.schema.json` at `SPEC_REV`; pinned
   by `enum_lists_match_the_canonical_schemas` in the `conformance-oracle` job;
   see `docs/deployment.md#governance-policy-files`.
5. **`docs/policy.md:131-139` — label the high-churn restatement.** The
   six-row threshold table is **accurate today** (each row was checked against
   RFC-MACP-0012 §4.1:128-148) and the SDKs read it, so keep it. But §4.1 has
   absorbed spec #99, #112, #122 and `1.6.0-draft` within this release cycle
   alone. Add a header line: "canonical: RFC-MACP-0012 §4.1 +
   `schemas/json/policy/decision-rules.schema.json` @ `SPEC_REV`" so a reviewer
   knows exactly what to diff.
6. **`docs/policy.md:205`, `:212` — wrong RFC-MACP-0011 sections**, same
   defect as Phase 2 item 6: §6 → **§5 rule 6**; §4a → **§5 rule 4a**.
   Also `:211`'s "the `ApprovalRequest` is refused" compounds the §5 rule 6
   deviation — cross-reference Phase 2 item 8's deviation note rather than
   restating it.
7. **`docs/API.md:78` — separate spec requirements from runtime caps.** The
   sentence mixes them: RFC-MACP-0001 §7.1 (`:205-222`) requires `intent` (MAY
   be empty), `mode_version`, `configuration_version`, `ttl_ms > 0`,
   `participants` (when required by the Mode), and `policy_version` **present**
   (MAY be empty). The `ttl_ms` ceiling of `86400000` and the 1000-entry
   participant cap are **runtime limits with no spec basis**. Cite §7.1 for the
   required set, then list the two numeric caps under a "runtime limits"
   sub-bullet. Add the missing `intent` and `policy_version` presence rules.
8. **`docs/API.md:307` and `docs/examples.md:85` — docs invite a refused
   operation.** "optionally assigning a new identifier" / "optionally renaming
   it" for `PromoteMode`. RFC-MACP-0002 §12 (`:248`): the runtime "**MUST NOT**
   rename it to the `macp.mode.*` namespace unless…", and
   `crates/macp-modes/src/mode_registry.rs:596-604` enforces it
   unconditionally. Add the constraint and cite §12. (`docs/examples.md:85` is
   edited here rather than in Phase 6 because it is the same rule — keep both
   in one commit.)
9. `docs/API.md:319` — collapse the paragraph that repeats `policy.md:52`'s
   prose a third time into the pointer that already exists at `:321`.

**Acceptance criteria**

- **`docs/policy.md:288` is either corrected-with-a-§5.2-citation, or carries a
  "known deviation, tracked at #NNN" note with a real issue link.** It is not
  left as-is with no citation, and it is not silently rewritten to match the
  spec without the investigation in 1(a) having been done. The `DECISIONS.md`
  entry exists.
- The commitment-authority section states that `commitment.authority` /
  `designated_roles` breaches return `FORBIDDEN`, citing RFC-MACP-0002 §6.1 and
  RFC-MACP-0012 §10.
- The unknown-fields behavior is labeled a known deviation from RFC-MACP-0012
  §4 with a tracking link; the `^[_$]` annotation namespace is documented.
- `docs/policy.md:55` names `SPEC_REV`, the test, and the CI job.
- The §4.1 threshold table carries a canonical-source header line.
- `grep -n 'RFC-MACP-0011 §6\|RFC-MACP-0011 §4a' docs/` returns nothing.
- `docs/API.md`'s `SessionStart` contract cites §7.1 and separates the two
  runtime caps from the spec-required fields; `intent` and `policy_version`
  presence both appear.
- `PromoteMode`'s rename constraint appears in both `docs/API.md` and
  `docs/examples.md`.

**How a verifier checks:** open each cited RFC section and confirm it says what
the doc now claims; confirm the issue links resolve; read `DECISIONS.md` for the
item-1 entry. For item 1 specifically, the verifier must confirm **an
investigation happened** — a corrected paragraph with no evidence behind it
fails this phase.

**Risk: complex.** Not because the edits are large, but because item 1 may
surface a runtime conformance gap, and items 3 and 5 describe behavior that may
need code work to close. The failure mode is a docs-only PR that makes the
runtime *look* conformant. If any of 1, 3 is found to be a code gap, the
correct output is a doc note plus a filed issue — **never** prose asserting
conformance the code does not have.

---

### Phase 6 — version, enumeration, and env-var sweep

**Delivers** every remaining `0.5.0`-era string, the stale enumerations, and
five production env vars missing from the deployment reference.

**Files:** `docs/README.md`, `docs/API.md`, `docs/getting-started.md`,
`docs/examples.md`, `docs/architecture.md`, `docs/deployment.md`

**Changes**

| Spot | Now | Should be |
|---|---|---|
| `docs/README.md:3` | `**Version**: v0.5.0` | drop the pinned version; keep `**Protocol**: MACP 1.0` |
| `docs/README.md:11`, `:25` | "24 gRPC RPCs" | **correct** — verify only, no edit |
| `docs/API.md:32` | `version: "0.5.0"` | it is `env!("CARGO_PKG_VERSION")` (`src/server.rs:820`) — say so, drop the literal |
| `docs/getting-started.md:73` | `version: "0.5.0"` in the sample | mark illustrative or make current |
| `docs/getting-started.md:202` | "see CHANGELOG 0.5.0" | drop the stale pointer, keep the HS256 reasoning |
| `docs/deployment.md:292` | "(see CHANGELOG 0.5.0)" | same |
| `docs/examples.md:7` | "All examples target `macp-runtime v0.5.0`" | drop the version pin |
| `docs/architecture.md:209` | 3 lifecycle events | all six (`src/runtime.rs:27-34`) |
| `docs/architecture.md:211` | "every 60 seconds" | correct (default) — keep the `MACP_CLEANUP_INTERVAL_SECS` parenthetical |

**Plus: five production env vars are read in `src/main.rs` but appear in
`docs/deployment.md` nowhere — not its table, not its prose.** They are in
`CLAUDE.md`'s table only. Add rows to `docs/deployment.md`'s "Environment
variables" table:

| Variable | Read at | Default |
|---|---|---|
| `MACP_METRICS_ADDR` | `src/main.rs:584` | — (off) |
| `MACP_CONCURRENCY_LIMIT_PER_CONNECTION` | `src/main.rs:456` | `64` |
| `MACP_MAX_CONCURRENT_STREAMS` | `src/main.rs:460` | `128` |
| `MACP_REQUEST_TIMEOUT_SECS` | `src/main.rs:464` | `30` |
| `MACP_SHUTDOWN_DRAIN_SECS` | `src/main.rs:524` | `10` |

Also add `MACP_AUTH_JWT_ALGS` as a table **row** in `docs/deployment.md` (it
currently appears only inline in the resolver-chain bullet at `:292`) and in
`README.md`'s auth table. These are operator-facing (metrics endpoint,
concurrency, timeouts, graceful drain) and their absence from the deployment
reference is a real gap, not churn.

**Otherwise leave `docs/deployment.md` alone** — its ten-item upgrade guide and
tag contract are current, and churn there is pure risk. (The one factual
correction it does need is in Phase 7, item 2.)

**Acceptance criteria**

- `grep -rn '0\.5\.0' README.md docs/*.md CONTRIBUTING.md` returns only
  intentional history: `docs/change-review-phases-a-e.md` (out of scope) and
  `CONTRIBUTING.md:123`'s "historical v0.5.0 build log", which is a correct
  historical statement and **stays**.
- No doc states a workspace version as current fact except by pointing at
  `[workspace.package]`.
- Every lifecycle-event enumeration in scope lists six variants or links
  `docs/API.md#watchsessions`.
- `grep -rn 'CHANGELOG 0\.5\.0' docs/` returns nothing.
- Every `MACP_*` variable read outside a `#[cfg(test)]`/`tests/` path appears in
  `docs/deployment.md`'s table. Check with
  `grep -rhoE '"MACP_[A-Z0-9_]+"' --include='*.rs' src/ crates/ | sort -u` and
  diff against the table.

**How a verifier checks:** the three greps, the env-var diff, and a read of
`docs/architecture.md:200-215` against `src/runtime.rs:27-34`. **No tests.**

**Risk: simple.** Single-line substitutions plus six additive table rows.

---

### Phase 7 — provenance and operations: conformance SOURCE.md, the pin claim, docker-compose

**Delivers** the third pin-discipline gap, one factually wrong sentence, and the
one undocumented added artifact.

**Files:** `tests/conformance/SOURCE.md` (new), `docs/testing.md`,
`docs/deployment.md`

**Changes**

1. **`tests/conformance/` is a vendored spec artifact with no provenance
   file.** All **34 `.json` fixtures plus `cmt-hash/` are byte-identical** to
   the spec repo's `schemas/conformance/`, and they are gated by exactly the
   same mechanism as `tests/parity/` — `.github/workflows/ci.yml:632-633` calls
   `check_dir` on both, bidirectionally, against the spec checked out at
   `ref: ${{ env.SPEC_REV }}` (`ci.yml:592`). Yet `tests/parity/` has a
   33-line `SOURCE.md` and `tests/conformance/` has **none**, and
   `docs/testing.md:15` describes the fixtures as if locally authored ("define
   mode lifecycles as JSON files and verify that each mode's happy path and
   reject paths produce the expected results") with no mention of vendoring,
   `SPEC_REV`, or a re-vendor command.
   **Add `tests/conformance/SOURCE.md` modeled verbatim on
   `tests/parity/SOURCE.md`** (sha + date + the `check_dir` explanation + the
   re-vendor command), and amend `docs/testing.md:15` to say it is a
   byte-identical vendored copy of the spec repo's `schemas/conformance/` at
   `SPEC_REV`, pointing at the new file.
   *Scope note:* `tests/conformance/SOURCE.md` is a **new** provenance file, not
   an edit to a CI-gated artifact. Confirm it falls outside `check_dir`'s
   `*.json` glob (as `tests/parity/SOURCE.md` explicitly does) so adding it
   cannot red the `conformance-oracle` job.
2. **`docs/deployment.md:235` is factually wrong and contradicts
   `docs/testing.md:133`.** It states "**CI checks the spec repo out at `main`
   with no pinned ref**, so a sibling checkout that is dirty, or on a local
   branch ahead of `main`, produces parity failures that do not exist in CI".
   But `ci.yml:588-593` **does** pin (`ref: ${{ env.SPEC_REV }}`), and
   `docs/testing.md:133` says so correctly ("reads the spec repo at a pinned
   revision (`SPEC_REV` in `ci.yml`), **not at its default branch**").
   The *advice* (export with `git archive`, never point at the sibling tree)
   stays correct; only the stated reason is wrong. Replace "at `main` with no
   pinned ref" with "at `SPEC_REV`", keeping the export-first rule with the real
   reason: the sibling tree may be dirty or ahead of the pin.
   **Note:** `CLAUDE.md` §9 carries the same wrong claim ("CI checks the spec
   repo out at `main` with no pinned ref" is mirrored in its
   `MACP_POLICY_SCHEMAS_DIR` paragraph) — fix both; Phase 8 covers the CLAUDE.md
   half.
3. **Document `docker-compose.yml`** in `docs/testing.md`'s "Running
   integration tests" section (`:74-103`). Read the file first; per its own
   header comments it defines one service, `integration-tests`, invoked as:
   ```
   docker compose build integration-tests
   docker compose run --rm integration-tests          # tier 1 + 2 (default CMD)
   docker compose run --rm integration-tests cargo test --test tier3 -- --ignored --test-threads=1
   ```
   with `OPENAI_API_KEY` (from a gitignored root `.env` or the host) and
   `RUST_LOG` (default `warn`) passed through, backed by
   `integration_tests/Dockerfile`. State that **CI does not use it** — the
   `integration` job runs the suite directly — so it is a contributor
   convenience, and add it to the Configuration table's neighbourhood rather
   than implying it is the canonical path.
   Also add `MACP_CONFORMANCE_FIXTURES_DIR` (`tests/conformance_loader.rs:531`,
   also set by `ci.yml`'s `conformance-oracle`) to `docs/testing.md`'s
   Configuration table — it is currently the one env var documented **nowhere**.
   **Do not add a Makefile target** in this docs-only PR; note it as a
   follow-up.

**Acceptance criteria**

- `tests/conformance/SOURCE.md` exists, names the same sha as
  `.github/workflows/ci.yml:39` and `tests/parity/SOURCE.md`, and carries a
  re-vendor command.
- `docs/testing.md:15` states the fixtures are vendored at `SPEC_REV` and links
  the new `SOURCE.md`.
- `grep -n 'with no pinned ref' docs/ CLAUDE.md` returns nothing.
- `docs/deployment.md` and `docs/testing.md` no longer contradict each other on
  whether CI pins the spec checkout.
- `docs/testing.md` documents `docker-compose.yml` with an invocation matching
  the file's real service name, and says whether CI uses it.
- `MACP_CONFORMANCE_FIXTURES_DIR` appears in `docs/testing.md`.
- `cargo test --test parity_contract` and the `conformance-oracle` job's
  `check_dir` still pass — i.e. adding `SOURCE.md` did not red the gate. **This
  is the one phase with a real mechanical check beyond greps**: run
  `cargo test --test parity_contract` locally.

**How a verifier checks:** the two greps, read the new `SOURCE.md` against
`tests/parity/SOURCE.md` for structural parity, confirm the shas match
`ci.yml:39`, and run `cargo test --test parity_contract`.

**Risk: simple.** One new provenance file, one factual correction, one additive
docs section. The only hazard is the new file tripping `check_dir`, which the
acceptance criteria test for directly.

---

### Phase 8 — `CLAUDE.md`

**Delivers** the agent/contributor brief brought current. **Reminder: this file
is gitignored, so none of this appears in the PR diff.**

**Files:** `CLAUDE.md` (untracked)

**Changes**

1. `:7` — "workspace version `0.7.6`, with G4 cutting **0.8.0**" → `0.8.7`.
   Keep the existing caveat (read `[workspace.package].version` rather than
   trusting the line); drop the spent "G4 cutting 0.8.0" clause.
2. **`:183` — "bearer token or dev header" → bearer token only.** The
   `x-macp-agent-id` dev header is read by no production path
   (`src/server.rs:2363`, guarded by the assertion at `:2379-2380`); the
   non-test region of `crates/macp-auth/src/security.rs` reads only
   `x-macp-token` (`:402`). Dev-mode auth is a bearer-token fallback.
3. **§8a — the sealed-type list is 7 of 18.** Add the 11 missing:
   `MultiRoundState`; `ProposalRecord`, `ProposalState`, `RejectRecord`,
   `TerminalRejectRecord`; `TaskRecord`, `TaskRejectRecord`,
   `TaskUpdateRecord`, `TaskCompleteRecord`, `TaskFailRecord`, `TaskState`.
   Verify with
   `grep -rn -A1 'non_exhaustive' crates/macp-modes/src/mode/*.rs crates/macp-storage/src/registry.rs | grep -c 'pub struct'`
   → must read 18. Note `PersistedRoot` was **not** sealed.
   Also extend the "deliberately not sealed" enum list, which names 3 of 8:
   add `ProposalDisposition` (`proposal.rs:17`), `ProposalPhase` (`:23`),
   `TaskTerminalReport` (`task.rs:95`), `CriticalObjectionAction`
   (`macp-core/src/policy/rules.rs:105`), `EffectiveThreshold` (`rules.rs:289`).
4. `:142` — "Tier 1 (100 tests + 8 JWT)". The real count is 122 + 8, but it has
   walked 118→143 in this window alone. **Drop the number**, matching
   `docs/testing.md`'s deliberate choice.
5. **Add the `reflection` Cargo feature** — CLAUDE.md mentions it nowhere, and
   it is the only runtime-behaviour feature flag absent from the file. Consider
   a short feature inventory (`rocksdb-backend`, `redis-backend`, `otel`,
   `reflection`) since the per-crate table only covers the storage two.
6. **Env table** — add `MACP_ROCKSDB_PATH` (`src/main.rs:273`) and
   `MACP_REDIS_URL` (`src/main.rs:279`), both present in
   `docs/deployment.md:209-210` but missing here. Add `MACP_PARITY_CONTRACT`
   (`tests/parity_contract.rs:96`) and `MACP_CONFORMANCE_FIXTURES_DIR` as
   dev/CI-only, alongside the existing `MACP_POLICY_SCHEMAS_DIR`.
7. **§9** — record that the evaluator supports `schema_version` `{1, 2, 3}`
   (registration rejects only `0`), and that a zero-participant Decision
   `SessionStart` is accepted and inert. Fix the same "spec repo checked out at
   `main` with no pinned ref" error Phase 7 item 2 fixes in
   `docs/deployment.md`, and name the `SPEC_REV` pin.
8. **Mention `spec-drift.yml`** — the one workflow CLAUDE.md never names
   (10 workflows exist; it describes the drift watcher's behaviour in §9's
   neighbourhood without naming the file or the `SPEC_REV` pin mechanism).
9. **Key files table** — add `src/watch_sync.rs`, `tests/parity/`,
   `tests/parity_contract.rs`.
10. Makefile targets — add the missing `test-integration-hosted`
    (`Makefile:49`, documented at `docs/testing.md:102`).
11. Add a `semantics_rev` line noting the constant is now **3** and what each
    revision gates, so the file does not drift the way `docs/modes.md` did.

**Acceptance criteria**

- `grep -n 'dev header' CLAUDE.md` returns nothing.
- `grep -n '0\.7\.6\|G4 cutting' CLAUDE.md` returns nothing.
- §8a lists 18 sealed structs; the count matches the `grep -c` above.
- No Tier-1 test count appears.
- `grep -c 'reflection' CLAUDE.md` ≥ 1.
- `grep -n 'with no pinned ref' CLAUDE.md` returns nothing.
- Every `MACP_*` read outside tests appears in CLAUDE.md's table or
  `docs/deployment.md`'s; no table entry is unread by code.
- The PR body states that CLAUDE.md changes are not in the diff.

**How a verifier checks:** run the greps and the env-var diff **against the
local file** — the PR diff will not show these changes, so a verifier working
only from the PR cannot complete this phase and must open `CLAUDE.md` directly.
**No tests.**

**Risk: simple.** Additive and corrective in an untracked file.

---

## PR strategy

**One PR, eight commits (one per phase).**

Why one PR: the change is docs-only, fully reversible with a single revert, and
touches no code path, no test, and no CI config — so there is no incremental
risk to isolate. Splitting would also fragment the two things a reviewer most
needs to see whole: that `README.md:16`, `docs/modes.md:169`, and
`docs/sdk-guide.md:166-174` stop disagreeing about multi_round's encoding, and
that all four `after_sequence` sites plus `README.md:54` now state one rule.
Commit per phase so each stays individually revertible inside the PR.

**One exception that can force a second PR:** if Phase 5 item 1 (or item 3)
turns out to be a runtime **conformance** gap rather than a doc error, the code
fix does **not** belong in this PR. File the issue, land the doc note here, and
plan the fix separately.

Suggested title: `docs: bring living docs current for 0.7.3 → 0.8.7`

The PR body must carry four notes the diff cannot show:
1. `CLAUDE.md` edits are **not in the diff** (`.gitignore:20`).
2. `docs/deployment.md`'s upgrade guide and tag contract, and `docs/policy.md`'s
   #99/#117/#122/#126 coverage, were **audited and deliberately left alone** —
   those commits closed their own doc gaps.
3. The `Withdraw` gap (Phase 2) and the `after_sequence` inversion (Phase 3) are
   **pre-existing**, not introduced in this window; `25e9b97`'s parity wiring
   and the SDK audit are what surfaced them.
4. Any issue filed out of Phase 5, with its number.

---

## Risk summary

| Phase | Risk | Why |
|---|---|---|
| 1 — `README.md` | **complex** | removes/restructures two sections that may be external anchor targets; grep siblings for `README.md#` first |
| 2 — `docs/modes.md` | simple | additive + one deleted wrong sentence + citation fixes |
| 3 — `after_sequence` + anchors | simple | four sentence rewrites; the heading renames *repair* inbound breakage |
| 4 — `docs/sdk-guide.md` | simple | additive; sequence after Phase 3 (same file) |
| 5 — `docs/policy.md` + `docs/API.md` | **complex** | item 1 may surface a conformance gap; the failure mode is prose that makes the runtime look conformant when it isn't |
| 6 — version/env sweep | simple | line substitutions + additive table rows |
| 7 — provenance + compose | simple | one new file; verify it does not trip `check_dir` |
| 8 — `CLAUDE.md` | simple | untracked file, corrective |

All eight are docs-only and revert cleanly. **No code, test, or CI change is in
this plan** — if a phase appears to need one, stop and re-plan rather than
widening the PR. Phases 2 and 3 are the highest value per unit of risk; land
them first if the sweep is cut short.

## Out of scope (do not touch)

- `plans/**` — permanent paper trail (`plans/defer/README.md`).
- `CHANGELOG.md` — release-plz generated.
- `docs/change-review-phases-a-e.md` — a dated change-review record, not living docs.
- `tests/parity/contract.json`, `tests/conformance/*.json` — byte-gated against
  the spec repo by `conformance-oracle`'s `check_dir`. (Adding
  `tests/conformance/SOURCE.md` in Phase 7 is outside that glob — verify.)
- `docs/deployment.md`'s upgrade guide (`:17-196`) and tag contract
  (`:355-401`); `docs/policy.md`'s spec-issue coverage — current.
- The SDK repos themselves. Phase 4 records one cross-repo follow-up (retarget
  `macp-sdk-python/docs/guides/error-handling.md:3`); it is **not** done here.
- PRs #213 / #214 content — open, unreviewed, not landed.

# PROGRESS — `docker.yml` publishes a semver-tagged image on every release (issue #184)

Plan: [`plans/docker-tag-trigger-184.md`](docker-tag-trigger-184.md)
Branch: `feat/docker-tag-trigger-184-phase1`, cut from `main` @ `11429e8` (2026-09-23,
"feat(ci): add docker-compose for running the integration test suite (#189)")
Model tiering for this run: **Opus plans, Opus executes, fresh Opus verifies**. No phase in this
plan is a one-way door; where the skill would escalate to Fable, a fresh Opus agent is used and the
substitution is noted.

**PR strategy: two PRs**, decided at `/implement` §0 (Opus, reversible, no ask). Phase 2 dispatches
`gh workflow run docker.yml --ref main`, which requires `main` to already carry Phase 1's
`workflow_dispatch` trigger — so Phase 1 must be merged before Phase 2 can run at all, and the plan
itself orders Phase 2 before Phase 3 specifically so the release-build path is proven by a
no-cost manual dispatch *before* it is wired into the automatic release pipeline. Phase 4's docs
must also be written after Phase 2's registry probe ("against observed tags rather than intent",
per the plan). So:
- **PR A — Phase 1 only.** `.github/workflows/docker.yml`. Merges, then Phase 2 runs against `main`.
- **PR B — Phases 3+4 together.** `.github/workflows/release-plz.yml` wiring + the docs phase,
  written against Phase 2's actual observed tag list. Low risk, natural single review unit.
- Phase 5 is a post-merge observation of the next real release; no PR.

## Status

| Phase | Title | Status | Verdict | Rounds | Commit |
|-------|-------|--------|---------|--------|--------|
| 0 | Plan + reverify | DONE | FLAWED -> patched (rev 2) | 1 | (uncommitted) |
| 1 | Teach `docker.yml` to do a release build | DONE | GAPS -> GAPS -> PASS | 3 | (pending) |
| 2 | Backfill the image for the current release | TODO | — | — | — |
| 3 | Call `docker.yml` from the release | TODO | — | — | — |
| 4 | Document the published image | TODO | — | — | — |
| 5 | Observe the next real release | TODO | — | — | — |

**Two phases are not code changes, and the plan is not done without them.**

- **Phase 2** is a `gh workflow run` dispatch plus a registry probe. `/implement` (or a human)
  must actually execute it — a plan phase cannot retroactively publish an image for a release that
  was already cut.
- **Phase 5** is an observation of the next real release, and it is the **only** evidence for the
  issue's acceptance criterion 1. Phases 1-3 are verifiable by inspection and by a manual
  dispatch; none of them observes the automatic path firing. Do not report criterion 1 as met
  before Phase 5 is recorded — report it as *implemented but unobserved*.

## Repo map

Written during planning so `/implement` does not re-scan the repo. Line numbers verified against
`main` @ `11429e8`.

> **Do not trust a session-opening git snapshot.** This session's was stale — it named `5e95c4a`
> as HEAD and `49ba49e` as the newest release commit, and `5e95c4a` is not even an ancestor of the
> real HEAD. Re-run `git log --oneline -1` before citing a base commit.

### The files this plan changes

| Path | Purpose |
|------|---------|
| `.github/workflows/docker.yml` | 66 lines, one `docker` job. `on:` at **:6-9** (`push` branches `[main]` + `tags: ["v*"]` — the bug). Checkout **:24-25** (no `ref:`). `docker/metadata-action` step **:40-51**, pinned `dc802804100637a589fabce1cb79ff13a1411302` (v6.2.0); tag rules **:46-51**; **no `flavor:` input**, so `latest=auto` is in force. `build-push-action` **:53-66** with `provenance`/`sbom` **:61-62**; **no `timeout-minutes`**. **Phase 1 rewrites `on:`, adds a resolve step (+ a `ref`-input guard), an explicit checkout `ref:`, a `git rev-parse HEAD` capture, `flavor: latest=false`, an explicit `org.opencontainers.image.revision` label, `timeout-minutes: 90`, and `enable=`/`value=` on every tag rule.** |
| `.github/workflows/release-plz.yml` | 389 lines. `on: push: branches: [main]` **:3-5**; workflow-level `permissions` **:7-9** (`contents: write`, `pull-requests: write` — **no `packages: write`**); header comment explaining call-not-tag **:15-26**; `release-plz` job **:28-39** with `outputs:` **:32-39**; action pin `b5543c19b03be9bd48852d20ca89f478b7723260` (v0.5.132) **:61-62**; `publish` job **:69-74**; `sync-integration-lock` **:92-105+** with the separate-job rationale at **:83-91**. **Phase 3 adds a `releases` output, a `docker` calling job with its own `permissions:`, and extends the header comment.** |
| `docs/deployment.md` | `## Container deployment` at **:327**. Documents only the local `Dockerfile`; no registry reference anywhere in tracked docs. **Phase 4.** |
| `CONTRIBUTING.md` | `### Approving a release PR` at **:134**. **Phase 4** adds one sentence about the image publish. |
| `CLAUDE.md` | **Gitignored in this tree** (same carve-out as `DECISIONS.md` D45). Phase 4 updates it as a local mirror; the edit will NOT appear in the PR diff. Say so in the phase report rather than force-adding it. |

### The files this plan reasons about but does NOT change

| Path | Why it matters |
|------|----------------|
| `.github/workflows/publish.yml` | **The precedent this plan copies.** `on:` **:24-41**: `workflow_call` **:25-33**, `push: tags: ["macp-runtime-v*"]` **:34-35**, `workflow_dispatch` **:36-41**. Header **:15-23** is the canonical statement of why the tag trigger is inert for release-plz's tags — Phase 1(a)'s comment is modelled on it. Must not be disturbed. |
| `release-plz.toml` | `publish = false` **:27**; `git_release_enable = false` **:34** with the umbrella-crate exception **:66-68** ("The one GitHub Release per version rides the umbrella crate's tag (`macp-runtime-v{version}`)"); seven packages in one `version_group = "macp"` **:45-65** — this lockstep is what makes `fromJSON(releases)[0].version` exact. No `git_tag_name` override, so tags use release-plz's default `{{ package }}-v{{ version }}` template. |
| `Cargo.toml` | `[workspace.package]` **:23-24**, `version = "0.8.1"` — the source of truth for "current version" per `CLAUDE.md`, and what Phase 1(e) cross-checks the resolved version against. |
| `.github/workflows/ci.yml` | `docker-build` job **:706-723** — build-only PR gate, part of `ci-pass` **:728**. Explains why `type=ref,event=pr` in `docker.yml` is dead code. Also holds the "Internal crate versions are in lockstep" assertion that backs the `[0]` indexing. Not modified. |
| `.github/workflows/spec-drift.yml` | **:58-71** is the house style for a shell step that emits `$GITHUB_OUTPUT` (`set -euo pipefail`, `::error::`, then `echo "k=v" >> "$GITHUB_OUTPUT"`) and **:77** for a computed `checkout` `ref:`. Phase 1(b) follows both. |
| `Dockerfile` | `FROM rust:1.98-bookworm` — relevant only to Phase 2's "can the old tree still build" risk. |
| `docker-compose.yml` | Builds `integration_tests/Dockerfile` from the checkout; does **not** pull the GHCR image. Unaffected. |

### Evidence gathered during planning (do not re-derive)

| Claim | Evidence |
|---|---|
| No semver image since the scheme changed | GHCR anonymous probe: **109 tags** = 4 semver (`0.4`, `0.4.1`, `0.5`, `0.5.0`) + `latest` + `main` + 5 `pr-*` (`pr-37`..`pr-40`, `pr-43`) + 98 SHA. Nothing for 0.6.x/0.7.x/0.8.x. Independently re-probed by the reviewer with identical results. The `pr-*` tags mean `type=ref,event=pr` is dead *now* but did fire historically. |
| The break is dated | `v0.5.0` 2026-07-06 (last bare tag) → `b908458` 2026-07-10 (release split) → `macp-runtime-v0.6.1` 2026-07-11 (first new-scheme tag). |
| Current release is **0.8.1**, not 0.8.0 | `Cargo.toml:24` = `0.8.1`; `gh release list` newest = `macp-runtime-v0.8.1`, 2026-09-22. The issue says 0.8.0; it is one release stale. |
| `docker.yml` has never run on a tag | `gh run list --workflow=docker.yml --json event,headBranch` — every run is `event=push`, `headBranch=main`. |
| Nothing else tags `v*` | All ten workflows surveyed; `docker.yml:9` is the only `v*` matcher left. |
| `type=semver` cannot parse `macp-runtime-v0.8.1` | `metadata-action@dc80280` `src/meta.ts:155-199` — `procSemver` does no prefix-stripping; `semver.valid()` fails → `core.warning` + `return version`. Silent skip, green job. |
| `value=` works on a branch ref | Guard at `meta.ts:156` is a conjunction (`!isTagRef && value.length == 0`). Covered by the action's own tests `push14` and `push35` (the latter on `refs/heads/master`). |
| `latest=auto` auto-tags `latest` on any resolved semver | `meta.ts:195`; test `push14` asserts `latest` on a branch ref. **This is the criterion-3 regression trap.** Hence `flavor: latest=false`. |
| `context: git` overrides only `ref`/`sha` | `src/context.ts:85-92` + `actions-toolkit@v0.92.0/src/git.ts`. It works for a tag-name checkout but changes every ref-derived rule at once — rejected in favor of `value=`. |
| `releases` output exists at the pinned SHA | `action.yml` @ `b5543c19…` declares five outputs incl. `releases`. Added in **v0.5.54**; pin is **v0.5.132**. No version risk. |
| `releases` is always a single-line JSON **array** | Action's `run:` does `releases=$(… \| jq -c .releases)`; `[]` when nothing released — **never** the literal `{}`. That `{}` quirk is `pr`-only and is already documented at `release-plz.yml:34-37`. `fromJSON` is safe. |
| Every `releases` entry shares one version; `tag` is the full name | One entry per released crate; `tag` renders `{{package}}-v{{version}}` → `macp-runtime-v0.8.1`. Lockstep `version_group` forces one version across all seven. |
| A called workflow cannot exceed the caller's token scope | GitHub reusable-workflow reference: permissions "can be only downgraded (not elevated) by the called workflow". `jobs.<id>.permissions` **is** supported on a `uses:` job. |
| `github` context in a called workflow is the **caller's** | Reference: "the `github` context is always associated with the caller workflow"; event table gives `GITHUB_SHA`/`GITHUB_REF` as "Same as the caller workflow". |
| **`gh workflow run docker.yml --ref macp-runtime-v0.8.1` cannot work** | The dispatch is validated against the file **at the target ref**; the tag's copy has no `workflow_dispatch`. Verified locally: `git show macp-runtime-v0.8.1:.github/workflows/docker.yml` → `on: push: {branches:[main], tags:["v*"]}`, and `grep -c 'workflow_dispatch\|workflow_call'` → **0**. Tags are immutable, so this cannot be repaired. Dispatch against `main` with a `ref` **input** instead. |
| release-plz tags the release-PR merge commit on `main` | `macp-runtime-v0.8.1` → `6128ccc` ("chore: release v0.8.1 (#180)"), an ancestor of `main`. This is what makes Phase 3's "`github.sha` under `workflow_call` **is** the tagged commit" argument true, and why the `workflow_call` path needs no `ref` input. |
| The backfill's `latest` hazard is concrete, not theoretical | `git rev-list --count macp-runtime-v0.8.1..HEAD` = **3**. `main` is three commits ahead of the tag, so an ungated backfill would drag `latest` back three commits. |
| Build cost | Recent `docker.yml` runs: 36-51 min (arm64 via QEMU); one cache-hot run at 1m32s. |

### Downstream consumers to notify (outside this repo, do not edit here)

**One** sibling-repo plan tells readers **not** to pin a semver tag, citing this exact defect:
`plans/cross-repo/macp-sdk-python-examples-docs-and-release.md:101` and `:221` ("Do not pin a
semver tag … the list stops at `0.5.0`/`0.5`"). That is the one to report so the workaround can be
retired.

Two others merely *use* `:latest` and advise nothing about semver pinning — they need no change:
`plans/cross-repo/macp-sdk-python-handoff-implicit-accept-0-8-0.md:34` and
`plans/cross-repo/macp-sdk-typescript-handoff-implicit-accept-0-8-0.md:27,33`. (An earlier draft
of this map claimed all three carried the advice; the reviewer caught the over-claim.)

Issue #184 names a `macp-control-plane` writeup as the origin of the report.

### Conventions to follow

- `ASSUMPTIONS.md` entries: `## Title` then `- **Plan:** / **Assumed:** / **Chose:** /
  **Alternatives:** / **Blast radius if wrong:** / **Status:**`. Currently **4 UNCONFIRMED**.
- `DECISIONS.md` is the durable record `/reconcile` appends to (latest entry D47).
- Workflow edits are validated locally with `actionlint` (**1.7.12**, installed at
  `/opt/homebrew/bin/actionlint`) plus a `yaml.safe_load` parse.
- **Environment:** every cargo command must be prefixed `RUSTC_WRAPPER=""` — the repo's
  `.cargo/config.toml` sets `rustc-wrapper = "sccache"` and the daemon is broken in this sandbox.
  Never run `sccache --stop-server` or `pkill`. (No cargo is needed for Phases 1-4, but the
  Phase 1(e) cross-check parses `Cargo.toml` textually rather than via `cargo metadata` partly for
  this reason — and mainly because the container job should not need a Rust toolchain.)
- GHCR probe (no auth needed, used for Phase 2's acceptance):
  ```bash
  TOKEN=$(curl -s "https://ghcr.io/token?scope=repository:multiagentcoordinationprotocol/macp-runtime:pull&service=ghcr.io" | jq -r .token)
  curl -s -H "Authorization: Bearer $TOKEN" \
    "https://ghcr.io/v2/multiagentcoordinationprotocol/macp-runtime/tags/list?n=1000" | jq -r '.tags[]' | sort
  ```

## Phase 0 — planning + reverification

Three parallel research agents verified the third-party behavior the plan rests on, each against
pinned sources rather than memory: `docker/metadata-action` at `dc80280…`, `release-plz-action` at
`b5543c19…`, and GitHub's own reusable-workflow / `workflow_dispatch` references. Their findings
are folded into the plan and summarized in the evidence table above.

Two of those findings changed the design rather than confirming it:

| Finding | Effect on the plan |
|---|---|
| `type=semver` cannot parse a `macp-runtime-v`-prefixed tag — it warns and skips, green | The issue's suggested glob fix gains a **second** independent reason for being insufficient. Phase 1 drives the version through `value=` and adds a hard failure so a release build can never be a green no-op. |
| `latest=auto` auto-tags `latest` whenever a semver rule resolves | A latent criterion-3 regression: a backfill of an old tag would move `latest` backwards. Phase 1(f) adds `flavor: latest=false` and Phase 1(g) gates `latest` on `push_latest`. |

And one changed the manual step:

| Finding | Effect on the plan |
|---|---|
| `workflow_dispatch` is validated against the workflow file **at the dispatched ref**, and the tag's copy has neither `workflow_dispatch` nor `workflow_call` (verified locally) | `gh workflow run docker.yml --ref macp-runtime-v0.8.1` would 422. Phase 2 dispatches against `main` with the tag as an **input**, which is why Phase 1 gives `workflow_dispatch` a `ref` input at all. |

Planning also dry-ran the proposed workflow rather than only writing it down:

- a scratch copy of the full proposed `docker.yml` passes **`actionlint` clean**;
- the resolve logic was exercised over **nine cases** — the five triggers plus four negatives.
  This is what surfaced the **`ref`-input guard**: a dispatch with `ref: some-branch` produced
  `is_release=false` *and* `push_latest=true`, which would have pushed `latest`, `main` and a SHA
  tag built from the wrong tree. The first draft would have shipped that.

Re-run both against the real file during Phase 1; the scratch result is evidence the design works,
not a substitute for testing the shipped file.

### Reverification (round 1)

A fresh **Opus** reviewer, with no sight of the planning work, returned **FLAWED**: 2 BLOCKERs,
7 SHOULD-FIXes, 6 NITs. The architecture was upheld; the defects were in specification precision
and in what the acceptance criteria could observe. The full findings table and its resolutions are
in the plan's `## Plan review` section. The two blockers, in short:

| ID | Finding |
|----|---------|
| B1 | The `ref` output was specified per *build kind*, which is unimplementable for `workflow_call` — and Phase 1(e)'s `Cargo.toml` cross-check **cannot** catch the resulting mis-checkout, because `main` retains the released version afterwards. Fixed with an exhaustive per-trigger table. |
| B2 | `org.opencontainers.image.revision` defaults to `github.sha`, so a backfilled image would publish a **false source-commit claim** — and Phase 2's "built from the tag" criterion was uncheckable. Fixed with an explicit `labels:` override, which also makes that criterion verifiable. |

Round 2 was not required: every finding had a determinate fix and none invalidated the
architecture. Model note: the reviewer ran on **Opus**, not Fable, per the no-Fable instruction;
no phase here is a one-way door, so no tier-3 escalation was owed.

## Checkpoints

_(appended per phase by `/implement`)_

### Phase 1 — 2026-09-25 — DONE, verdict PASS (3 rounds), fresh-Opus verifier each round

**Files touched:** `.github/workflows/docker.yml` only (252 insertions / 11 deletions from
`main` as of commit `e8ce52b`, the last commit to touch this file in Phase 1 — `git diff
--numstat main..e8ce52b -- .github/workflows/docker.yml`. This number has already gone
stale twice in this section from edits made after it was written: 197/11 predates the
round-2/3 `/implement` fixes, and 226/11 predated this same `/ship`-gate round's own fixes
to this file. Pinned to a commit SHA rather than "the final revision" so it cannot go
stale a third time by definition — if `docker.yml` is touched again, cite the new commit,
don't edit this number in place). `ASSUMPTIONS.md` (+66
lines: the three plan-level assumptions from the plan's Open Questions, logged here since
they hadn't been written yet when Phase 0 closed) and the two new
`plans/docker-tag-trigger-184*.md` files from planning are also in the working tree but are
not Phase 1's own diff. No local `docs/`/`CLAUDE.md` touched (Phase 1's own Docs field:
none yet, Phase 4).

**Static checks:** `actionlint .github/workflows/docker.yml` clean (1.7.12, shellcheck
0.11.0 on PATH so shell linting ran too) after every round. `python3 -c "import yaml;
yaml.safe_load(...)"` clean.

**Resolve-script dry-run matrix, per plan Tests item (ii) — run against the script
*extracted from the shipped YAML* (`python3 -c "import yaml; ..."`, confirmed zero
unresolved `${{ }}`), not a hand-copied version, so this is testing what actually ships:**

| # | Case | `ref` | `version` | `is_release` | `push_latest` | Result |
|---|------|-------|-----------|---------------|----------------|--------|
| 1 | push to `main` (branch build) | `$SHA` | *(empty)* | `false` | `true` | rc 0, matches today's behavior |
| 2 | `workflow_call`, **real inherited context** (`EVENT_NAME=push`, `INPUT_VERSION` set, `INPUT_REF` empty) | `$SHA` | input | `true` | `false` | rc 0 — the load-bearing case; see Gap round 1 below |
| 3 | `workflow_dispatch` with `ref` (backfill) | input ref | derived/override | `true` | `false` | rc 0 |
| 3b | `workflow_dispatch` with `ref` + `version` override | input ref | override | `true` | `false` | rc 0 |
| 4 | `workflow_dispatch`, no `ref`, dispatched against `main` | `$SHA` | *(empty)* | `false` | `true` | rc 0 |
| 4b | `workflow_dispatch`, no `ref`, dispatched against a feature branch | `$SHA` | *(empty)* | `false` | **`false`** | rc 0 — Gap round 1 (G2) fix |
| 5 | push of a `macp-runtime-v*` tag (human/PAT backstop) | tag name | derived | `true` | `false` | rc 0 |
| N1 | `workflow_dispatch` `ref` not `macp-runtime-v*` | — | — | — | — | rc 1, `::error::` |
| N2 | `workflow_call`-context empty version | — | — | — | — | rc 0, `is_release=false`, `push_latest=true` (an *empty* `INPUT_VERSION` under the inherited `EVENT_NAME=push` context is indistinguishable from a plain push and resolves as one, silently, not as a release — this row originally recorded that as "verified deliberate, not a gap"; the `/ship` gate retracted that verdict as Gap 2 below, since it contradicts the header's "disjoint by construction" claim. The guard belongs in the caller, not here — see Gap 2 and Phase 3 sub-item (d)/AC7 in the plan) |
| N3 | `workflow_call`-context malformed version (`not-a-version`) | — | — | — | — | rc 1, `::error::` (semver regex) |
| N4 | `workflow_dispatch` `ref` with valid prefix, bad version suffix | — | — | — | — | rc 1, `::error::` |
| G4 | dispatched-caller release: `EVENT_NAME=workflow_dispatch`, `INPUT_REF=""`, `INPUT_VERSION` set (simulating a future caller with its own `workflow_dispatch` trigger) | — | — | — | — | rc 1, `::error::` — round 2/3 fix, `$GITHUB_OUTPUT` left empty |
| — | plain user error: `docker.yml` dispatched directly, `version` typed without `ref` | — | — | — | — | rc 1, identical to G4 — same guard correctly covers both, by design |

Cargo.toml cross-check step's shell logic dry-run separately against the real root
`Cargo.toml`: matches `0.8.1` → OK; a deliberately wrong resolved version (`9.9.9`) →
`::error::` + exit 1.

**Verification rounds (each a fresh Opus subagent, no Fable — no phase here is a
one-way door per the plan's Models section):**

- **Round 1 — GAPS.** Found:
  - **G1 (BLOCKER):** `github.event_name` is never literally `"workflow_call"` inside a
    called workflow — GitHub's own reusable-workflow reference states the called
    workflow's `github` context, `event_name` included, is "the same as the caller
    workflow." Since `release-plz.yml` (Phase 3's caller) is `push`-triggered, the plan's
    literal `case "$EVENT_NAME" in workflow_call)` arm is unreachable on the real
    platform — the release path would silently fall through to a branch build, emitting
    no semver tag and exiting green. **Independently re-confirmed by the executor** via
    `WebFetch` against `docs.github.com`'s `workflow_call` event reference table
    ("Webhook event payload: Same as the caller workflow") before accepting the finding.
  - G2: `push_latest`'s no-`ref`-dispatch case was missing the default-branch conjunct
    from the plan's own 1(b) note — a feature-branch dispatch would have moved `latest`.
  - G3: the original dry-run tested the `workflow_call` arm with a fabricated
    `EVENT_NAME=workflow_call`, which never occurs, so it validated unreachable code.
  - Fix: restructured the resolve step to detect `workflow_call` by the presence of its
    required `version` input (checked after ruling out `workflow_dispatch` by event_name),
    added the default-branch conjunct to `push_latest`, and re-ran the whole matrix against
    the script extracted from the shipped file. Also fixed 2 NITs (an uncited "per the
    action's own tests" claim; a present-tense doc-forward-reference to Phase 4 content
    that didn't exist yet).
- **Round 2 — GAPS.** Confirmed G1/G2/G3 closed (14 cases re-derived from the shipped
  script) and confirmed the fix wasn't "differently wrong" (a plain push to `main`
  correctly still resolves as a branch build; `inputs` genuinely doesn't populate for a
  bare `push` event). Found one **new latent gap**:
  - G4: the resolve step's own comment claimed `workflow_dispatch`'s `event_name` is
    always "genuine" — false by the identical inheritance rule the comment documents for
    `workflow_call` three lines above. It's only true today because `release-plz.yml` (the
    sole caller) has no `workflow_dispatch` trigger of its own. If one is ever added
    (plausible — `publish.yml` already has one), a dispatched caller run would silently
    degrade a release build to a green branch build, discarding the passed `version`.
  - NIT-2 (doc forward-reference) was judged only partially closed — softer wording, still
    read as present-tense.
  - Fix: added a hard-fail guard (`version` supplied without `ref` under
    `workflow_dispatch` → `::error::` + exit 1, covering both the inherited-caller case and
    the plain user-error case identically) and corrected the comment to state the real,
    narrower invariant. Reworded the NIT-2 comment to drop the forward reference entirely.
- **Round 3 — PASS.** Re-executed the actual shipped script (confirmed zero unresolved
  `${{ }}`) under `bash --noprofile --norc -eo pipefail` (the Actions default shell) via
  `shellcheck` as well as `actionlint`, both clean. G4 case now hard-fails with
  `$GITHUB_OUTPUT` left empty (no partial resolve can leak downstream); the live
  `workflow_call` path (row 2 above) is confirmed unaffected by the new guard, since the
  `workflow_dispatch` branch is entered only on the literal event name and `workflow_call`
  inherits `push`. No regression across the full matrix. NIT-2 confirmed fully closed (no
  `docs/`/`deployment` reference left anywhere in the file). Two non-blocking observations
  recorded (not gaps, not tied to any AC): the file header's one-line trigger summary
  doesn't enumerate the new hard-fail sub-case, and a comment describes `release-plz.yml`
  as "today's only caller" ahead of Phase 3 actually wiring that up — both accepted as
  correct at their own level of detail across all 3 rounds.

**No `ASSUMPTIONS.md` entries from Phase 1 itself** — G1/G2/G4 were factual/platform
corrections closed to a determinate fix, not ambiguous judgment calls with a chosen
alternative and a blast radius.

### `/ship` gate (separate from the `/implement` rounds above) — GAPS -> PASS

The `/implement`-level gate above checks phase-vs-spec compliance; `/ship`'s own gate is a
separate pass checking the diff as a shippable unit (doc drift, tracked-file consistency,
enterprise-readiness) — run fresh against commit `fdd82de`, after Phase 1 was already
marked `DONE` above. It returned **GAPS**, 4 items, none ship-blocking on their own (`git
diff --numstat` confirms `docker.yml` is internally complete and inert until Phase 3
exists — not half-migrated), but all closed before opening the PR rather than carried
forward:

1. **`docker.yml`'s no-`ref` `workflow_dispatch` branch didn't reject a dispatch made
   directly against a tag ref.** Not in the 13-case matrix (which only covered
   `REF_TYPE=branch`, on `main` and on a feature branch). Verified live: dispatching with
   no `ref` input against `REF_TYPE=tag` built that tag's tree as an ordinary
   non-release build — no semver tag, a bare SHA tag pushed anyway, exit green. This is
   exactly the wrong command Phase 2's own plan text warns against
   (`--ref macp-runtime-vX.Y.Z` instead of `--ref main -f ref=...`), and every tag cut
   after this merges will carry the `workflow_dispatch` trigger (unlike today's
   `macp-runtime-v0.8.1`, which has neither trigger — verified during planning). Fixed:
   the no-`ref` branch now hard-fails unless `REF_TYPE = branch`.
2. **An empty `version` on the live `workflow_call` path silently falls through to a
   branch build** rather than failing loudly, contradicting the file's own header claim
   that the two build kinds are disjoint "by construction." The 13-case matrix's N2 row
   examined this and recorded "verified deliberate, not a gap" — the `/ship` gate
   disagreed, correctly: `docker.yml` cannot distinguish this case from a genuine push
   (identical inherited `event_name`/`ref`/`sha` under `workflow_call`), so the guard
   cannot live here and must live in the *caller* instead. Closed by: softening the
   header's "by construction" claim to "given a well-formed trigger" and stating the
   caller obligation explicitly, adding a matching comment at the `workflow_call`
   detection branch, and — the real fix — adding a new Phase 3 sub-item (d) and
   acceptance criterion 7 to `plans/docker-tag-trigger-184.md` requiring `release-plz.yml`
   to validate the extracted version is non-empty *before* invoking `docker.yml`, and
   correcting Phase 3's edge-case bullet that had wrongly assumed Phase 1(c)'s semver
   guard would already catch this (it does not, under the corrected discriminator: an
   empty `INPUT_VERSION` fails the `-n` test and is never even classified as a release
   build).
3. **`${{ steps.resolve.outputs.version }}` was interpolated directly into two `run:`
   shell lines** (the Cargo.toml cross-check step) rather than passed through `env:`, in a
   job holding `packages: write` — an inconsistency with the resolve step's own scrupulous
   `env:`-only pattern. The `/ship` gate demonstrated a crafted value (embedded quote +
   command + comment) reaching the unquoted-delimiter output line. Severity judged low —
   every input path already requires repo write access, so this is a hardening/consistency
   defect, not privilege escalation. Fixed: moved to `env: RESOLVED_VERSION:` and compared
   `"$RESOLVED_VERSION"`, matching the resolve step's pattern.
4. **This file's own diff-stat claim was stale** (197/11 vs. actual 226/11, after the
   round-2/3 `/implement` fixes were applied but before this file's number was updated).
   Corrected above.

The gate also flagged, as a forward note rather than a gap: Phase 3 must **not** copy
`publish.yml`'s `secrets: inherit` literally when calling `docker.yml` — `docker.yml`
declares no `secrets:` block on `workflow_call` and needs only the automatically-granted
`secrets.GITHUB_TOKEN`, so `secrets: inherit` would needlessly hand a Docker-build job
every repo secret including `CARGO_REGISTRY_TOKEN`. Phase 1 already gets this right (no
`secrets:` block); recorded here so Phase 3 doesn't regress it.

Endorsed without change: the two-PR strategy itself (the dependency is real and
physical — tags are immutable, so Phase 2 cannot exist before Phase 1 is on `main`);
zero doc drift (no tracked `.md` anywhere mentions `ghcr.io`/`docker.yml`/an image tag
scheme, and `CLAUDE.md` is confirmed gitignored at `.gitignore:20`); all `ASSUMPTIONS.md`
entries correctly scoped to Phase 2/3, none blocking Phase 1; tracked-file consistency
(Phase 1 `DONE`, Phases 2-5 `TODO`, in both files, in agreement).

Re-validated after the fixes: `actionlint` clean (both `docker.yml` alone and the full
`.github/workflows/` sweep), `yaml.safe_load` clean, full resolve-script matrix re-run
including the new tag-ref-dispatch and cross-check-quoting cases.

**What's next:** commit Phase 1 (including the `/ship`-gate fixes above and the Phase 3
plan update), open PR A (Phase 1 only, per the PR strategy above), watch CI, merge. Then
Phase 2 (manual backfill dispatch against `main`) can run.

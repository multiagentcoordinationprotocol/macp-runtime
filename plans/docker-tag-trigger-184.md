# Plan — `docker.yml` publishes a semver-tagged image on every release (issue #184)

> Revision 2. Written against the real workflow files, the real GHCR tag list, and the pinned
> sources of both third-party actions involved. A fresh-Opus reverification pass tested every
> claim in revision 1 and returned **FLAWED** — 2 blockers, 7 should-fixes, 6 nits; all are
> applied below and the evidence is retained inline so `/implement` does not re-derive it. The
> findings table is in `## Plan review` at the bottom.

## Context

`.github/workflows/docker.yml:6-9` triggers on:

```yaml
on:
  push:
    branches: [main]
    tags: ["v*"]
```

release-plz creates per-crate tags named `<package>-v<version>`, so the umbrella tag is
`macp-runtime-v0.8.1`. GitHub's filter patterns are **anchored prefix matches** — `*` "matches
zero or more characters, but does not match the `/` character", and the match starts at the
beginning of the ref name — so `v*` does not match `macp-runtime-v0.8.1`. The tag half of this
trigger has been dead since the tagging scheme changed.

**The break is dated and visible in the registry.** `v0.5.0` (2026-07-06) was the last bare tag;
commit `b908458` (2026-07-10, "ci: split release into release-plz (version+git) and publish.yml
(crates.io)") moved releases onto `macp-runtime-v*`; `macp-runtime-v0.6.1` (2026-07-11) was the
first tag under the new scheme. Probing `ghcr.io/multiagentcoordinationprotocol/macp-runtime`
anonymously returns **109 tags**, of which the only semver-shaped ones are:

```
0.4  0.4.1  0.5  0.5.0
```

plus `latest`, `main`, and ~100 commit-SHA tags. Nothing for 0.6.x, 0.7.x or 0.8.x. The 0.4/0.5
entries are the fossil record of the trigger working, under the old scheme, before `b908458`.
`docker.yml` is also the **only** workflow in the repo still matching `v*` (surveyed all ten),
which answers the issue's "worth checking before deciding" aside: nothing else tags that way.

**Current version is `0.8.1`, not the `0.8.0` the issue names.** Root `Cargo.toml:23-24` reads
`[workspace.package] version = "0.8.1"`, and `macp-runtime-v0.8.1` was released 2026-09-22
(`gh release list`). 0.8.0 shipped 2026-09-20 and also has no image. Acceptance criterion 2
therefore means **`:0.8.1` and `:0.8`**, and the plan says so wherever the issue said 0.8.0.

### Why the issue's suggested fix does not work — two independent reasons

The issue proposes changing the glob to `tags: ["macp-runtime-v*"]`. That is a correct edit and
it is **not sufficient**. Both failures below were verified against pinned sources, not inferred.

**Reason 1 — the corrected trigger is inert for release-plz's own tags.** This repo has already
hit this bug once and fixed it, for `publish.yml`. `publish.yml:34-35` *already* uses
`tags: ["macp-runtime-v*"]`, and its own header (`publish.yml:15-23`) records why that is a
backstop rather than the live path:

> the `macp-runtime-v*` tag push. Kept as a backstop, but note it does NOT fire for tags
> release-plz creates: GitHub does not start workflow runs from events made with the default
> GITHUB_TOKEN (the recursion guard), so this trigger sat inert and 0.6.1 was tagged 2026-07-12
> and never reached crates.io. It still fires for a tag pushed by a human or a PAT.

`release-plz.yml:15-26` and `release-plz.toml:10-14` state the same cause independently. The
live path for `publish.yml` is `workflow_call` from `release-plz.yml:69-74` (`needs: release-plz`,
`if: releases_created == 'true'`, `secrets: inherit`). **`docker.yml` needs the same
architecture.** Shipping only the glob fix would produce a workflow that looks fixed, stays
silent for the same reason, and re-teaches the 0.6.1 lesson at the cost of another release cycle.

**Reason 2 — `type=semver` cannot parse this repo's tag names even when it does fire.**
`docker.yml:49-50` uses `type=semver,pattern={{version}}`. Read at the pinned SHA
`dc802804100637a589fabce1cb79ff13a1411302` (= `docker/metadata-action` v6.2.0), `procSemver`
(`src/meta.ts:155-199`) does **no** prefix-stripping. Its only transformations before validation
are an opt-in `match` regex and `/`→`-`; then:

```ts
    if (!semver.valid(vraw)) {
      core.warning(`${vraw} is not a valid semver. More info: https://semver.org/`);
      return version;
    }
```

`semver.valid("macp-runtime-v0.8.1")` is null — node-semver tolerates a `v` immediately before
the digits, not a `macp-runtime-v` prefix. So a human- or PAT-pushed tag would run the workflow,
log one warning, emit **no semver tag**, and exit green. A silent no-op that passes CI is exactly
how the original bug survived seven releases; the fix must not install a second one.

The supported escape hatches, both present at this pin and both covered by the action's own tests
on a **branch** ref (`__tests__/meta.test.ts` cases `push14`, `push35`, `tag34`):

- `match=` — a regex whose **capture group 1** becomes the version (`vraw = tmatch[1]`).
- `value=` — an explicit version string that **bypasses the ref check entirely**. The guard at
  `meta.ts:156` is a conjunction: `if (!/^refs\/tags\//.test(ref) && value.length == 0) return`.
  A non-empty `value` therefore resolves correctly even when the ref is `refs/heads/main` — which
  is precisely the `workflow_call` situation.

### The `workflow_call` context wrinkle

A reusable workflow invoked via `workflow_call` runs inside the **caller's** event — "the `github`
context is always associated with the caller workflow", and the event reference gives both
`GITHUB_SHA` and `GITHUB_REF` as "Same as the caller workflow". Since `release-plz.yml` is
triggered by `push: branches: [main]`, a called `docker.yml` sees `github.ref == refs/heads/main`,
`github.ref_type == branch` and `github.event_name == push` — **not** the tag release-plz created
moments earlier in the same run.
(This is the flip side of the property `release-plz.yml:25-26` relies on: "`workflow_call` runs
inside this same run, so the recursion guard never applies.") Consequences, all verified in
`meta.ts`:

- every `type=semver` rule **silently skips** (`return version` before any warning);
- `type=sha,prefix=` resolves `github.sha`, which is the caller's commit — correct for a release
  call, **wrong** for a backfill dispatch of an older tag;
- `{{is_default_branch}}` is **true**, so the existing `latest` rule (`docker.yml:46`) would fire.

### The `latest=auto` trap

`docker.yml` sets no `flavor:` input, so `metadata-action` defaults to `latest=auto`. Under that
flavor, **any** resolved non-prerelease semver rule sets `latest` (`meta.ts:195`) — including one
driven by `value=` on a branch ref (test `push14` asserts exactly this). Today this is dormant
because no semver rule ever resolves. The moment one does, a backfill build of an old tag would
**move `latest` backwards** to that older tree. That is a criterion-3 regression concealed inside
the fix, and it is the single most dangerous thing in this change.

### Chosen approach

Make `docker.yml` able to perform an explicit **release build** — a build of a named ref tagged
from an explicit version — and reach it three ways: `workflow_call` (the live path, from
`release-plz.yml`), `workflow_dispatch` (recovery + the retroactive backfill), and the corrected
`macp-runtime-v*` tag push (the documented-inert backstop, mirroring `publish.yml`).

The two build kinds emit **disjoint** tag sets:

| Build kind | Trigger | Tags emitted |
|---|---|---|
| branch build | `push: branches: [main]`, or a dispatch with no `ref` | `latest`, `main`, `<sha>` — **exactly today's behavior** |
| release build | `workflow_call`, a dispatch with `ref`, or a `macp-runtime-v*` tag push | `<version>`, `<major>.<minor>` — and nothing else |

Disjointness is load-bearing. It means the release build cannot touch `latest`, cannot re-point a
SHA tag at the wrong tree, and cannot race the branch build that runs concurrently for the same
commit when a release PR merges. It also makes criterion 3 provable by inspection rather than by
argument: no rule that exists today changes its output on any trigger that exists today.

Version resolution is explicit (`value=`) rather than ref-derived, and is **cross-checked against
the checked-out `[workspace.package].version`** so a wrong ref or a wrong input fails loudly
instead of mis-tagging.

### What was ruled out, and why

1. **Glob fix alone (the issue's suggestion).** Inert for release-plz's tags, and emits no semver
   tag even when it does fire. Both reasons above. Kept as a *backstop*, never as the mechanism.
2. **`context: git` + checkout the tag.** Works — verified: `actions-toolkit@v0.92.0`'s
   `Git.ref()` handles a detached-HEAD tag checkout via `git show -s --pretty=%D` and
   `for-each-ref --points-at HEAD`, returning a real `refs/tags/...`. Rejected anyway: it
   overrides `ref` **and** `sha` for *every* rule at once (`src/context.ts:85-92`), so it silently
   changes the meaning of `type=sha`, `type=ref` and `{{is_default_branch}}` as a side effect of
   fixing `type=semver`; it only works when checked out **by tag name** (a SHA checkout resolves
   back to `refs/heads/main` via `inferRefFromHead`, re-breaking semver); it can throw hard errors
   (`Cannot find detached HEAD ref`); and under `workflow_call` you must pass the tag name in
   anyway, at which point `value=` is strictly simpler. `value=` overrides exactly the one rule
   that needs overriding.
3. **`match=` instead of `value=`.** Needed only on paths where the version must be recovered from
   the tag *name*. Since the release build already carries an explicit version, `match=` would be
   a second, redundant parser — and a quoting hazard (the `tags` input is CSV-parsed, so a regex
   containing `,` must be quoted). Not used.
4. **A `repository_dispatch` / `workflow_run` bridge from the release.** Note the tempting-but-wrong
   reason first: `workflow_run` *would* fire here. `release-plz.yml`'s own trigger is the human
   merge push to `main` (`release-plz.yml:3-5`), not a `GITHUB_TOKEN` event, so the recursion
   guard does not apply to it. The real reasons are that `workflow_run` cannot read the triggering
   workflow's **job outputs** (so the version would have to be smuggled through an artifact), and
   that it always runs the **default branch's** copy of the workflow. `repository_dispatch` needs
   a PAT. `workflow_call` is the pattern this repo already proved, and it passes inputs directly.
5. **Re-tagging or force-moving `macp-runtime-v0.8.1`** to make a tag-push event fire for the
   backfill. Rewrites published release history to work around a CI gap. Never.
6. **Reading the version from `releases` via a `jq` step inside the `release-plz` job.** See
   Phase 3(b) — this is the 0.6.1 hazard in miniature and is rejected on structural grounds.

---

## Phases

### Phase 1 — Teach `docker.yml` to do a release build

- **Status:** DONE (2026-09-25)

  **Divergence from the written spec, found only during implementation-time verification —
  not anticipated by the plan or by its own reverification round.** The exhaustive
  per-trigger table in (b) specifies discriminating on `github.event_name`, including a
  literal `case "$EVENT_NAME" in workflow_call)` arm. **That arm is dead code on the real
  GitHub Actions platform**: per GitHub's own reusable-workflow reference, a called
  workflow's `github` context — `event_name` included — is "the same as the caller
  workflow", never the literal string `"workflow_call"`. Since `release-plz.yml` (Phase
  3's caller) is itself `push`-triggered, `EVENT_NAME` reads `"push"` on the live release
  path too, so the plan's literal case-statement would have silently fallen through to the
  branch-build arm on every real release — pushing `latest`/`main`/`<sha>` with **no**
  semver tag, exit green. This is precisely the failure AC6 and edge case "Malformed or
  absent version on a release build → hard failure ... never a green no-op" forbid, and it
  would have reproduced issue #184 itself through the very fix meant to close it.

  **Fix, found and closed over 3 verification rounds (see `PROGRESS.md`'s Phase 1
  checkpoint for the full gap history):** `workflow_call` is now detected by the presence
  of its own required `version` input rather than by `event_name`, checked only after
  `workflow_dispatch` is ruled out by its own (genuinely reliable, since it can only be a
  top-level trigger, never an inherited one — *while every caller stays push-only*, see
  below) `event_name`. Two further findings surfaced by the same scrutiny, both closed:
  `push_latest` on a no-`ref` dispatch needed a default-branch conjunct (a feature-branch
  dispatch must not move `latest`); and a *future* caller of `docker.yml` gaining its own
  `workflow_dispatch` trigger would inherit `EVENT_NAME=workflow_dispatch` with no `ref`
  input, indistinguishable from a genuine no-ref dispatch — now a hard failure
  (`version` supplied without `ref`) rather than a silent degrade, and the resolve step's
  comment states the narrower true invariant instead of the plan's (and round 1's) implicit
  assumption that `workflow_dispatch`'s `event_name` is unconditionally trustworthy.

  The per-trigger *outcomes* in (b)'s table (ref/version/is_release/push_latest for each of
  the 5 rows) are all still exactly as specified and verified correct; only the
  *discriminator* used to reach them changed. Approach items (a), (c)-(i) are implemented
  as written, with no other divergence.
- **Delivers:** a `docker.yml` that can build and push a semver-tagged image for an explicit ref,
  reachable by `workflow_call`, `workflow_dispatch`, or a `macp-runtime-v*` tag push — with the
  existing branch-push behavior byte-for-byte unchanged.
- **Depends on:** nothing. Lands alone and is inert until Phase 2 or 3 invokes it.
- **Files:** `.github/workflows/docker.yml`
- **Approach:**

  **(a) Triggers.** Keep `push: branches: [main]` exactly as-is (this is what produces `latest`
  and the SHA tags today, and criterion 3 is about not disturbing it). Correct the glob to
  `tags: ["macp-runtime-v*"]` **and carry a comment modelled on `publish.yml:15-23`** saying in
  the same words that this trigger does not fire for release-plz's own tags and is retained only
  for a human- or PAT-pushed tag. A corrected-but-undocumented glob is how the next reader
  concludes the tag path is live. Add:

  ```yaml
    workflow_call:
      inputs:
        version:
          description: "Released workspace version, e.g. 0.8.1 (no leading v)"
          type: string
          required: true
    workflow_dispatch:
      inputs:
        ref:
          description: "Git ref to build, e.g. macp-runtime-v0.8.1 (blank = build the default branch)"
          type: string
          required: false
          default: ""
        version:
          description: "Override the version tag (blank = derive from ref)"
          type: string
          required: false
          default: ""
  ```

  `workflow_call` takes **only** `version`: under a call from `release-plz.yml`, `github.sha` is
  already the released commit (release-plz tags the head of `main` it just released), so the
  default checkout is correct and a `ref` input would be a second source of truth for the same
  thing. `workflow_dispatch` takes `ref` because the backfill must name an *older* tag.

  **(b) A `resolve` step, before checkout**, emitting four outputs. House style per
  `spec-drift.yml:58-71` (`set -euo pipefail`, `::error::`, `>> "$GITHUB_OUTPUT"`).

  **Specify `ref` per trigger, never per "build kind".** An earlier draft of this plan said "the
  tag name for a release build, else `github.sha`", which is unimplementable: the `workflow_call`
  path *is* a release build but has no tag name and no `ref` input. A literal executor would emit
  empty or `main`, build **the tip of `main`**, and tag it `:0.8.1`. The exhaustive table:

  | Trigger | `ref` | `version` | `is_release` | `push_latest` |
  |---|---|---|---|---|
  | `push` to `main` | `${{ github.sha }}` | *(empty)* | `false` | `true` |
  | `workflow_call` (from release-plz) | `${{ github.sha }}` — the released commit | `inputs.version` | `true` | `false` |
  | `workflow_dispatch` with `ref` | `inputs.ref` | `inputs.version` if given, else `ref` minus `macp-runtime-v` | `true` | `false` |
  | `workflow_dispatch`, no `ref` | `${{ github.sha }}` | *(empty)* | `false` | `true` |
  | `push` of a `macp-runtime-v*` tag | `${{ github.ref_name }}` | tag name minus `macp-runtime-v` | `true` | `false` |

  Notes that the table encodes and an executor must not re-derive:

  - `ref` is **always concrete**, so `actions/checkout` never receives an empty string and this
    plan never rests on checkout's empty-ref semantics.
  - `github.sha` is the right checkout target for `workflow_call` because release-plz tags the
    release-PR merge commit on `main`, which *is* the caller's `github.sha`. Verified:
    `macp-runtime-v0.8.1` → `6128ccc` ("chore: release v0.8.1 (#180)"), an ancestor of `main`.
  - **The `ref` input must name a `macp-runtime-v*` tag; reject anything else.** Without this
    guard a dispatch with `ref: some-branch` yields `is_release=false` *and* `push_latest=true`,
    so it would push `latest`, `main` and a SHA tag built from the wrong tree. This was found by
    dry-running the resolve logic during planning, not by inspection. One-line guard, hard failure.
  - **`is_release` and `push_latest` must be the literal lowercase strings `true`/`false`, never
    empty.** `metadata-action` throws `Invalid value for enable attribute:` on anything else
    (`meta.ts:54-57`) — which makes a malformed output a loud failure rather than a silent
    mis-tag, so this is a free guard rather than only a hazard.
  - `push_latest`'s default-branch test is `github.ref == format('refs/heads/{0}',
    github.event.repository.default_branch)` (or simply `refs/heads/main`), **and** `inputs.ref`
    empty.

  **(c) Validate, loudly.** If `is_release` is true, the resolved version must match
  `^[0-9]+\.[0-9]+\.[0-9]+([-+].*)?$`; otherwise `::error::` and exit 1. This converts
  `metadata-action`'s silent skip (Reason 2) into a hard failure — the whole point of the phase is
  that a release build which produces no semver tag must never be green.

  **(d) Checkout `ref: ${{ steps.resolve.outputs.ref }}`.** Required: under `workflow_call` and
  under a backfill dispatch the default checkout would otherwise be `main`, and
  `docker/build-push-action`'s `context: .` builds the checked-out tree.

  **(e) Cross-check the version against the tree.** After checkout, when `is_release` is true,
  parse `[workspace.package].version` from the root `Cargo.toml` and fail unless it equals the
  resolved version. This makes the cheap version source in Phase 3(b) sound and catches a typo'd
  dispatch input or a tag that does not point where it is assumed to.

  **Be precise about what this check does NOT catch.** It does *not* detect "checked out `main`
  instead of the release tag", because `main` keeps `version = "0.8.1"` for every commit after the
  release — there are three such commits right now. So the cross-check would pass while building
  the wrong tree. That hole is closed by the checkout table in (b) (which removes the ambiguity)
  and observed by the revision label in (f). Do not present (e) as the guard against a wrong
  checkout; it is a guard against a wrong *version*.

  **(f) Pin `org.opencontainers.image.revision` to the commit actually built.** By default
  `metadata-action` sets that label from `context.sha` (`meta.ts:548`) — `github.sha`. On the
  `workflow_call` and tag-push paths that is already correct. On a **backfill dispatch** it is
  `main`'s HEAD, so the published `:0.8.1` image would carry a label asserting it was built from a
  commit it was not built from. Publishing a false provenance claim is not acceptable in a repo
  that deliberately turns on `provenance: true` and `sbom: true` (`docker.yml:61-62`).

  So capture the resolved commit after checkout (`git rev-parse HEAD`) and pass an explicit
  override to the metadata step:

  ```yaml
    labels: |
      org.opencontainers.image.revision=${{ steps.resolve_sha.outputs.sha }}
  ```

  Extra `labels` override the defaults — they are merged last-write-wins into a `Map`
  (`meta.ts:551-561`). This also makes Phase 2's "built from the tag, not from `main`" acceptance
  criterion *checkable*, which it otherwise is not: the two trees produce different digests but
  nothing externally distinguishes them, and `macp-runtime --version` reports `0.8.1` either way.

  **Residual, stated rather than hidden:** the SLSA **provenance attestation** is generated by
  buildkit from the runner environment and records `GITHUB_SHA`; it cannot be overridden this way.
  On the live release path that is correct (`github.sha` *is* the released commit). On a one-off
  backfill the attestation will reference the dispatch's `main` SHA. Accepted, confined to
  manually backfilled images, and documented in Phase 4.

  **(g) `flavor: latest=false`** on the metadata step. Without it, `latest=auto` re-adds `latest`
  whenever a semver rule resolves (the trap above): `meta.ts:195` sets `latest=true` and `:198`
  honors it under `auto`. The deliberate `latest` remains the explicit `type=raw` rule in (h).

  Two facts settled during review, so neither needs re-deriving at implementation time:

  - **`latest=false` does not suppress an explicit `type=raw,value=latest`.** `procRaw`
    (`meta.ts:338-341`) routes the value into `version.main`/`partial`, emitted at
    `generateTags:521-523`; `flavor.latest` governs only `version.latest`, which drives a
    *separate* auto-latest push at `:525-527`. Acceptance criterion 5 will pass. It is retained as
    a check, not as an open question.
  - **The flavor change is a no-op for branch builds.** On a `main` push today, `procRefBranch`
    (priority 600) is the first rule to call `setVersion` — both semver rules skip — and it
    already sets `version.latest = false`. So `latest` already comes solely from the raw rule.
    This is the cleanest possible evidence for criterion 3.

  **(h) Gate every tag rule** so the two build kinds are disjoint:

  ```yaml
  tags: |
    type=raw,value=latest,enable=${{ steps.resolve.outputs.push_latest }}
    type=ref,event=branch,enable=${{ steps.resolve.outputs.is_release != 'true' }}
    type=ref,event=pr,enable=${{ steps.resolve.outputs.is_release != 'true' }}
    type=sha,prefix=,enable=${{ steps.resolve.outputs.is_release != 'true' }}
    type=semver,pattern={{version}},value=${{ steps.resolve.outputs.version }},enable=${{ steps.resolve.outputs.is_release }}
    type=semver,pattern={{major}}.{{minor}},value=${{ steps.resolve.outputs.version }},enable=${{ steps.resolve.outputs.is_release }}
  ```

  `push_latest` replaces the current `{{is_default_branch}}` handlebars guard because the new
  condition is a conjunction (default branch **and** not a release build **and** no `ref` input)
  and mixing a handlebars expression with a `${{ }}` expression inside one attribute is not worth
  the ambiguity. On a plain `main` push the two are equivalent by construction.

  `enable=` is a **global** tag attribute, honored for every type used here: `tag.ts:206-208` sets
  it generically after the per-type switch, and `meta.ts:53-60` evaluates it uniformly *before*
  dispatching on type. No type-specific caveat applies.

  **(i) `timeout-minutes: 90` on the job.** See Phase 3's concurrency note — once this workflow
  runs inside the release run, a hung build sits on the release-serialising concurrency group for
  the 360-minute default. This is the same reasoning, and the same fix, as
  `release-plz.yml:95-99`. Observed maximum build time is 51 minutes.

  **Do not add a `{{major}}` rule.** `metadata-action`'s README is explicit that `0` should not be
  generated for `0.y.z`, and the repo is on `0.8.x`. The existing two rules are already correct.

  `type=ref,event=pr` is dead code — PRs never reach this workflow (`ci.yml:706-723` is the
  build-only gate). It is left in place and merely gated; removing it is unrelated cleanup.

- **Edge cases & failure modes:**
  - *Release PR merges* → **two** runs for one commit: the `push: main` run (branch build →
    `latest`/`main`/sha) and, after `release-plz` cuts the release, the `workflow_call` run
    (release build → semver only). Disjoint tag sets, so no race and no clobber, and they need no
    `concurrency` group of their own. **They run concurrently and both run cold** — the release-plz
    job takes ~2m39s (measured on the v0.8.1 run), so the second build starts while the first is
    ~3 minutes into a ~45-minute build and has exported nothing to the `type=gha` cache. Do not
    expect the second build to be a cache hit; it is a full additional build.
  - *Two digests for one source commit.* The branch build and the release build produce different
    images for the same commit, because `org.opencontainers.image.created` differs
    (`meta.ts:547`). So `:0.8.2` and `:<sha>` will not share a digest even though they are the
    same source tree. Harmless, but Phase 4 must say so rather than let readers assume otherwise.
  - *Backfill dispatch of an old tag* → `push_latest` is false, so `latest` is not moved; sha and
    branch rules are off, so no SHA tag is re-pointed. This is the case the gating exists for.
  - *Dispatch with no `ref`* → a branch build; a manual "refresh `latest` from `main`" affordance.
  - *A human or PAT pushes `macp-runtime-v1.2.3`* → release build via the backstop glob; version
    derived from the tag name and cross-checked against that tag's `Cargo.toml`.
  - *A `v*`-shaped tag pushed by a human* → **no longer builds.** Deliberate: nothing in the repo
    creates those any more, and matching both globs would resurrect a scheme `b908458` retired.
  - *Malformed or absent version on a release build* → hard failure at (c) or (e), never a green
    no-op.
- **Acceptance criteria:**
  1. `on:` has `push` (branches `[main]`, tags `["macp-runtime-v*"]`), `workflow_call` and
     `workflow_dispatch`; the `v*` glob is gone and the `macp-runtime-v*` one carries a comment
     stating it is inert for release-plz's own tags, citing the same GITHUB_TOKEN cause as
     `publish.yml:15-23`.
  2. *(post-merge observation)* A branch build (push to `main`) emits exactly `latest`, `main`,
     `<sha>` — the same set as before this change, read off the run's metadata-step output.
  3. A release build emits exactly `<version>` and `<major>.<minor>`, and **no** `latest`, branch
     or SHA tag. Checkable pre-merge from the Phase 2 dispatch.
  4. The metadata step sets `flavor: latest=false`.
  5. *(post-merge observation)* `latest` still appears on a `main` push after the flavor change.
  6. A release build whose resolved version is empty or not semver-shaped **fails the job**; a
     release build whose version disagrees with the checked-out `[workspace.package].version`
     **fails the job**; a dispatch whose `ref` is not a `macp-runtime-v*` tag **fails the job**.
  7. `actions/checkout` receives a non-empty `ref` on every path, per the table in (b).
  8. The job sets `timeout-minutes`.
  9. The metadata step passes an explicit `org.opencontainers.image.revision` label equal to the
     commit actually checked out.
  10. `actionlint .github/workflows/docker.yml` is clean.

  **ACs 2 and 5 are not checkable before merge.** `docker.yml` has no `pull_request` trigger, and
  a bare dispatch against a feature branch yields `push_latest=false`, so the `latest` path cannot
  be exercised pre-merge — and a feature-branch dispatch would additionally push a stray
  `<branch>` tag to GHCR, so do not do it casually. Assign these two to the merging human as
  post-merge observations and record them in `PROGRESS.md`.
- **Tests:** No Rust test covers a workflow. Prove by (i) `actionlint`; (ii) a local dry-run of
  the resolve script over every row of the (b) table plus the negative cases, asserting the four
  outputs, recorded in `PROGRESS.md`; (iii) the live evidence from Phase 2. A guard only ever seen
  passing is not a guard — also exercise the three failure paths in (6).

  Planning already did (i) and (ii) against a scratch copy: `actionlint` clean, and the resolve
  logic exercised over nine cases (five triggers + four negatives). That run is what surfaced the
  `ref`-input guard. Re-run both against the real file; do not treat the scratch result as
  sufficient.
- **Docs:** none yet (Phase 4).

---

### Phase 2 — Backfill the image for the current release

- **Status:** TODO — **this phase contains a manual action that `/implement` (or a human) must
  actually run. It cannot be completed by a code change.** The 0.8.1 release is already cut; no
  edit to a workflow file retroactively fires an event for it.
- **Delivers:** acceptance criterion 2 — `ghcr.io/multiagentcoordinationprotocol/macp-runtime:0.8.1`
  and `:0.8` exist.
- **Depends on:** **Phase 1 only.** Deliberately ordered *before* Phase 3: it exercises the entire
  release-build path under a manual trigger, where a failure costs nothing, **before** that path
  becomes part of the release pipeline.
- **Files:** none.
- **Approach:** run

  ```bash
  gh workflow run docker.yml --ref main -f ref=macp-runtime-v0.8.1
  ```

  **Dispatch against `main`, passing the tag as an input — NOT `--ref macp-runtime-v0.8.1`.**
  This is the single most important detail in the phase, and the obvious command is the wrong one.

  A `workflow_dispatch` is validated and executed against **the copy of the workflow file at the
  dispatched ref** (`gh workflow run`'s own manual: `--ref` is the "Branch or tag name which
  contains the version of the workflow file you'd like to run"; the event reference gives
  `GITHUB_REF` = "Branch or tag that received dispatch"). Two conditions must both hold: the file
  must exist on the default branch for the workflow to be dispatchable at all, **and** the copy at
  the target ref must itself declare `workflow_dispatch`. When it does not, the API returns
  `HTTP 422: Workflow does not have 'workflow_dispatch' trigger` — the message says "in this
  branch", i.e. the target ref.

  Verified directly, not assumed:

  ```
  $ git show macp-runtime-v0.8.1:.github/workflows/docker.yml | head -9
  on:
    push:
      branches: [main]
      tags: ["v*"]
  $ git show macp-runtime-v0.8.1:.github/workflows/docker.yml | grep -c 'workflow_dispatch\|workflow_call'
  0
  ```

  The tag's copy has neither trigger, and a tag is immutable. So `--ref macp-runtime-v0.8.1`
  **cannot work** and adding the trigger on `main` does not change that. Dispatching against
  `main` runs the **new** file (with Phase 1's fixes) and checks out the tag's tree via the `ref`
  input. This is the whole reason Phase 1 gives `workflow_dispatch` a `ref` input, and it is why
  the "no inputs needed for dispatch, just pick the ref in the UI" assumption does not hold here.

  Then verify from the registry rather than from the workflow's exit code:

  ```bash
  TOKEN=$(curl -s "https://ghcr.io/token?scope=repository:multiagentcoordinationprotocol/macp-runtime:pull&service=ghcr.io" | jq -r .token)
  curl -s -H "Authorization: Bearer $TOKEN" \
    "https://ghcr.io/v2/multiagentcoordinationprotocol/macp-runtime/tags/list?n=1000" | jq -r '.tags[]' | sort
  ```

  Record the before/after tag lists in `PROGRESS.md`.
- **Edge cases & failure modes:**
  - **Backfill order matters for the moving `0.8` tag.** `{{major}}.{{minor}}` is mutable. If
    0.8.0 is ever backfilled too, it must be built **before** 0.8.1 or it will drag `0.8` back to
    the older patch. The plan backfills **0.8.1 only**, which is what criterion 2 asks for; if
    someone later wants 0.8.0 as well, build it first and then rebuild 0.8.1.
  - *`latest` must not move.* `main` is ahead of `macp-runtime-v0.8.1` by several commits. Phase
    1(g)'s `push_latest` gate is what prevents this; confirm by digest that `latest` is unchanged
    across the backfill.
  - *Building an old tree fails* (toolchain drift in the `rust:1.98-bookworm` base, a yanked
    crate). Then the backfill is not achievable as specified and the finding is reported rather
    than worked around — do **not** patch the old tree or re-tag.
  - *Multi-arch build time.* Recent `docker.yml` runs take 35-50 minutes (arm64 via QEMU); the
    cache will not help an old tree much. Budget an hour.
- **Acceptance criteria:**
  1. `:0.8.1` and `:0.8` are present in the GHCR tag list, and both resolve to the same digest.
  2. That image's `org.opencontainers.image.revision` label equals
     `git rev-parse macp-runtime-v0.8.1` (= `6128ccc…`). This is what proves it was built from the
     tag rather than from `main`, and it is only checkable because of Phase 1(f) — the default
     label would report `main`'s HEAD, and the digest alone distinguishes nothing (both trees
     report `0.8.1`).
  3. `latest` points at the same digest as the one **captured immediately before dispatching**.
     If it moved, reconcile against `gh run list --workflow=docker.yml` before calling it a
     failure: a legitimate push to `main` during the ~45-minute build moves `latest` too (three
     main pushes landed within 17 minutes on 2026-09-22).
  4. No SHA tag was added or re-pointed by the backfill run.
  5. `PROGRESS.md` records the dispatch command, the run URL, the before/after tag lists, and the
     `latest` digest captured before dispatch.
- **Tests:** the registry probe above is the test.
- **Docs:** none (Phase 4).

---

### Phase 3 — Call `docker.yml` from the release

- **Status:** TODO
- **Delivers:** acceptance criterion 1 — cutting a release publishes a correspondingly-tagged
  image, automatically, on the live path rather than the inert one.
- **Depends on:** Phase 1 (the `workflow_call` entry point must exist, or the `uses:` is a
  configuration error). Should follow Phase 2, which proves the path works.
- **Files:** `.github/workflows/release-plz.yml`
- **Approach:**

  **(a) A new `docker` job, sibling to `publish`** (`release-plz.yml:69-74`), same gate:

  ```yaml
    docker:
      name: docker image
      needs: release-plz
      if: ${{ needs.release-plz.outputs.releases_created == 'true' }}
      permissions:
        contents: read
        packages: write
      uses: ./.github/workflows/docker.yml
      with:
        version: ${{ fromJSON(needs.release-plz.outputs.releases)[0].version }}
  ```

  **`permissions:` on the calling job is mandatory and is the easiest thing to omit.**
  `release-plz.yml:7-9` grants `contents: write, pull-requests: write` — **no `packages: write`**.
  Per GitHub's reusable-workflow reference, the caller's grant is a hard ceiling:

  > The `GITHUB_TOKEN` permissions passed from the caller workflow can be only downgraded (not
  > elevated) by the called workflow.

  So `docker.yml`'s own job-level `{contents: read, packages: write}` is a *downgrade request
  applied to what it was handed*, **not** a grant. Without the block above it is handed no
  package scope, and the run authenticates to GHCR successfully and then 403s on the push —
  *after* a 35-50 minute multi-arch build. That is the failure mode that reads as "the secret
  worked but the push was denied", and it is why this is called out as its own acceptance
  criterion.

  `jobs.<job_id>.permissions` is one of the twelve keys explicitly supported on a job that calls a
  reusable workflow, and GitHub's own example caller sets it on a `uses:` job. Setting it there
  rather than widening `release-plz.yml`'s workflow-level block keeps `packages: write` away from
  the `release-plz` and `sync-integration-lock` jobs, which have no business with the registry.
  It also sidesteps a documented ambiguity: the reference says that when the calling job omits
  `permissions:`, "the called workflow will have the **default** permissions for the
  `GITHUB_TOKEN`" — which does not clearly mean the caller's workflow-level block. Being explicit
  removes the question.

  `publish.yml` never surfaced any of this because it needs no `GITHUB_TOKEN` scope at all — only
  the `CARGO_REGISTRY_TOKEN` secret.

  No `secrets: inherit` is needed: `docker.yml` uses only `secrets.GITHUB_TOKEN`
  (`docker.yml:38`), and a called workflow "is automatically granted access to `github.token` and
  `secrets.GITHUB_TOKEN`". Note that `secrets: inherit` would not have helped anyway — secrets and
  permissions are orthogonal, and inheriting secrets grants no scope.

  **(b) Expose `releases` as a job output** alongside the existing three
  (`release-plz.yml:32-39`), and read the version as `fromJSON(...)[0].version`.

  Verified at the pinned action SHA `b5543c19b03be9bd48852d20ca89f478b7723260` (= v0.5.132):
  `releases` is a declared output, added in v0.5.54 — 78 releases before this pin, so no version
  risk. Its `run:` block does `releases=$(echo $release_output | jq -c .releases)` and writes one
  line to `$GITHUB_OUTPUT`, so it is always a single-line JSON **array** — `[]` when nothing
  released, never the literal `{}`. That is a genuine difference from the `pr` output, whose `{}`
  quirk `release-plz.yml:34-37` already documents; the new output does **not** need the same
  warning, and `fromJSON` is safe on it. `releases_created` is itself computed as
  `jq 'length' != 0`, so the `if:` guard above proves index `0` exists.

  **Why `[0]` and not a `select(.package_name == "macp-runtime")`.** Every entry carries the same
  `version`, because `release-plz.toml:45-65` puts all seven crates in one
  `version_group = "macp"` and `ci.yml`'s "Internal crate versions are in lockstep" step asserts
  that on every PR. Only the `tag` field differs per crate, and the design does not use `tag` —
  the checkout target is `github.sha`, which is the released commit. Indexing is therefore exact,
  not a shortcut, and Phase 1(e)'s cross-check against the tree's `Cargo.toml` fails the build
  loudly if that reasoning is ever wrong.

  The `select()` alternative is rejected on structural grounds, not cosmetic ones. It is not
  expressible in `${{ }}` (no jq equivalent), so it needs a shell step. That step cannot live in
  the `release-plz` job: `publish` is gated on `needs: release-plz`, so **any** step added there
  that can fail would, on failing, skip the crates.io upload while tags and the GitHub Release
  already exist — verbatim the 0.6.1 incident `release-plz.yml:15-26` exists to prevent, and the
  same argument that makes `sync-integration-lock` a separate job (`release-plz.yml:83-91`). It
  would therefore need a *third* job purely to run one `jq` line, plus a `needs` edge, to obtain a
  value a pure expression already yields correctly. Not worth it.

  **(c) Extend the workflow header comment** (`release-plz.yml:15-26`) to name `docker.yml` as the
  second called workflow, with the same rationale. That comment is the repo's canonical statement
  of why calling beats tagging; leaving it describing only `publish.yml` invites the next reader
  to treat the docker call as ad hoc.

  **(d) Guard the extracted version before the call, not after.** Phase 1's `/ship`-gate review
  found that Phase 1(c)'s semver validation does **not** catch an empty `version` reaching
  `docker.yml` the way this section originally (and wrongly) assumed. Phase 1's resolve step
  detects a `workflow_call` invocation by `INPUT_VERSION` being **non-empty** — a platform
  correction made necessary because `github.event_name` is never literally `"workflow_call"`
  inside a called workflow (it inherits the caller's own event). One consequence: an *empty*
  `version` input doesn't land in the release-build branch at all — it fails the `-n
  "$INPUT_VERSION"` test and falls through to the plain-push branch-build arm instead, silently
  building a second, concurrent branch build of the same commit (no semver tag, `latest` and
  `main` re-pushed) rather than failing loudly. `docker.yml` cannot close this from its own side:
  a genuine push to `main` and a `workflow_call` with an empty version are indistinguishable from
  inside the called workflow (identical inherited `event_name`/`ref`/`sha`).

  So this job's own `with:` block must validate non-emptiness before the call, e.g. a `run:` step
  ahead of the `uses:` (or an `if:` on the job itself) asserting
  `fromJSON(needs.release-plz.outputs.releases)[0].version != ''`, failing loudly (`::error::` +
  non-zero exit / a job-level `if:` that still surfaces as a visible skip-with-reason, not a
  silent one) rather than letting an empty value reach `docker.yml` at all. This is a **new
  acceptance criterion for this phase**, not optional polish — see AC7 below.

- **Edge cases & failure modes:**
  - *A run that only opens/refreshes the release PR* → `releases_created == 'false'` → job
    skipped, as `publish` is.
  - *The docker job fails* → `publish` is unaffected; it declares only `needs: release-plz` and
    they are siblings. A release can therefore complete on crates.io with no image, which is the
    correct priority ordering and is recoverable by the Phase 2 dispatch.
  - *`releases` is `[]` despite `releases_created == 'true'`* → impossible by the action's own
    definition of that boolean, so this specific case cannot occur. **Superseded concern:** an
    earlier draft of this section claimed an empty `fromJSON(...)[0].version` would be caught by
    Phase 1(c)'s semver guard. It is not — see (d) above. The guard belongs on this side of the
    call, and (d)/AC7 exist because of it.
  - *Duplicate build for the release commit* (Phase 1's first edge case) — accepted; disjoint
    tags, but **both builds run cold and concurrently**, so the real cost is a second full
    ~45-minute multi-arch build, not a cache hit.
  - **The release run now holds its concurrency group for ~45 minutes instead of ~3.**
    `release-plz.yml:11-13` sets `concurrency: release-plz-${{ github.ref }}` with
    `cancel-in-progress: false`, and a called workflow's jobs run inside the caller's run. The
    v0.8.1 release run took **2m39s**; adding the image build makes it ~45-50 minutes, during
    which pushes to `main` **queue** their release-plz runs rather than cancelling them.

    Accepted, with the trade-off stated rather than discovered later: the queued work is
    release-PR refreshes, which are idempotent and simply happen later; releases are roughly
    weekly; and serialising releases is the property that group exists to provide. The unacceptable
    version of this is a *hung* build holding the group for the 360-minute default — which is
    exactly what `release-plz.yml:95-99` already reasons about for `sync-integration-lock`, and
    why Phase 1(i) adds `timeout-minutes` to `docker.yml`.

    If the queueing ever becomes a real problem, the escape is to stop calling `docker.yml` and
    trigger it out-of-band — but that needs a PAT (a `GITHUB_TOKEN` cannot fire
    `workflow_dispatch` either), which is a larger change than this issue warrants. Not now.
  - *Nesting* is one level (`release-plz.yml` → `docker.yml`), parallel to the existing `publish`
    call. The limits are ten levels and 50 unique reusable workflows per top-level caller; this
    takes the repo to two.
- **Acceptance criteria:**
  1. `release-plz.yml` has a `docker` job with `needs: release-plz` and
     `if: releases_created == 'true'`, invoking `./.github/workflows/docker.yml` via `uses:`.
  2. That job declares `permissions: {contents: read, packages: write}`; the workflow-level
     `permissions` block at `release-plz.yml:7-9` is **unchanged**, so no other job gains
     registry scope.
  3. The `release-plz` job's `outputs:` exposes `releases` in addition to the existing three.
  4. `publish` is untouched — same `needs`, same `if`, and it does **not** list `docker` in
     `needs`.
  5. The header comment names both called workflows and why.
  6. `actionlint .github/workflows/release-plz.yml` is clean.
  7. **A run in which the extracted version is empty fails loudly before invoking `docker.yml`**,
     rather than reaching the call and silently degrading to a second branch build — see (d).
     Provable by a dry-run of the guard's condition/script against a crafted `releases: []`-shaped
     (or single-entry-with-empty-`version`) payload, the same way (b)'s `fromJSON` behavior is
     tested.
- **Tests:** `actionlint`, plus `fromJSON(...)[0].version` evaluated locally against a real
  `releases` payload shape recorded in `PROGRESS.md`, **and AC7's guard exercised against an
  empty-version payload to confirm it fails loudly rather than reaching the call.** The true
  end-to-end test is the **next real release**, which must be watched rather than assumed — see
  Enterprise concerns.
- **Docs:** Phase 4.

---

### Phase 4 — Document the published image

- **Status:** TODO
- **Delivers:** the semver tags are discoverable, and the standing downstream advice "do not pin a
  semver tag" is retired.
- **Depends on:** Phases 1-3.
- **Files:** `CONTRIBUTING.md`, `docs/deployment.md`
- **Approach:** `docs/deployment.md:327` ("## Container deployment") documents only building the
  local `Dockerfile` — `docker run ... macp-runtime` with no registry reference anywhere in the
  tracked docs. Add the published image and the tag contract: `:X.Y.Z` immutable, `:X.Y` moving
  within a minor, `latest` = tip of `main` (**not** the newest release — an important distinction
  that this repo's tagging makes true and that readers will otherwise assume backwards), plus SHA
  tags. `CONTRIBUTING.md:134` ("### Approving a release PR") gains one sentence that a release now
  also publishes an image, next to the existing `sync-integration-lock` explanation.

  `CLAUDE.md` is **gitignored** in this tree (the same carve-out recorded in `DECISIONS.md` D45),
  so a note there is a local mirror only and will not appear in the PR diff. Update it anyway; say
  plainly in the phase report that it is absent from the diff by design, not skipped.

  Also state two things readers would otherwise assume wrongly:
  - **`:X.Y.Z` and `:<sha>` for the same release do not share a digest**, because the two builds
    stamp different `org.opencontainers.image.created` values (`meta.ts:547`). Same source, two
    images.
  - **A manually backfilled image's SLSA provenance attestation references the dispatch's commit,
    not the tag's** (Phase 1(f)'s residual). Only `:0.8.1` is affected; every image published by
    the automatic path has correct provenance. The `org.opencontainers.image.revision` label is
    correct in both cases.

  **Out of scope, but worth reporting:** one sibling-repo plan instructs readers not to pin a
  semver tag, citing this exact defect —
  `plans/cross-repo/macp-sdk-python-examples-docs-and-release.md:101` and `:221` ("Do not pin a
  semver tag … the list stops at `0.5.0`/`0.5`"). Two others merely *use* `:latest` without
  advising against semver (`…python-handoff-implicit-accept-0-8-0.md:34`,
  `…typescript-handoff-implicit-accept-0-8-0.md:27,33`) and need no change. These live in other
  repositories; this plan does not edit them. Flag the first in the final report so the issue's
  downstream consumer is told the workaround can be retired.
- **Edge cases & failure modes:** none functional. The risk is documenting a contract the
  workflow does not implement — so write this phase **after** Phase 2's registry probe, against
  observed tags rather than intent.
- **Acceptance criteria:**
  1. `docs/deployment.md` names the GHCR image and documents all four tag kinds, including that
     `latest` tracks `main` rather than the newest release.
  2. `CONTRIBUTING.md`'s release section mentions the image publish.
  3. No documented claim contradicts the final `docker.yml`.
  4. The phase report states the `CLAUDE.md` edit is absent from the diff because the file is
     gitignored.
- **Tests:** prose, read against the final workflow and against the Phase 2 tag list.
- **Docs:** this phase is the docs phase.

---

### Phase 5 — Observe the next real release

- **Status:** TODO — **a manual observation, like Phase 2. It cannot be completed by a code
  change,** and the plan is not done until it is recorded.
- **Delivers:** the only real proof of acceptance criterion 1. Phases 1-3 are verifiable by
  inspection and by a manual dispatch; **neither observes the automatic path actually firing on a
  release.** Without this phase the plan could be signed off with criterion 1 unproven — which is
  precisely how the original bug survived seven releases while every workflow looked correct.
- **Depends on:** Phase 3 merged, plus the next release being cut (days to a week).
- **Files:** `PROGRESS.md` only.
- **Approach:** when the next release PR merges, watch the `release-plz` run and record:
  1. the `docker` job appeared in the run and succeeded;
  2. `:X.Y.Z` and `:X.Y` for the new version are present on GHCR;
  3. that image's `org.opencontainers.image.revision` equals the release commit;
  4. `latest` still tracks `main` (it will have been re-pushed by the branch build for the same
     commit — confirm it points at the release commit, which is `main`'s tip at that moment);
  5. `publish` still succeeded, i.e. the new job did not disturb the crates.io upload;
  6. the wall-clock the release run now takes, against the ~2m39s baseline — this is the
     concurrency-group cost from Phase 3, measured rather than estimated.
- **Edge cases & failure modes:** if the `docker` job is skipped, check `releases_created`; if it
  fails on the GHCR push with a 403, the calling job's `permissions:` block is missing or wrong
  (Phase 3(a)) — that is the predicted failure and it costs a build, not a release.
- **Acceptance criteria:** all six observations above recorded in `PROGRESS.md`, with the run URL.
  Until then the issue's criterion 1 is **implemented but unobserved**, and the plan should say so
  rather than claim completion.
- **Tests:** the observation is the test.
- **Docs:** none.

---

## Long-term posture

- **Nothing here is a one-way door.** Two workflow edits and a docs edit, each revertable in one
  commit. The backfill pushes immutable new tags to GHCR and moves `0.8`; it deletes nothing.
- **The durable lesson is architectural, and it is now recorded twice.** Tag triggers do not fire
  for release-plz's tags. `publish.yml` learned this at the cost of the 0.6.1 release;
  `docker.yml` is learning it at the cost of seven un-imaged releases. After this plan, both
  called workflows carry the same comment, so the third one will not have to learn it again.
- **Debt deliberately not taken:** collapsing `ci.yml`'s `docker-build` gate and `docker.yml` into
  one reusable workflow. Tempting (they duplicate the build step) but it couples a required PR
  status context to the release path. Not now.
- **Residual coupling:** `docker.yml` now assumes the tag prefix `macp-runtime-v` in exactly one
  place (the resolve step's fallback for the tag-push and dispatch paths). If `release-plz.toml`
  ever sets a custom `git_tag_name`, that fallback and the `push:` glob both need updating —
  Phase 1(e)'s cross-check turns that into a loud failure rather than a silent one.

## Enterprise concerns

- **Failure domain.** The `docker` job is a sibling of `publish`, never a dependency of it. An
  image failure cannot cost a crates.io release; the reverse is also true. This is the same
  isolation principle `release-plz.yml:83-91` states for `sync-integration-lock`.
- **Least privilege.** `packages: write` is granted on the one calling job, not workflow-wide.
  No new secrets. The `macp-runtime-v*` backstop trigger deliberately remains reachable by a
  human- or PAT-pushed tag.
- **Rollback.** Revert Phase 3 to stop automatic publishing (Phase 1 then idles, reachable only by
  dispatch). Revert Phase 1 as well to return to today's behavior. Published tags are not rolled
  back; `0.8` would simply stop moving.
- **Cost.** One extra multi-arch build per release, **cold, ~45 minutes** — not a cache hit. The
  two builds for a release commit start ~3 minutes apart and run concurrently, so neither warms
  the other's `type=gha` cache. Releases are roughly weekly.
- **Latency.** Consequently the release run holds `release-plz-${{ github.ref }}` for ~45-50
  minutes instead of ~2m39s, queueing (not cancelling) subsequent `main` pushes' release-plz runs.
  Accepted; reasoned through in Phase 3's edge cases, and bounded by `timeout-minutes` so a hung
  build cannot hold it for the 360-minute default.
- **Supply chain.** `provenance`/`sbom` stay on. The published `org.opencontainers.image.revision`
  is pinned to the commit actually built (Phase 1(f)) so no image asserts a source commit it was
  not built from; the one residual — a manually backfilled image's SLSA attestation naming the
  dispatch commit — is documented rather than silently shipped.
- **Validation.** **Phase 5 exists for this** and is a required part of the plan, not a
  nice-to-have: the next real release is the only proof of criterion 1, and it must be watched
  rather than assumed. Phase 2 is the rehearsal that makes it low-risk.
- **Observability.** Every failure mode introduced here is loud by construction: a missing or
  mismatched version fails the job rather than emitting the warning-and-skip that hid the original
  bug for seven releases.

## Open questions

None blocking. Three judgment calls logged rather than asked, all to go to `ASSUMPTIONS.md` as
`UNCONFIRMED` and be revisited at `/reconcile`:

- **`fromJSON(releases)[0].version` instead of a `jq select()` on `package_name`.** Justified by
  the enforced lockstep invariant. Note the backstop is weaker than revision 1 claimed: Phase
  1(e)'s `Cargo.toml` cross-check catches a wrong *version*, not a wrong *checkout*. Blast radius
  if wrong: a mis-tagged image, caught by that cross-check before it is pushed.
- **Backfilling 0.8.1 only, not 0.8.0.** Criterion 2 asks for "the current release". Adding 0.8.0
  later is possible but must be built *before* a 0.8.1 rebuild to leave the moving `0.8` tag
  correct.
- **Accepting a ~45-minute release run in exchange for an automatic image.** The image build runs
  inside the release run's concurrency group, so `main` pushes queue for the duration (Phase 3's
  edge cases). Judged worth it: the queued work is idempotent release-PR refreshes, releases are
  weekly, and the alternative (an out-of-band trigger) needs a PAT. Blast radius if wrong: delayed
  release-PR refreshes, no lost work. Revisit if release cadence rises.

## Models

Opus plans, Opus executes, fresh Opus verifies. **No Fable at any tier.** No phase here is a
one-way door; where the skill would escalate, a fresh Opus agent is used and the substitution is
noted in `PROGRESS.md`.

## Plan review

A fresh **Opus** reviewer, with no sight of the planning work, re-verified every load-bearing
citation against the real tree, re-probed GHCR, and read both pinned actions' sources.

**Round 1 verdict: FLAWED** — 2 BLOCKERs, 7 SHOULD-FIXes, 6 NITs. The architecture
(`workflow_call` + `value=` + disjoint tag sets) was upheld; the defects were in specification
precision and in what the acceptance criteria could actually observe. All are applied in this
revision:

| ID | Finding | Applied as |
|----|---------|-----------|
| B1 | The `ref` output was specified per *build kind*, which is unimplementable on the `workflow_call` path (release build, but no tag name and no `ref` input). A literal executor would build the tip of `main` and tag it `:0.8.1`. **Phase 1(e)'s `Cargo.toml` check cannot catch this** — `main` keeps the released version afterwards (3 such commits exist today) | 1(b) replaced with an exhaustive **per-trigger** table; 1(e) rewritten to state plainly what it does *not* guard |
| B2 | `org.opencontainers.image.revision` comes from `github.sha` (`meta.ts:548`), so a backfilled `:0.8.1` would carry a **false source-commit claim**; and Phase 2's "built from the tag" criterion was therefore uncheckable | New 1(f): explicit `labels:` override pinning the label to the commit actually checked out; Phase 2 AC 2 rewritten against that label; the un-overridable SLSA-attestation residual documented |
| S3/S4 | The ~45-min build now holds `release-plz`'s concurrency group (`cancel-in-progress: false`), and `docker.yml` has no `timeout-minutes` — the hazard `release-plz.yml:95-99` already fixed once for `sync-integration-lock` | New 1(i) `timeout-minutes: 90`; the trade-off reasoned through in Phase 3's edge cases and in Enterprise concerns |
| S5 | "near-total cache hit" / "mostly cache-warm" was **wrong**: the two builds start ~3 min apart and run concurrently, so both run cold | Cost corrected to a full additional ~45-min build, in Phase 1's edge cases and Enterprise concerns |
| S6 | `enable=` must be the literal `true`/`false`; `metadata-action` **throws** otherwise (`meta.ts:54-57`) | Stated in 1(b), and reframed as a free hard-fail guard |
| S7 | `PROGRESS.md` cited base `5e95c4a`, which is **not an ancestor of HEAD** | Corrected to `11429e8`, with a warning not to trust a session-opening git snapshot (caught independently during planning) |
| S8 | ACs 2 and 5 are only observable **post-merge** — no `pull_request` trigger, and a feature-branch dispatch would push a stray `<branch>` tag | Both relabelled as post-merge observations with the hazard named |
| S9 | **No phase actually observed a real release publishing an image**, so criterion 1 could not be signed off at merge | **New Phase 5**, a required manual observation with six recorded checks |
| N10 | The `workflow_run` rejection gave a reason that does not apply (release-plz.yml is triggered by a human merge push, so it *would* fire) | Replaced with the real reasons: no access to job outputs; runs the default-branch copy |
| N11-N14 | GHCR breakdown omitted 5 `pr-*` tags; `latest`-digest check was racy; two-digests-per-commit unstated; downstream citations over-claimed (only one of the three advises against semver pinning) | All four corrected in place |
| N15 | The dispatch `version` override is unused surface | Kept deliberately as a recovery hatch; noted |

The reviewer independently **confirmed** the two assumptions the plan had flagged as risks, so
neither needs re-checking at implementation time: `enable=` is a **global** attribute honored for
every tag type (`tag.ts:206-208`, `meta.ts:53-60`), and `flavor: latest=false` does **not**
suppress an explicit `type=raw,value=latest` (`procRaw` → `version.main`, emitted at
`generateTags:521-523`; the flavor governs only the separate auto-latest at `:525-527`). It also
established a bonus fact that strengthens criterion 3: on a `main` push today `procRefBranch`
already sets `version.latest = false`, so `latest` *already* comes solely from the raw rule and
the flavor change is a no-op for branch builds.

Every file:line citation in both documents was checked against the real tree and found correct,
as were the GHCR probe (109 tags), the `0.8.1` version, the missing `packages: write`, the absent
`flavor:`, the `v*`-only-in-`docker.yml` survey, and the tag-copy-has-no-`workflow_dispatch`
finding.

Round 2 was not required: every finding was a specification or observability defect with a
determinate fix, and none invalidated the architecture.

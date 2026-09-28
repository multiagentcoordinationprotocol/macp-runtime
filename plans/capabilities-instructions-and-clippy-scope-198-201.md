# PLAN — fix the stale `x-macp-agent-id` capabilities string (#198) and CI's clippy scope gap (#201)

**Written:** 2026-09-28 · **Base:** `ca8baad` on `main` (workspace `0.8.3`) · **Planner:** Opus, both files read directly, both issues' acceptance criteria checked against live code before drafting.

**Models for execution:** Opus, two phases, both `Risk: simple`. No Fable: neither is a one-way door or trust boundary — one is a served string correction with zero behavior change, the other is a CI lint-scope fix plus a `#[cfg(test)]`-only type alias.

---

## Context

Two small, unrelated maintenance issues, bundled into one `/drive` run at the user's request. Independent files, independent concerns — batched as one PR for review convenience (same rationale as the `#169`/`#170` precedent: "the two issues are one story and the PR body wants to tell it once"), not because they depend on each other.

**#198.** `src/server.rs:857`'s `InitializeResponse.instructions` string (served on every `Initialize` call) tells clients: *"For local development only, x-macp-agent-id may be enabled by configuration."* No non-test code path reads that header — confirmed by a repo-wide grep across every file type (not just `.rs`): every occurrence of `x-macp-agent-id` is either that one instructions string or inside `crates/macp-auth/src/security.rs`'s `#[cfg(test)] mod tests` (opens `:534-535`; the five hits are at `:599, :970, :993, :1020, :1050`, all inside). The actual dev-mode behavior, read directly at `crates/macp-auth/src/security.rs:136-148` (`dev_authenticate`): reached whenever no auth is configured (`MACP_ALLOW_INSECURE=1` with no tokens/issuer, per `src/main.rs` — see also `CLAUDE.md`'s "Build and run" section, which already documents this correctly), it takes the bearer token's own value as `sender` directly (`sender: token` at `:139`). (`bearer_token`, `:394-406`, also accepts an `x-macp-token` header as an alternate way to *supply* the bearer value at `:401-405` — unrelated to `x-macp-agent-id`, not part of what #198 asks to fix, but the replacement string below is worded to not imply no header is ever involved, just that this specific header isn't.)

**#201.** `.github/workflows/ci.yml`'s `Run clippy` step (`:158-159`) runs `cargo clippy --all-targets -- -D warnings` with no `--workspace` flag. The root `Cargo.toml` has its own `[package]` (`:1`) and no `default-members` override in `[workspace]` (`:15-16`), so Cargo's default-members rule scopes an unqualified invocation to the root `macp-runtime` package only. **Coverage is not literally zero for every other crate**, though: the `features` job separately runs `cargo clippy -p macp-storage --features rocksdb-backend,redis-backend --all-targets -- -D warnings` (`ci.yml:456`) and two more `-p macp-runtime` variants (`:460`, `:465`) — but that only reaches `macp-storage` (under those specific features) and the root crate again, never `macp-core`, `macp-pb`, `macp-policy`, `macp-modes`, or `macp-auth`, and even `macp-storage`'s coverage there is narrower than a full `--workspace --all-targets` sweep. Confirmed live during the #197/#199 PR's verification, and re-confirmed during plan review with a clean-cache run: `cargo clippy --all-targets -- -D warnings` (CI's exact command) is clean; `cargo clippy --workspace --all-targets -- -D warnings` finds exactly **one** issue, at `crates/macp-modes/src/mode/multi_round.rs:1014`, `clippy::type_complexity` on `let shapes: [(&str, fn(usize) -> String); 5]` — `#[cfg(test)]`-only code from already-merged PR #196 (2026-09-25), invisible to CI ever since (every other crate's lib and non-lib targets are otherwise clean at `--workspace` scope). `Makefile:18` has this same narrow command, feeding `make check` (`:37`) and `make test-all` (`:29`) — local contributor runs are blind in exactly the same way and need the same fix.

**Scope.** Two phases, entirely in `macp-runtime`. No cross-repo writes.

---

## Phase 1 — correct the capabilities `instructions` string (#198)

- **Status:** DONE (2026-09-28). Implemented exactly as planned, no divergence. All 5
  acceptance criteria measured (see `PROGRESS.md`).
- **Risk:** simple — one string literal + one new test, zero behavior change, no new boundary.
- **Delivers:** `Initialize`'s served `instructions` field describes the dev-mode auth path that actually exists instead of a removed header. Closes **#198**.
- **Depends on:** nothing.
- **Files:** `src/server.rs:857`.
- **Approach:** Replace the trailing sentence. Keep the first two sentences (`Authenticate requests with Authorization: Bearer <token>. Use the unary Send RPC for all session messaging.`) verbatim — accurate and unrelated to the defect. Replace `"For local development only, x-macp-agent-id may be enabled by configuration."` with a sentence describing the real path, phrased consistently with `CLAUDE.md`'s existing, already-correct description of the same mechanism ("With no auth configured, the runtime falls back to dev-mode auth: any `Authorization: Bearer <value>` header authenticates as sender `<value>`"):

  > `"For local development only (MACP_ALLOW_INSECURE=1 with no auth configured), the bearer token's value is used directly as the sender identity."`

  **Rejected:** dropping the dev-mode sentence entirely instead of correcting it. The issue's acceptance criterion 2 explicitly asks for "the one that exists," and a client implementer reading capabilities benefits from knowing the real dev shortcut exists, not just that the old one doesn't.
- **Edge cases & failure modes:** None — this is a served string with no parsing consumer inside this codebase (`src/bin/*.rs`'s own `instructions` fields are unrelated example-client request payloads, a different proto field on a different message type entirely). The only "failure mode" is a stale claim reappearing, which is what criterion 3 below guards.
- **Acceptance criteria:**
  1. `src/server.rs:857`'s `instructions` string no longer contains `x-macp-agent-id`.
  2. The replacement sentence accurately describes `dev_authenticate` (`crates/macp-auth/src/security.rs:126-146`): bearer-token-value-as-sender, gated on no auth configured under `MACP_ALLOW_INSECURE`.
  3. `grep -rn "x-macp-agent-id" --include="*.rs" .` (excluding `target/`) returns matches **only** inside `crates/macp-auth/src/security.rs`'s `#[cfg(test)] mod tests` block (i.e., only at line numbers > the `mod tests` opening line) — zero matches elsewhere, including `src/server.rs`.
  4. `cargo test -p macp-auth --lib` and the two `initialize`-related tests in `src/server.rs` (`initialize_empty_versions_rejected` at `:2335`, `initialize_unsupported_version_rejected` at `:2349`) still pass unmodified — neither currently asserts on `InitializeResponse.instructions` (confirmed: every `instructions` hit besides `:857` itself belongs to the unrelated `TaskPayload.instructions` field), so nothing should need updating.
  5. **New regression test**, added by this phase: a unit or integration test calling `initialize` and asserting `response.instructions` does not contain `"x-macp-agent-id"` (and, positively, does contain `"MACP_ALLOW_INSECURE"` or similar, so the test fails if the sentence is deleted rather than corrected) — so #198 cannot silently regress. Place it beside `initialize_empty_versions_rejected`/`initialize_unsupported_version_rejected` in `src/server.rs`'s test module.
- **Tests:** criterion 5's new test is the regression guard; criterion 3's grep and criterion 4's existing-test re-run are the rest of the proof.
- **Docs:** none. `CLAUDE.md`'s own description of dev-mode auth (`## Build and run`) was already correct and is the source this phase's replacement string is checked against, not a target to update.

## Phase 2 — give CI's clippy step real `crates/*` coverage (#201)

- **Status:** DONE (2026-09-28). Implemented exactly as planned, no divergence. All 5
  acceptance criteria measured, including the clean-build-cache re-run the plan review
  required (see `PROGRESS.md`).
- **Risk:** simple — CI config plus a `#[cfg(test)]`-only type alias; no production code, no new boundary.
- **Delivers:** CI's required `Clippy` check actually lints all seven crates, and the lint failure it was missing is fixed. Closes **#201**.
- **Depends on:** nothing (independent of Phase 1; different files, different crates).
- **Files:** `.github/workflows/ci.yml:159`; `Makefile:18` (same narrow command, feeds `make check`/`make test-all`); `crates/macp-modes/src/mode/multi_round.rs:1014` (test-only).
- **Approach:** Two changes, in this order (fix the lint first, so the CI-scope change lands already green rather than landing red for one commit):
  1. **`multi_round.rs:1014`** — factor the tuple-of-fn-pointer array into a named type alias, exactly per clippy's own suggestion (`for further information visit ... #type_complexity`), placed immediately above the test that uses it (`canonical_proto_exhaustively_round_trips_at_rev3`, the sole user):
     ```rust
     type ContributeValueShape = (&'static str, fn(usize) -> String);
     ```
     then `let shapes: [(&str, fn(usize) -> String); 5]` becomes `let shapes: [ContributeValueShape; 5]`. **Rejected:** `#[allow(clippy::type_complexity)]` — it suppresses the signal for this one site forever rather than fixing the (real, if minor) readability complaint clippy is making, and the type alias costs one line. **Rejected:** restructuring the test to avoid an array-of-closures shape entirely — unrelated scope creep on a test that's otherwise fine; the shape itself is fine, only its spelled-out type is complex.
  2. **`ci.yml:159`** — `cargo clippy --all-targets -- -D warnings` → `cargo clippy --workspace --all-targets -- -D warnings`. **`Makefile:18`** — same change, same reasoning (local `make check`/`make test-all` currently reproduce CI's blind spot exactly). **Rejected:** leaving the scope as-is and only fixing the one found lint — the issue this phase closes is specifically about the missing coverage, not just the one symptom it happened to surface; landing the alias fix without widening scope would leave the check exactly as blind as before for the next `crates/*` lint.
- **Edge cases & failure modes:** Widening `--workspace` could surface **other** pre-existing lints in `crates/*` beyond the one already found (the earlier `--workspace` run during the #197/#199 PR only got as far as the first error before Cargo aborted the build — clippy stops at the first hard error in a `-D warnings` run, so there is no guarantee `multi_round.rs:1014` was the *only* finding). This phase's acceptance criteria require a full clean `--workspace` run, not just a fix for the one known site — if more turn up, fix them too (same class of change: readability-only lints, not logic), and note any genuinely non-trivial one in `PROGRESS.md` rather than silently patching around it. `integration_tests/` is excluded from the root workspace (`Cargo.toml:18-19`) and is not linted by either invocation, before or after this phase — out of scope, unrelated to what #201 asked for (CI's existing `Clippy` job has never covered it and this phase doesn't change that job's checkout scope).
- **Acceptance criteria:**
  1. `crates/macp-modes/src/mode/multi_round.rs` defines `ContributeValueShape` and `canonical_proto_exhaustively_round_trips_at_rev3` uses it; `cargo test -p macp-modes --lib -- canonical_proto_exhaustively_round_trips_at_rev3` still passes (behavior-identical, type-alias-only change).
  2. `.github/workflows/ci.yml:159` and `Makefile:18` both read `cargo clippy --workspace --all-targets -- -D warnings`.
  3. `RUSTC_WRAPPER="" SDKROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX26.sdk cargo clippy --workspace --all-targets -- -D warnings` exits clean (the exact new CI/`make` command, run locally) — not just the narrower pre-existing invocation. **Run this against a clean build cache** (`cargo clean -p macp-core -p macp-pb -p macp-storage -p macp-policy -p macp-modes -p macp-auth -p macp-runtime`, or a fresh `target/`): clippy's lint arguments (`CLIPPY_ARGS`) aren't part of Cargo's fingerprint, so a cached-fresh unit from an earlier, narrower invocation can silently replay stale (empty) diagnostics and make a real failure look clean. Confirmed as a real risk during plan review, not a hypothetical.
  4. `actionlint` (or a diff-only review, since this is a one-token change on an existing line) confirms no YAML/shell syntax issue introduced.
  5. Full workspace suite still green: `cargo test --workspace` (unaffected — a type alias changes no runtime behavior).
- **Tests:** criterion 1 is the existing test, re-run to confirm the alias didn't change anything; criterion 3 is the actual regression test for the gap this phase closes — CI enforces it structurally from here on, so no new Rust test is needed to "test the fix," the fix *is* the test surface widening.
- **Docs:** none. No repo doc describes clippy's invocation scope today (checked: `CLAUDE.md` and `docs/` don't mention `cargo clippy` at all beyond the top-level `## Build and run` command list, which already shows plain `cargo clippy` with no flags — that's the local dev command, unaffected by this CI-only change).

---

## Long-term posture

Neither phase is a one-way door. Phase 1 is a served-string correction (no wire/schema change — `InitializeResponse.instructions` is a free-text field). Phase 2 widens a CI check's scope and fixes what it finds; if it surfaces additional pre-existing lints beyond the one already known, those get fixed in the same phase (same low-risk class) rather than deferred, per the Edge cases note above — deferring a **known-red** widened check would be worse than not widening it.

## Enterprise concerns

Phase 2 is itself the enterprise-concern fix: a required CI check that's silently only covering 1 of 7 crates is exactly the kind of gap that lets a real defect through while the badge stays green. No new concern introduced by closing it.

## Open questions

None requiring escalation. Both fixes are fully determined by the issues' own acceptance criteria and the live code read while planning.

## Repo map

- `src/server.rs:793-858` — `initialize` RPC handler; `:857` is the target string. `src/server.rs:1109-1133` — `get_manifest`, confirmed to have no separate `instructions` field (not a second occurrence to fix).
- `crates/macp-auth/src/security.rs:103-148` — `dev_mode()` / `dev_authenticate` (`:136-148`), the real dev-auth behavior Phase 1's replacement string describes. `:394-406` — `bearer_token`, which also accepts an unrelated `x-macp-token` alternate header (not part of #198's fix). `:534-535` — `#[cfg(test)] mod tests` opens; every other `x-macp-agent-id` occurrence in this file (`:599, :970, :993, :1020, :1050`) is inside it.
- `CLAUDE.md`'s `## Build and run` section — already-correct prior art for how to describe dev-mode auth in prose; Phase 1's new string is checked for consistency against it, not required to match verbatim (different audience: this repo's contributors vs. a wire client).
- `.github/workflows/ci.yml:1-39` — top `env:` block, `SPEC_REV` (unrelated to this plan, touched by the prior `#197`/`#199` PR). `:133-159` — the `clippy` job, `:159` is Phase 2's target line.
- `crates/macp-modes/src/mode/multi_round.rs:976-1022` — `canonical_proto_exhaustively_round_trips_at_rev3`, the sole consumer of the `shapes` array Phase 2 retypes.
- Root `Cargo.toml:1-19` — `[package]` (root is itself a crate) and `[workspace]` (`members = ["crates/*"]`, no `default-members`), the mechanism behind #201's gap.

## Plan review

**Round 1 (fresh Opus subagent, independently re-reading the code — including an empirical `cargo clippy --workspace --all-targets -- -D warnings` run on a cleaned cache, not just reading the plan's prose):** Verdict **REVISE**, three real gaps, several claims independently confirmed correct or *better* than drafted. Applied to the plan: (1) `Makefile:18` has the identical narrow clippy invocation, feeding `make check`/`make test-all` — added to Phase 2's `Files`/`Approach`/acceptance criteria, so local contributor runs get the same fix as CI; (2) Context's "`crates/*` get zero clippy coverage" was overstated — a separate `features` job (`ci.yml:456,460,465`) already covers `macp-storage` under specific feature flags and the root crate again, just not the other five crates or a full `--workspace --all-targets` sweep — corrected, and independently confirmed (via the reviewer's own clean-cache run) that `multi_round.rs:1014` really is the *only* finding at full `--workspace` scope, so Phase 2's fix is complete as scoped, not partial; (3) acceptance criterion 3 needed a clean-build-cache caveat — clippy's `-- <args>` aren't part of Cargo's fingerprint, so a stale cached unit can replay empty diagnostics from an earlier narrower run and look clean when it isn't; added the explicit `cargo clean -p ...` requirement. Also fixed: `dev_authenticate`'s cited line range (`:136-148`, not `:126-146`), the "no header involved at all" phrasing (there *is* an unrelated alternate-bearer-value header, `x-macp-token`, distinct from `x-macp-agent-id`), and added a Phase 1 regression test (criterion 5) so #198 can't silently regress, since the reviewer confirmed no existing test asserts on `InitializeResponse.instructions`. **Round 2:** not run — every finding was a small, independently-confirmable correction (a missing file, a narrative overstatement the reviewer's own empirical run refuted in the *safer* direction, a caching caveat), not a structural disagreement about approach; `/implement`'s per-phase verify gate re-checks against the real diff regardless.

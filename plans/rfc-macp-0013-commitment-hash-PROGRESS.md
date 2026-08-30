# PROGRESS — RFC-MACP-0013 canonical commitment hash (PR 3 of 5, macp-runtime)

**Plan:** `../multiagentcoordinationprotocol/plans/cross-repo/macp-runtime-rfc-macp-0013.md`
(spec repo, git-ignored there — this file is the durable tracker on this side).
**Verified against:** `e778b38` on `main`, clean tree, confirmed at implementation start (2026-08-29).

## Repo map (built once, §0)

| Path | Purpose |
|------|---------|
| `crates/macp-core/src/lib.rs` | Flat `pub mod` list (decision, error, mode, policy, session) + re-exports at `:15-25`. Phase 1 adds `pub mod commitment_hash;`. |
| `crates/macp-core/Cargo.toml` | `[dependencies]` at `:12-18`, all `{ workspace = true }`. Phase 1 adds `sha2` line. |
| `Cargo.toml` (root) | `[workspace.dependencies]` at `:35-68`. Phase 1 adds `sha2` entry. `macp-proto = "0.1.5"` at `:65`, locked at 0.1.5 in `Cargo.lock` (confirmed). |
| `crates/macp-modes/src/mode/util.rs` | Doc comment `:44-48`, `supersedes` well-formedness guard `:49-53` (currently `trim().is_empty()` on both fields). Fixture `"abc123"` at `:240` inside `well_formed_supersedes_is_accepted` (`:234-243`). Negative-case loop `:248` — currently `[("", "abc123"), ("prior-session", ""), ("  ", "abc123")]`, field assignment `:250-253`. Phase 2 touches all of this. |
| `tests/conformance_loader.rs` | `fixtures_dir()` `:521-526` (env-var override, ignored by the format-guard test). `fixtures_conform_to_canonical_format` `:590-643` — **non-recursive** `read_dir` over `tests/conformance`, panics on non-session-transcript `.json`. Floor `checked >= 13` at `:639-641`; real count today is **17**. |
| `tests/conformance/` | 17 vendored session-transcript fixtures + `schema.json`. Phase 3 adds a `cmt-hash/` subdirectory here (never flat files — H12). |
| `.github/workflows/ci.yml` | `conformance-oracle` job `:490-535`. Byte-diff loop over `tests/conformance/*.json` at `:504-513` (non-recursive glob — invisible to `cmt-hash/` unless extended). Oracle's own test step `:532-535` runs only `cargo test --test conformance_loader` against `MACP_CONFORMANCE_FIXTURES_DIR` = spec-repo checkout — **never** runs a new `cmt_hash_vectors.rs` binary. The separate `test` job (`:172` area) runs `cargo test --all-targets`, which **does** pick up `tests/cmt_hash_vectors.rs`, against the vendored copy. `ci-pass` needs-list at `:563`. |
| `crates/macp-pb` | Generated prost message types (`macp-proto` 0.1.5, `.extern_path` against it). `CommitmentPayload` (9 fields, field 9 = `supersedes: Option<CommitmentRef>`) and `CommitmentRef` (`session_id`, `commitment_hash`) confirmed from upstream `.proto` (field order matches RFC-MACP-0013 §5 exactly). |
| Spec repo RFC | `multiagentcoordinationprotocol/rfcs/RFC-MACP-0013-commitment-hash.md` — read in full; projection rules (§3), algorithm (§4: `sha256:` + hex of SHA-256(`"macp-commitment-hash/1:" || JCS(projection)`)), frozen field set (§5), hard-reject backward-compat (§9). |
| Spec repo vectors | `multiagentcoordinationprotocol/schemas/conformance/cmt-hash/` — 5 vectors + `vector-schema.json`, all read and hashes recorded below. |

## Reference vector hashes (pin exactly, H1/D5 — do not deviate)

| Vector | Hash |
|--------|------|
| `cmt_hash_001_minimal` | `sha256:9f58e9d114d11860d48aa2bcb8cda458b9618b1cc8560595a802b68c4af85d41` |
| `cmt_hash_002_supersedes` | `sha256:7cc490432ad6b25e9c19fc7c3a84f1e33abe497fca1fd5266ff0275db3650f9d` |
| `cmt_hash_003_all_empty` | `sha256:3240d1a7adb7bd9420ad5490182227ce699c9e4e465f7934885fe2ded939f32e` |
| `cmt_hash_004_empty_supersedes` | `sha256:9776c22ef165f26817f89bb456cf6bc56a659eb1561a576f6ea9a435bd3291d7` (`must_differ_from` 003) |
| `cmt_hash_005_escapes` | `sha256:03f8ac2b8172958504092ce9fe5154dbcfe300fd30a350453d4e4bd715822ab2` |

## Phase status

| Phase | Status | Verifier | Rounds | Commit | Shipped? |
|-------|--------|----------|--------|--------|----------|
| 1 — commitment_hash module | DONE | Opus | 1 (PASS) | (pending — see below) | Accumulating — not independently shippable (unused public API on a published crate until Phase 2 gives it a caller) |
| 2 — tighten §7.3.1 + fix fixtures | DONE | Opus | 2 (GAPS→PASS) | (pending — see below) | Accumulating — verifier's call: this is the plan's actual wire-visible break and should ship alongside Phase 3's conformance-oracle verification, not before it |
| 3 — vendor vectors + wire oracle | TODO | — | — | — | — |

## Log

- 2026-08-29: Repo map built, plan read from sibling checkout, RFC + vectors read in full, all line citations in the plan re-verified against `e778b38` and confirmed accurate. Starting Phase 1.
- 2026-08-29: Phase 1 done. Sonnet executor implemented `crates/macp-core/src/commitment_hash.rs` (hand-written JCS projection + SHA-256, `sha2` the only new dependency). Opus verifier independently re-derived the algorithm from the RFC, differentially fuzzed the string escaper against Python's `json.dumps` over 20,777 cases (zero mismatches), and confirmed all 8 acceptance criteria plus 4 extra CI gates (docs, MSRV 1.89.0, deps-isolation, full workspace tests). Verdict: **PASS**, round 1.
  - Files touched: `crates/macp-core/src/commitment_hash.rs` (new), `crates/macp-core/src/lib.rs`, `crates/macp-core/Cargo.toml`, root `Cargo.toml`, `Cargo.lock`.
  - Non-blocking follow-ups noted by the verifier, deliberately deferred rather than looped as gaps (verdict was PASS, not GAPS): (a) add unit tests for the untested-but-implemented `\b`/`\f`/`\r` short-form escapes; (b) add a `jcs_utf8_hex` assertion to vector 002 (localizes nested `supersedes` key-order regressions) and a `preimage_utf8_hex` assertion to at least one vector (localizes domain-prefix regressions) — both values already sit in the vendored vector files once Phase 3 lands them; (c) whether `macp-runtime`'s `src/lib.rs` should re-export `macp_core` so external consumers can reach `commitment_hash` without a direct `macp-core` dependency — CLAUDE.md states the root crate re-exports the lower crates, and today it re-exports `macp_modes`/`macp_policy`/`macp_storage`/`macp_auth` but not `macp_core` itself (only thin `error`/`session` shims). Logged to `ASSUMPTIONS.md`.
  - `Cargo.lock`'s incidental `socket2 0.5.10`→ dedup-onto-`0.6.4` edge move (both versions were already present pre-Phase-1; no new package, no version upgrade) is benign resolver dedup from adding `sha2` — verifier traced it fully, confirmed carry-forward is correct, do not `cargo update --precise` it back.
  - Committing now; accumulating locally per verifier's PR-timing call (§2 item 5) — Phase 2 depends on this module.
- 2026-08-29: Phase 2 done. Sonnet executor tightened `crates/macp-modes/src/mode/util.rs`'s `supersedes.commitment_hash` guard to `sha256:` + 64 lowercase hex, fixed both `"abc123"` fixture sites (valid-fixture now uses `cmt_hash_001_minimal`'s pinned hash), extended the negative-case loop with 3 new cases exercising the new logic, and added 9 direct unit tests of the new `is_canonical_commitment_hash` helper. Round 1 Opus verifier reproduced the "revert-and-confirm-fail" mutation test independently, recomputed the fixture hash from scratch in Python to confirm it against the spec vector, and traced every call site to confirm the rejection path introduces no new/differently-handled error route. Verdict: **GAPS** (2 items) — G1 (should-fix): the combined `session_id`/`commitment_hash` guard emitted one hash-specific log message even when `session_id` was the actual failure, misleading an operator since `MacpError::InvalidPayload` carries no message of its own; G2 (nice-to-fix): no test pinned that the `sha256:` prefix itself (not just the hex) must be lowercase.
  - Sonnet fixer split the guard into two independent checks with accurate per-field messages (G1) and added `canonical_hash_rejects_uppercase_prefix` (G2).
  - Round 2 Opus re-verifier confirmed both closed via mutation testing (temporarily replaced each `return Err` with a distinct `panic!` marker and confirmed the right branch fires for each failure shape; temporarily loosened the prefix check to case-insensitive and confirmed exactly the new test — and only that test — turns red). Verdict: **PASS**, round 2.
  - Files touched: `crates/macp-modes/src/mode/util.rs` only (both rounds) — confirmed via `git diff --stat` against the Phase 1 commit both times.
  - This is the plan's actual wire-visible break (RFC-MACP-0013 §9, immediate hard-reject, no dual-read period) — the PR description must state it explicitly, not read as a validation tidy-up.
  - Verifier's PR-timing call (§2 item 5): accumulate, do not ship alone — Phase 2's own tests are unit-level and self-referential (one hand-transcribed hash literal, no gRPC-boundary integration test of the rejection), and Phase 3's vendored conformance vectors + extended CI oracle are what verify the accepted format against the normative spec source rather than a copied string. A break with this blast radius should ship with the plan's strongest verification, not ahead of it.

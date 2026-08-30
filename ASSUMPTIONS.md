## macp-core re-export from macp-runtime's root
- **Plan:** `plans/rfc-macp-0013-commitment-hash-PROGRESS.md` (RFC-MACP-0013, PR 3 of 5)
- **Assumed:** CLAUDE.md states "the root crate re-exports the lower crates so the historical `macp_runtime::*` paths are preserved," and today `src/lib.rs` re-exports `macp_modes`, `macp_policy`, `macp_storage`, and `macp_auth` — but not `macp_core` itself (only thin `error`/`session` shims). An external consumer of the published `macp-runtime` crate therefore cannot reach `macp_core::commitment_hash::commitment_hash` through `macp_runtime::*` and must take a direct `macp-core` dependency.
- **Chose:** Left this alone in Phase 1 — `commitment_hash` is a new module-only export in `macp-core` (no flat function re-export at that crate's root either, since the crate's existing flat re-exports are all types, and a function re-export shadowing its own module name would read oddly at a call site). Whether `macp-runtime`'s root should also re-export `macp_core` wholesale is a pre-existing gap this phase surfaced, not one it introduced.
- **Alternatives:** Add `pub use macp_core;` (or a narrower `pub mod commitment_hash` shim) to `src/lib.rs` in this same PR.
- **Blast radius if wrong:** Low and cheap to reverse — adding a re-export later is backward-compatible (additive) and doesn't touch any existing call site.
- **Status:** UNCONFIRMED

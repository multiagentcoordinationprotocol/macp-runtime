# Stability policy for embeddable crates

Four crates are meant to be used as libraries by systems other than
`macp-runtime`'s own server: `macp-core`, `macp-modes`, `macp-policy` and
`macp-pb`. An embedder (see [Embedding](embedding.md)) pins them, drives
`macp_modes::step` and `macp_modes::replay` directly, decodes `macp_pb`
envelopes, and replays histories it recorded under earlier releases. This page
says what those crates promise across releases and how CI holds them to it.

`macp-storage`, `macp-auth` and the `macp-runtime` crate are published too, but
they are the server's internals. They follow Cargo's version rules, and nothing
more is promised for them.

`macp-conform`, the conformance runner, is published in the same lockstep
group. It is a tool, not an embeddable crate: it is not in the semver job's
crate list, and its API follows Cargo's version rules only. Its JSON report
is versioned separately, by the report's own `report_format` field.

## The public API

The public API of an embeddable crate is:

- every item reachable through `pub` paths from the crate root and shown in its
  rustdoc. Items marked `#[doc(hidden)]` are excluded, even when they are `pub`;
  they exist for this workspace's own tests and re-export paths;
- the behaviour those items document: an error variant's `error_code()`, which
  envelopes `step` accepts or rejects and in what order it checks them, what
  `replay` rebuilds from a given record;
- the meaning of `Session::semantics_rev` and the values of
  `CURRENT_SEMANTICS_REV`. This is the replay contract, set out in
  [Semantics revisions](semantics-revisions.md): a history sealed under
  revision N replays identically on every later release.
- `macp-pb`'s generated messages, as far as the `.proto` sources they are
  generated from are stable. Those are vendored from the spec repository's
  `macp-proto` package; field additions there are additive here.

Structs and enums marked `#[non_exhaustive]` can gain fields and variants in
any release, so a field or variant addition to one of them is not a breaking
change. Match them with a wildcard arm, and build them through their
constructors and `with_*` builders.

### Optional features

`macp-core`'s `receipts` feature (off by default) adds `macp_core::receipt`:
decision-receipt building, signing and verification (RFC-MACP-0013 §13,
Appendix A). The module is public API under the same promise as the rest of
the crate. With the feature off, `macp-core` carries no signature crates, which
CI checks with `cargo tree`. The receipt bytes `receipt::build` emits for given
inputs and key are part of that promise: a served receipt is permanent, so its
layout (the JCS form of the whole receipt) never changes within `macp-receipt/1`.

## What counts as a breaking change

A change is breaking when code or data that worked against the previous release
can stop working. That includes:

- an API break that `cargo-semver-checks` reports at major level: a removed or
  renamed item, a changed signature, a new required trait method, a type newly
  `#[non_exhaustive]`, and the rest of its lint list;
- a behaviour change to a public item that an embedder can observe without
  changing its own code: an envelope that was accepted and is now rejected (for
  a new session), a different error code for the same input, or a stricter
  `PolicyRegistry::register`;
- a change to what `replay` rebuilds from an existing record. This is never
  allowed silently, not even in a breaking release: an acceptance-time
  tightening gets a new `semantics_rev` instead (see
  [Semantics revisions](semantics-revisions.md));
- a raised MSRV (`rust-version` in the root `Cargo.toml`).

A loosening (an input that was rejected and is now accepted) is not breaking.
Neither is a new public item, a new variant of a `#[non_exhaustive]` enum, or a
bug fix that brings behaviour in line with the spec the item documents. Fixes
that change behaviour are still listed in [MIGRATING.md](../MIGRATING.md) when
an embedder could notice them.

## Version rules

The seven workspace crates share one version (see `release-plz.toml`). Before
1.0, Cargo reads the **minor** number as the breaking one:

- `0.x.y` to `0.x.(y+1)`: no breaking change in any embeddable crate.
- `0.x.y` to `0.(x+1).0`: may break. Every breaking change is announced in
  [MIGRATING.md](../MIGRATING.md) under a `## 0.(x+1).0` heading, with what
  changed and what an embedder does about it. That heading lands on `main` with
  the change itself, before the release.

From 1.0 on, the same rules apply to the major number.

Versions are computed by release-plz from the commit history. Mark a breaking
commit with `!` (`feat!: ...`) or a `BREAKING CHANGE:` footer so release-plz
picks the minor bump. Its own `cargo-semver-checks` run would pick it anyway
for an API break, but not for a behaviour break.

## How CI enforces it

The `Embedder contract` workflow (`.github/workflows/embedder-contract.yml`,
logic in `scripts/embedder_contract.py`) runs two jobs on every pull request.

**`Semver (embeddable crates)`** runs `cargo-semver-checks` (pinned version) on
the four crates against their last version published on crates.io:

1. No major-level finding: the job passes.
2. Otherwise it reruns against the PR's base commit, to tell a break this PR
   introduces from one already on `main`:
   - **already on `main`:** it passes when `MIGRATING.md` has a heading for the
     next breaking version (`## 0.(x+1).0`), and fails otherwise;
   - **introduced by this PR:** it passes only when the PR carries the
     `semver-breaking` label **and** edits `MIGRATING.md` so that it has that
     heading.

A release PR that bumps to `0.(x+1).0` passes step 1, since the bump permits
the break. If `cargo-semver-checks` fails without a verdict (a crate that does
not build, an unreachable registry), the job fails and says so. The check is API-only: it cannot see a behaviour break, so a
behaviour break is announced the same way by the author. Reviewers check for
it.

**`MIGRATING.md on breaking release`** runs only on release-plz's release PRs
(branches named `release-plz-*`). When the PR gives an embeddable crate a
breaking bump, `MIGRATING.md` must have a `## <new version>` heading. The entry
is expected to arrive on `main` with the breaking change. A human commit to a
release PR makes release-plz close it and open a new one, so the fix is to add
the entry on `main` and let release-plz refresh its PR.

# Semantics revisions

Every session records the behavioural revision it was accepted under,
`Session::semantics_rev`. Replay reads it from the record and applies that
revision's rules, so upgrading the runtime or the library never changes what an
existing history rebuilds to. This page is the compatibility promise built on
it, for embedders of `macp-core` and `macp-modes` (see [Embedding](embedding.md)
and [Stability](stability.md)).

The normative definition of a per-session semantics revision is in
RFC-MACP-0003. The reference runtime's revision is
`macp_core::session::CURRENT_SEMANTICS_REV`. Its value means something only to
this implementation: it is not an envelope field, and another implementation's
revision 4 is unrelated to this one's.

## The guarantee

1. **Sealed histories replay identically.** A history recorded under revision
   N replays to the same accept/reject decisions and the same state on every
   later release with the same major version. Before 1.0 that holds across
   minor releases too: a breaking release may change the API, but never what an
   existing history rebuilds to. The promise starts with 0.9.0, which itself
   corrected two replay divergences (see [MIGRATING.md](../MIGRATING.md)).
2. **Tightenings get a new revision.** A change that makes the runtime reject
   something it used to accept bumps `CURRENT_SEMANTICS_REV` to N+1 and is
   gated `semantics_rev >= N+1`. Sessions recorded earlier keep the older
   rule. Gates are always `>=` or `<=`, never `==`, so a later revision
   inherits every earlier one.
3. **Loosenings need no gate.** A change that only accepts more applies to
   every session, whatever its revision, because nothing it newly accepts can
   be in an older history. Quorum's S4 change in 0.9.0 is one.
4. **New sessions start at the current revision.** `accept_session_start` and
   `Session::builder` default to `CURRENT_SEMANTICS_REV`. Set a lower revision
   only to rebuild a history recorded under it. A legacy record with no
   revision replays as `0`.
5. **An unknown revision is refused.** A runtime or embedder MUST refuse to
   replay a history whose revision is newer than its own
   `CURRENT_SEMANTICS_REV`. It does not know that revision's rules, and
   replaying under its own would rebuild a different session without
   saying so.

   `macp_modes::replay` does not check this for you in 0.9.0. The server's
   log replay does not either. An embedder makes the check before calling
   `replay`:

   ```rust
   use macp_core::session::CURRENT_SEMANTICS_REV;

   if start.semantics_rev > CURRENT_SEMANTICS_REV {
       // Refuse: the log was written by a newer release.
   }
   ```

A bump of `CURRENT_SEMANTICS_REV` changes behaviour for new sessions, so it
ships in a breaking release (see [Stability](stability.md)) and is listed in
[MIGRATING.md](../MIGRATING.md).

## Revisions

The authoritative description of each revision, with the gate constant each
mode uses, is the rustdoc on
[`CURRENT_SEMANTICS_REV`](../crates/macp-core/src/session.rs). This table is
the summary. `tests/semantics_revisions_doc.rs` fails when the constant moves
without a row here.

| Rev | Release | Gates |
|-----|---------|-------|
| 0 | before 0.5.0 | Handoff's implicit-accept timeout is measured against the client-supplied envelope timestamp. |
| 1 | 0.5.0 | Handoff's implicit-accept timeout is measured against the runtime's acceptance clock (`MessageContext::accepted_at_ms`). |
| 2 | 0.8.0 | Handoff's implicit accept becomes a recorded synthetic `HandoffAccept` entry (RFC-MACP-0010 §5.1), and its deadline excludes suspended time. Independently, in every mode, `Session::resume` force-expires a session past `MAX_SUSPENSION_CYCLES`. |
| 3 | 0.8.3 | `ext.multi_round.v1` `Contribute` decoding gains the canonical-proto tie-break (issue #192). |
| 4 | 0.9.0 | Decision wire enums are case-exact and an empty `Objection.severity` is refused (S7). Handoff cannot re-offer to a target that declined (rule 3a, S5). A pre-assignment `TaskReject` is a personal decline (rule 2a, S6). |

## Adding a revision

When a change tightens acceptance:

1. Bump `CURRENT_SEMANTICS_REV`, add the revision to its rustdoc, and gate the
   rule `semantics_rev >= N` with a named constant in the mode.
2. Add a row to the table above.
3. Add a legacy-log test that replays a history recorded under N-1 to its
   original outcome (CONTRIBUTING.md, "Every behavior change lands with a
   test").
4. Add the revision to the `## <next breaking version>` entry in
   [MIGRATING.md](../MIGRATING.md).

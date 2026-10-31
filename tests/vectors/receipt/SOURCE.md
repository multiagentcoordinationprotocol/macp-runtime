# Source

The 13 `.json` files in this directory (the 12 RFC-MACP-0013 Appendix A.4
receipt vectors, `receipt_001`–`receipt_012`, and `vector-schema.json`) were
copied, byte-identical, point-in-time, from:

```
multiagentcoordinationprotocol/schemas/conformance/receipt/
```

in the spec repo, commit `0f77753a24e9fc4b9b8e7abddac8a4b5c5145494`, on
2026-10-31. That commit must stay equal to `SPEC_REV` in
`.github/workflows/ci.yml`.

The directory is `tests/vectors/receipt/`, the path both SDKs vendor these
vectors to, so the spec's `scripts/check-sdk-sync.py` can compare this copy
too.

**Gated by `check_dir` in `.github/workflows/ci.yml`'s `conformance-oracle`
job**, in both directions, against the spec repo at `SPEC_REV`, as for
`tests/conformance/`. `crates/macp-core/tests/receipt_vectors.rs` runs every
vector through `macp_core::receipt::verify` (the verifier `macp verify` uses)
and reproduces `receipt_010_wire_match` byte for byte with
`macp_core::receipt::build`.

## Re-vendoring after a `SPEC_REV` bump

```
cp ../multiagentcoordinationprotocol/schemas/conformance/receipt/*.json tests/vectors/receipt/
cargo test -p macp-core --features receipts --test receipt_vectors
```

then update the commit hash and date above.

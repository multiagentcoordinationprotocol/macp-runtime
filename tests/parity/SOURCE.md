# Source

`contract.json` was copied, byte-identical, point-in-time, from:

```
multiagentcoordinationprotocol/schemas/parity/contract.json
```

in the spec repo, commit `18f233229e6cb1156351c68614610a9d3bb40497`, on
2026-09-30. That commit and date record the provenance of this import; this
must stay equal to `SPEC_REV` in `.github/workflows/ci.yml`.

**Gated by `check_dir` in `.github/workflows/ci.yml`'s `conformance-oracle`
job:** this directory is byte-compared, in both directions, against
`spec-repo/schemas/parity/` (the spec repo checked out at `SPEC_REV`) on every
PR. A canonical file missing or different here is `DRIFT`; a file here with no
canonical counterpart is `EXTRA`. Both `README.md` (canonical side) and this
`SOURCE.md` (vendored side) are outside `check_dir`'s `*.json` glob, so
neither is subject to the check.

`tests/parity_contract.rs` also runs directly against the canonical copy in
CI (`MACP_PARITY_CONTRACT` pointed at the spec-repo checkout), so this
vendored copy and canonical cannot silently disagree on content either, not
just on bytes.

## Re-vendoring after a `SPEC_REV` bump

```
cp ../multiagentcoordinationprotocol/schemas/parity/contract.json tests/parity/contract.json
```

then update the commit hash and date above to match the new `SPEC_REV`, and
re-run `cargo test --test parity_contract` before committing.

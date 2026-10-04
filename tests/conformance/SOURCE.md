# Source

The 34 `.json` fixtures in this directory, plus the 6 under `cmt-hash/`, were
copied, byte-identical, point-in-time, from:

```
multiagentcoordinationprotocol/schemas/conformance/
multiagentcoordinationprotocol/schemas/conformance/cmt-hash/
```

in the spec repo, commit `7159afe384f50b67c9c090796d926441eaa9acf4`, on
2026-10-03. That commit and date record the provenance of this import; this
must stay equal to `SPEC_REV` in `.github/workflows/ci.yml`.

**Gated by `check_dir` in `.github/workflows/ci.yml`'s `conformance-oracle`
job:** both directories are byte-compared, in both directions, against
`spec-repo/schemas/conformance/` and `spec-repo/schemas/conformance/cmt-hash/`
(the spec repo checked out at `SPEC_REV`) on every PR. A canonical file
missing or different here is `DRIFT`; a file here with no canonical
counterpart is `EXTRA`. This `SOURCE.md` is outside `check_dir`'s `*.json`
glob, so it is not itself subject to the check.

`tests/conformance_loader.rs` also runs these fixtures directly against this
runtime's live code during `cargo test`, so the vendored copies and the
runtime's actual behavior cannot silently disagree either, not just the bytes
against the canonical source.

## Re-vendoring after a `SPEC_REV` bump

```
cp ../multiagentcoordinationprotocol/schemas/conformance/*.json tests/conformance/
cp ../multiagentcoordinationprotocol/schemas/conformance/cmt-hash/*.json tests/conformance/cmt-hash/
```

then update the commit hash and date above to match the new `SPEC_REV`, and
re-run `cargo test --test conformance_loader` (or `make test-conformance`)
before committing.

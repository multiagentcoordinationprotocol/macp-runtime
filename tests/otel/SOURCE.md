# Source

`macp-semconv.json` (the MACP OpenTelemetry semantic conventions,
`macp-semconv/0.1`) was copied, byte-identical, point-in-time, from:

```
multiagentcoordinationprotocol/schemas/otel/macp-semconv.json
```

in the spec repo, commit `99f3cfa090b4bfb72b3063ca0134ec661436c3ef`, on
2026-10-31. That commit and date record the provenance of this import; this
must stay equal to `SPEC_REV` in `.github/workflows/ci.yml`.

**Gated by `check_dir` in `.github/workflows/ci.yml`'s `conformance-oracle`
job:** this directory is byte-compared, in both directions, against
`spec-repo/schemas/otel/` (the spec repo checked out at `SPEC_REV`) on every
PR. A canonical file missing or different here is `DRIFT`; a file here with no
canonical counterpart is `EXTRA`. This `SOURCE.md` is outside `check_dir`'s
`*.json` glob, so it is not itself subject to the check.

`tests/otel_semconv.rs` loads this file (through the shared loader in
`tests/support/otel_semconv.rs`) and asserts that its span and metric groups
equal the set this runtime implements, so a new or renamed group upstream
fails `cargo test` here, not only the byte check.

## Re-vendoring after a `SPEC_REV` bump

```
cp ../multiagentcoordinationprotocol/schemas/otel/macp-semconv.json tests/otel/macp-semconv.json
```

then update the commit hash and date above to match the new `SPEC_REV`, and
re-run `cargo test --test otel_semconv` before committing.

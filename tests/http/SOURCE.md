# Source

The HTTP/JSON binding artifacts in this directory were copied, byte-identical,
point-in-time, from the spec repo:

| Vendored here | Canonical source |
|---------------|------------------|
| `examples/*.json` (4 files: `errors`, `send`, `stream_session_subscribe`, `watch_signals`) | `multiagentcoordinationprotocol/examples/http/` |
| `invalid-errors/*.json` (10 negative fixtures) | `multiagentcoordinationprotocol/schemas/json/tests/invalid-http-errors/` |
| `macp-http-error.schema.json` | `multiagentcoordinationprotocol/schemas/json/macp-http-error.schema.json` |

in the spec repo, commit `99f3cfa090b4bfb72b3063ca0134ec661436c3ef`, on
2026-10-31. That commit and date record the provenance of this import; this
must stay equal to `SPEC_REV` in `.github/workflows/ci.yml`.

The examples are informative worked exchanges for RFC-MACP-0006 §4 (the
HTTP/JSON binding). The schema and its negative fixtures pin the §4.5
request-level error body `{"error": MACPError}`; every file under
`invalid-errors/` must be **rejected** by the schema (see the canonical
directory's `README.md`, which is not vendored).

**Gated in `.github/workflows/ci.yml`'s `conformance-oracle` job:**
`examples/` and `invalid-errors/` by `check_dir` (byte-compared in both
directions, so a canonical file missing or different here is `MISSING` or
`DRIFT`, and a file here with no canonical counterpart is `EXTRA`), and
`macp-http-error.schema.json` by `check_file` (its canonical directory,
`schemas/json/`, holds many unrelated files, so `check_dir` cannot be used).
This `SOURCE.md` is outside both checks.

## Re-vendoring after a `SPEC_REV` bump

Copy from a clean export of the pinned commit, never from a working tree:

```
S=$(mktemp -d)
git -C ../multiagentcoordinationprotocol archive "$SPEC_REV" | tar -x -C "$S"
cp "$S"/examples/http/*.json tests/http/examples/
cp "$S"/schemas/json/tests/invalid-http-errors/*.json tests/http/invalid-errors/
cp "$S"/schemas/json/macp-http-error.schema.json tests/http/
```

then update the commit hash and date above to match the new `SPEC_REV`.

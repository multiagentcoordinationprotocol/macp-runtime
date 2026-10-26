# macp-cli

`macp`, the MACP command line.

```sh
cargo install macp-cli
```

Release binaries for macOS (arm64, x86_64) and Linux (x86_64, arm64) are
attached to each [GitHub release](https://github.com/multiagentcoordinationprotocol/macp-runtime/releases).

| Command | What it does |
|---|---|
| `macp up [--attach] [--env-file <f>]` | Runs the [self-host stack](../../deploy/README.md) pinned in this release. No clone needed. |
| `macp down [--volumes]` | Stops it. `--volumes` also deletes every recorded run. |
| `macp status` | Lists its containers. |
| `macp verify <receipt.json> [--key …]… [--require attested\|integrity] [--json]` | Verifies a decision receipt (RFC-MACP-0013 Appendix A.4). |
| `macp session tail <id> [--after N] [--no-follow] [--json]` | Prints a session's accepted envelopes, then follows it until it ends. |
| `macp conform …` | Runs the spec's conformance fixtures against an implementation. It takes the same arguments as [`macp-conform`](../macp-conform/README.md). |

## Exit status

Every command uses the same exit codes:

| Code | Meaning |
|---|---|
| 0 | It succeeded, or its check passed. |
| 1 | The check ran and failed. Examples: a receipt below `--require`, a failed conformance fixture, a stack that did not come up healthy. |
| 2 | It could not run. Examples: bad usage, unreadable input, or Docker missing. |

## `macp up`

The compose file and `.env.example` from the repo's `deploy/` are compiled into
the binary. `macp up` writes them to `~/.macp/deploy/<version>/` (or
`$MACP_HOME/deploy/<version>/`) and runs `docker compose up -d --wait`. To
change credentials or ports, put a `.env` in that directory or pass
`--env-file`.

## `macp session tail`

```sh
macp session tail 26819a42-8fc4-4390-bb0b-e075732fd1e0
macp session tail $SID --target grpcs://runtime.example.com --token "$TOKEN" --json
```

`tail` subscribes to the session as a passive `StreamSession` reader
(RFC-MACP-0006 §3.2). The runtime replays the accepted history and then sends
live traffic. Each line shows the envelope's accepted ordinal, its timestamp,
sender, message type and id, and the decoded payload when the mode is one this
release knows:

```text
#4    2026-10-26T02:51:44.257Z  bob              Vote             m4  VotePayload { proposal_id: "p1", vote: "APPROVE", reason: "approved for release" }
```

- The ordinal is 1-based. `--after N` skips the first N. `WhatIfReplay`'s
  `history_index` is the ordinal minus one.
- `--json` prints one object per line, with the payload as `payload_base64`.
- `tail` exits 0 once the session is `Resolved`, `Expired` or `Cancelled`.
  `--no-follow` exits as soon as the history has been printed.
- `--target` (env `MACP_TARGET`) defaults to `grpc://127.0.0.1:50051`.
  `--token` (env `MACP_TOKEN`) defaults to the self-host stack's observer
  token, so `macp up` then `macp session tail` works as is. Any token that can
  read the session works: an observer, the initiator or a participant.
- An unknown session, a refused token or a lost connection exits 2.

## `macp verify`

```sh
macp verify receipt.json --key example-1=TDVzkxaSf6_Oxu7mmn5TbCkF80s8Ulh33qKQsi0gnZM
macp verify receipt.json --key issuer-jwks.json --json
```

`macp verify` reports integrity, the outcome of each attestation, and an
overall verdict.

- **Integrity** is *match*, *mismatch* or *cannot_verify*. It needs no keys:
  `commitment` is re-hashed and compared with `commitment_hash`. If
  `commitment_wire_b64` is present, it is checked as well.
- **Each attestation** is *valid*, *invalid*, *unknown_key* or *unsupported*.
  Only EdDSA `jws` attestations from a `--key` you trust can be *valid*.
  `--key` takes either a JWK/JWKS file or `<kid>=<x>`, and can be repeated.
- **The verdict** is one of:
  - *attested*: integrity matches and at least one attestation is valid.
  - *integrity_only*: integrity matches but no attestation is valid.
  - *unverified*: integrity does not match.
  - *refused*: the receipt format is unknown.

By default only *attested* exits 0. `--require integrity` also accepts
*integrity_only*. The verifier passes all 12 of the spec's reference vectors.

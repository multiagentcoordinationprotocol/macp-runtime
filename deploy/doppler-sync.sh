#!/usr/bin/env bash
#
# Push this service's configuration from Doppler into Railway.
#
#   deploy/doppler-sync.sh            # dry run: key names and changed/unchanged, nothing changes
#   deploy/doppler-sync.sh --apply    # set the changed keys in Railway, then redeploy if any changed
#
# Doppler (project macp-runtime, config prd) is the source of truth for
# the keys listed in deploy/doppler-keys.txt, and only those. The script sets
# and updates them; it never deletes a Railway variable, never touches a key
# outside the list, and never touches a Railway variable reference
# (${{service.VAR}}) or a RAILWAY_* variable: those stay Railway-managed.
#
# Values are piped from Doppler into Railway and are never printed or written
# to disk. Railway holds no Doppler token.
#
# Needs git, jq, curl and the doppler and railway CLIs, logged in. Refuses to
# run on a dirty working tree, so what runs is what is committed.

# Railway GraphQL queries are single-quoted on purpose.
# shellcheck disable=SC2016
set -euo pipefail

RAILWAY_PROJECT_ID=a8928fc3-53ec-447e-8d34-7ed8d464a705 # Railway: macp-scenario-runner
RAILWAY_SERVICE=macp-runtime
RAILWAY_ENVIRONMENT=production
DOPPLER_PROJECT=macp-runtime
DOPPLER_CONFIG=prd

mode=dry-run
while [ $# -gt 0 ]; do
  case "$1" in
    --dry-run) mode=dry-run ;;
    --apply) mode=apply ;;
    -h|--help) sed -n '2,18p' "$0"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
  shift
done

say() { printf '%s\n' "$*" >&2; }
die() { say "doppler-sync: $*"; exit 1; }

for c in git jq curl doppler railway; do
  command -v "$c" >/dev/null || die "needs $c on PATH"
done
repo="$(git rev-parse --show-toplevel)"
[ -z "$(git -C "$repo" status --porcelain)" ] || die "working tree is dirty; commit or stash first"
doppler me >/dev/null 2>&1 || die "doppler is not logged in (doppler login)"
railway whoami >/dev/null 2>&1 || die "railway is not logged in (railway login)"

keys_file="$repo/deploy/doppler-keys.txt"
[ -f "$keys_file" ] || die "missing $keys_file"
keys="$(sed -e 's/#.*//' -e 's/[[:space:]]//g' "$keys_file" | grep -v '^$' || true)"
[ -n "$keys" ] || die "no keys in $keys_file"
if grep -q '^RAILWAY_' <<<"$keys"; then die "RAILWAY_* keys are Railway-managed; remove them from $keys_file"; fi

# Resolved once, in this shell, so a missing token stops the run here.
railway_token="${RAILWAY_API_TOKEN:-}"
if [ -z "$railway_token" ] && [ -f "$HOME/.railway/config.json" ]; then
  railway_token="$(jq -r '.user.accessToken // .user.token // empty' "$HOME/.railway/config.json")" || railway_token=""
fi
[ -n "$railway_token" ] || die "no Railway token (set RAILWAY_API_TOKEN or run railway login)"
# Prints the response's .data. The token reaches curl as a header file
# (printf is a builtin, so it is on no process's argv and never in ps); the
# request body goes on stdin. Fails on a non-2xx, a network error, an
# unparsable body, or a GraphQL error (Railway reports those with HTTP 200
# and null data, which would otherwise read as "no variables").
railway_api() {
  jq -nc --arg q "$1" --argjson v "${2:-null}" '{query: $q, variables: ($v // {})}' |
    curl -fsS https://backboard.railway.com/graphql/v2 \
      -H @<(printf 'Authorization: Bearer %s\n' "$railway_token") -H 'content-type: application/json' \
      --data-binary @- |
    jq -ce 'if (.errors // []) | length > 0 then error("Railway GraphQL returned errors") else .data end'
}

ids="$(railway_api 'query($p: String!){project(id: $p){environments{edges{node{id name}}} services{edges{node{id name}}}}}' \
  "$(jq -nc --arg p "$RAILWAY_PROJECT_ID" '{p: $p}')" |
  jq -c --arg e "$RAILWAY_ENVIRONMENT" --arg s "$RAILWAY_SERVICE" '.project |
    {e: (.environments.edges[].node | select(.name == $e) | .id),
     s: (.services.edges[].node | select(.name == $s) | .id)}')" ||
  die "reading Railway project $RAILWAY_PROJECT_ID failed; nothing changed"
[ -n "$ids" ] || die "no service $RAILWAY_SERVICE in environment $RAILWAY_ENVIRONMENT"

# Railway's variables as stored (unrendered), so a reference reads as ${{...}}.
# Fails unless the read succeeds and yields an object.
railway_vars() {
  railway_api 'query($p: String!, $e: String!, $s: String!){variables(projectId: $p, environmentId: $e, serviceId: $s, unrendered: true)}' \
    "$(jq -c --arg p "$RAILWAY_PROJECT_ID" '{p: $p, e: .e, s: .s}' <<<"$ids")" |
    jq -ce '.variables | if type == "object" then . else error("no variables object") end'
}
doppler_vars() {
  doppler secrets download --no-file --format json --project "$DOPPLER_PROJECT" --config "$DOPPLER_CONFIG"
}

# One line per key: "<KEY> <state>". Both sides are read into shell variables
# first, each with its own check, so a failed read stops the run instead of
# looking like "no entry" (set -e does not see a failure inside a process
# substitution). Values reach jq only through printf pipes, never argv or disk.
plan() {
  local d r
  d="$(doppler_vars)" || die "reading Doppler $DOPPLER_PROJECT/$DOPPLER_CONFIG failed; nothing changed"
  r="$(railway_vars)" || die "reading Railway variables failed; nothing changed"
  printf '%s' "$d" | jq -e 'type == "object"' >/dev/null 2>&1 || die "unexpected Doppler response; nothing changed"
  jq -nr --slurpfile d <(printf '%s' "$d") --slurpfile r <(printf '%s' "$r") --arg keys "$keys" '
    $d[0] as $d | $r[0] as $r |
    $keys | split("\n")[] | . as $k |
    if ($d | has($k) | not) then "\($k) missing-in-doppler"
    elif ($r[$k] // "" | contains("${{")) then "\($k) railway-reference"
    elif ($r | has($k) | not) then "\($k) new"
    elif $r[$k] == $d[$k] then "\($k) unchanged"
    else "\($k) changed" end'
}

result="$(plan)"
say "Doppler $DOPPLER_PROJECT/$DOPPLER_CONFIG -> Railway $RAILWAY_SERVICE ($RAILWAY_ENVIRONMENT):"
while read -r k state; do say "  $k: $state"; done <<<"$result"
grep -q ' missing-in-doppler$' <<<"$result" && die "listed keys are missing from Doppler; nothing changed"
grep -q ' railway-reference$' <<<"$result" && die "listed keys are Railway references; take them out of $keys_file"
todo="$(awk '$2 == "changed" || $2 == "new" {print $1}' <<<"$result")"

if [ -z "$todo" ]; then say "in sync; nothing to do"; exit 0; fi
if [ "$mode" = dry-run ]; then say "dry run: $(wc -l <<<"$todo" | tr -d ' ') key(s) would change; --apply to push"; exit 0; fi

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
(cd "$work" && railway link --project "$RAILWAY_PROJECT_ID" --service "$RAILWAY_SERVICE" --environment "$RAILWAY_ENVIRONMENT" >/dev/null)
while read -r k; do
  say "setting $k"
  doppler_vars | jq -j --arg k "$k" '.[$k]' |
    (cd "$work" && railway variable set "$k" --stdin --service "$RAILWAY_SERVICE" --environment "$RAILWAY_ENVIRONMENT" --skip-deploys >/dev/null)
done <<<"$todo"

after="$(plan)"
if grep -qv ' unchanged$' <<<"$after"; then
  while read -r k state; do say "  $k: $state"; done <<<"$after"
  die "Railway does not match Doppler after --apply; not redeploying"
fi
say "variables match Doppler; redeploying $RAILWAY_SERVICE"
(cd "$work" && railway redeploy --service "$RAILWAY_SERVICE" --yes >/dev/null)

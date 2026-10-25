#!/usr/bin/env bash
# Acceptance test for the self-host stack: bring it up from nothing, launch the
# console's showcase run (a deterministic Decision) through the console's own
# proxy, wait for the control plane to report it Verified, tear down.
#
# Going through the console exercises its proxy and its wiring to the
# playground and control plane. The proxy rate-limits launches (5 per 10
# minutes per client), which one launch per run stays well under.
#
#   deploy/scripts/acceptance.sh            # leaves nothing behind
#   KEEP_STACK=1 deploy/scripts/acceptance.sh
#
# Needs docker compose v2, curl and jq. Reads the console port from
# MACP_CONSOLE_PORT if set; the console holds the control-plane key itself.
set -euo pipefail
cd "$(dirname "$0")/.."

CONSOLE="http://127.0.0.1:${MACP_CONSOLE_PORT:-3000}"
PLAYGROUND="$CONSOLE/api/proxy/macp-playground"
CP="$CONSOLE/api/proxy/macp-control-plane"
VERIFY_TIMEOUT_SECS="${VERIFY_TIMEOUT_SECS:-120}"

teardown() {
  local rc=$?
  if [ "$rc" -ne 0 ]; then
    docker compose ps -a || true
    docker compose logs --no-color --tail=200 || true
  fi
  [ -n "${KEEP_STACK:-}" ] || docker compose down -v --remove-orphans >/dev/null 2>&1 || true
  exit "$rc"
}
trap teardown EXIT

t0=$(date +%s)
docker compose up -d --wait --wait-timeout 300
echo "stack healthy after $(( $(date +%s) - t0 ))s"

curl -fsS -o /dev/null "$CONSOLE/" || { echo "console not answering at $CONSOLE" >&2; exit 1; }

# The console's /showcase launch, byte for byte (macp-ui-console app/showcase).
launch=$(curl -fsS -X POST "$PLAYGROUND/examples/run" \
  -H 'content-type: application/json' \
  -d '{"scenarioRef":"fraud/high-value-new-device@1.0.0","templateId":"showcase","mode":"live","inputs":{},"bootstrapAgents":true}')
execution=$(jq -r '.compiled.scenarioMeta.agentExecution // empty' <<<"$launch")
run_id=$(jq -r '.controlPlaneRun.runId // empty' <<<"$launch")
if [ "$execution" != deterministic ]; then
  echo "launch was not deterministic (agentExecution=${execution:-unset})" >&2
  exit 1
fi
if [ -z "$run_id" ]; then
  echo "playground did not register the run with the control plane (controlPlaneRun is null):" >&2
  jq -c '{sessionId, controlPlaneRun}' <<<"$launch" >&2
  exit 1
fi
echo "launched run $run_id (session $(jq -r .sessionId <<<"$launch"))"

deadline=$(( $(date +%s) + VERIFY_TIMEOUT_SECS ))
while :; do
  verification=$(curl -fsS "$CP/runs/$run_id/verification")
  status=$(jq -r .status <<<"$verification")
  case "$status" in
    verified) break ;;
    pending) ;;
    *) echo "verification status: $status" >&2; jq . <<<"$verification" >&2; exit 1 ;;
  esac
  if [ "$(date +%s)" -ge "$deadline" ]; then
    echo "still pending after ${VERIFY_TIMEOUT_SECS}s" >&2
    curl -fsS "$CP/runs/$run_id" | jq . >&2 || true
    exit 1
  fi
  sleep 1
done

echo "Verified: $(jq -r .commitmentHash <<<"$verification")"
echo "total $(( $(date +%s) - t0 ))s from up to Verified"

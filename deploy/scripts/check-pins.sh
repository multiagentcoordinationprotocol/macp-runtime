#!/usr/bin/env bash
# Every image in deploy/docker-compose.yml must be pinned by version: MACP
# images to an X.Y.Z or vX.Y.Z release tag, third-party images (Postgres, which
# versions as MAJOR.MINOR) to a numeric version. Never `latest`, `sha-*` or
# untagged. Reads the resolved config, so the
# `${VAR:-default}` defaults are what is checked (run it with no overrides).
set -euo pipefail
cd "$(dirname "$0")/.."

status=0
while read -r service image; do
  tag="${image##*:}"
  pattern=
  if [ "$tag" = "$image" ] || [[ "$tag" == */* ]]; then
    echo "UNPINNED: $service uses $image (no tag)" >&2
    status=1
  elif [[ "$image" == ghcr.io/multiagentcoordinationprotocol/* ]]; then
    pattern='^v?[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?$'
  else
    pattern='^[0-9]+(\.[0-9]+)+(-[0-9A-Za-z.]+)?$'
  fi
  if [ -n "${pattern:-}" ] && ! [[ "$tag" =~ $pattern ]]; then
    echo "NOT SEMVER: $service uses $image" >&2
    status=1
  elif [ -n "$pattern" ]; then
    echo "ok: $service $image"
  fi
done < <(docker compose config --format json | jq -r '.services | to_entries[] | "\(.key) \(.value.image)"')
exit $status

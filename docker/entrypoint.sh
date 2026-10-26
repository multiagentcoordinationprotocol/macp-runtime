#!/bin/sh
# Started as root (the image default), hand MACP_DATA_DIR to `macp` and drop
# privileges before the runtime starts. Platform volumes and bind mounts arrive
# root-owned, and a named volume's ownership is only inherited when it is new.
# Started as any other user (`--user`, Kubernetes `runAsUser`), run as is.
set -eu

if [ "$(id -u)" = 0 ]; then
    dir="${MACP_DATA_DIR:-/home/macp/.macp-data}"
    mkdir -p "$dir"
    # Recursive only when the directory is not already `macp`'s, so a large
    # data dir is not walked on every start.
    if [ "$(stat -c %u "$dir")" != "$(id -u macp)" ]; then
        chown -R macp:macp "$dir"
    fi
    exec setpriv --reuid=macp --regid=macp --init-groups -- macp-runtime "$@"
fi

exec macp-runtime "$@"

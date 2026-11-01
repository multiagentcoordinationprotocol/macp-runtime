#!/usr/bin/env bash
# Run every curl example of docs/examples.md's "HTTP/JSON binding (curl)"
# section against a local runtime, so the documentation cannot drift from the
# binding (plans/http-listener.md P11).
#
# The examples are the source of truth: each fenced ```bash block preceded by
# a `<!-- check-http-example ... -->` marker is extracted and executed, in
# document order, in this shell (so `export`s and variables carry over).
# Marker words:
#   setup        run with no checks (exports, ids)
#   status=N     the expected HTTP status of the block's request
#   expect=S     a string the output must contain (`%20` stands for a space)
#   stream       run in the background (an SSE stream with no natural end);
#                passes once its status is right and `expect=` has arrived
# A curl block in the section without a marker is an error, so an example
# cannot be added without being checked.
#
# `curl` is shadowed by a function that adds `--max-time` and `-D <file>`
# (to read the status) and copies the output to a file; the examples are run
# exactly as written otherwise. The runtime binds 127.0.0.1:0 for both
# listeners; the HTTP port is read from its startup log line.
#
# Usage: scripts/check-http-examples.sh
#   MACP_RUNTIME_BIN  runtime binary (default target/debug/macp-runtime), built
#                     with `cargo build -p macp-runtime --features http-listener`
#   MACP_EXAMPLES_DOC the document (default docs/examples.md)
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
doc=${MACP_EXAMPLES_DOC:-$root/docs/examples.md}
bin=${MACP_RUNTIME_BIN:-$root/target/debug/macp-runtime}
section='## HTTP/JSON binding (curl)'
deadline_secs=30

die() {
  echo "check-http-examples: FAIL: $*" >&2
  exit 1
}

[[ -f $doc ]] || die "no document at $doc"
[[ -x $bin ]] || die "no runtime binary at $bin (cargo build -p macp-runtime --features http-listener)"
command -v python3 >/dev/null || die "python3 is required by the examples' setup block"

work=$(mktemp -d)
runtime_pid=
stream_pids=()

kill_tree() {
  local child
  for child in $(pgrep -P "$1" 2>/dev/null || true); do
    kill_tree "$child"
  done
  kill "$1" 2>/dev/null || true
}

cleanup() {
  local pid
  for pid in "${stream_pids[@]+"${stream_pids[@]}"}"; do
    kill_tree "$pid"
  done
  if [[ -n $runtime_pid ]]; then
    kill "$runtime_pid" 2>/dev/null || true
    wait "$runtime_pid" 2>/dev/null || true
  fi
  rm -rf "$work"
}
trap cleanup EXIT

# ── 1. Extract the marked blocks ────────────────────────────────────────────
awk -v section="$section" -v out="$work" '
  $0 == section { in_section = 1; found = 1; next }
  in_section && /^## / { in_section = 0 }
  !in_section { next }
  pending && !/^```bash$/ {
    printf "line %d: marker not directly followed by a ```bash block\n", NR > "/dev/stderr"
    bad = 1; pending = 0
  }
  /^<!-- check-http-example/ {
    meta = $0
    sub(/^<!-- check-http-example */, "", meta)
    sub(/ *-->$/, "", meta)
    pending = 1
    next
  }
  /^```bash$/ {
    if (pending) {
      n++
      base = sprintf("%s/%02d", out, n)
      print meta > (base ".meta"); close(base ".meta")
      block = base ".sh"; mode = 1; pending = 0
    } else {
      start = NR; text = ""; mode = 2
    }
    next
  }
  /^```$/ && mode {
    if (mode == 1) close(block)
    if (mode == 2 && text ~ /(^|[^a-z])curl /) {
      printf "line %d: curl block without a check-http-example marker\n", start > "/dev/stderr"
      bad = 1
    }
    mode = 0
    next
  }
  mode == 1 { print > block; next }
  mode == 2 { text = text $0 "\n"; next }
  END {
    if (!found) { print "section not found" > "/dev/stderr"; bad = 1 }
    else if (!n) { print "no marked examples" > "/dev/stderr"; bad = 1 }
    exit bad
  }
' "$doc" || die "cannot extract the examples from $doc"

# ── 2. Start the runtime (dev-mode auth, memory only, ephemeral ports) ─────
log=$work/runtime.log
MACP_ALLOW_INSECURE=1 MACP_MEMORY_ONLY=1 \
  MACP_BIND_ADDR=127.0.0.1:0 MACP_HTTP_BIND_ADDR=127.0.0.1:0 \
  env -u MACP_AUTH_ISSUER -u MACP_AUTH_TOKENS_FILE -u MACP_AUTH_TOKENS_JSON \
  -u MACP_TLS_CERT_PATH -u MACP_TLS_KEY_PATH -u MACP_HTTP_PUBLIC_URL \
  -u MACP_HTTP_CORS_ORIGINS -u MACP_POLICIES_DIR -u MACP_METRICS_ADDR \
  "$bin" >"$log" 2>&1 &
runtime_pid=$!

addr=
end=$((SECONDS + deadline_secs))
while ((SECONDS < end)); do
  kill -0 "$runtime_pid" 2>/dev/null || { cat "$log" >&2; die "the runtime exited during startup"; }
  addr=$(grep 'HTTP listener bound' "$log" | grep -oE '127\.0\.0\.1:[0-9]+' | head -n1 || true)
  [[ -n $addr ]] && break
  sleep 0.1
done
[[ -n $addr ]] || { cat "$log" >&2; die "no 'HTTP listener bound' line within ${deadline_secs}s (built without --features http-listener?)"; }

until command curl -fsS --max-time 2 -o /dev/null -X POST -H 'Content-Type: application/json' \
  -d '{}' "http://$addr/macp/v1/GetManifest"; do
  ((SECONDS < end)) || { cat "$log" >&2; die "the HTTP listener on $addr never answered GetManifest"; }
  sleep 0.1
done
export MACP_HTTP="http://$addr"

# ── 3. Run the examples ─────────────────────────────────────────────────────
ex_max_time=15
ex_headers=/dev/null
ex_output=/dev/null
# Shadows curl for the examples only (the readiness probe above uses
# `command curl`).
curl() {
  command curl --max-time "$ex_max_time" -D "$ex_headers" "$@" | tee -a "$ex_output"
}

status_of() {
  awk '/^HTTP\//{s = $2} END{print s}' "$1"
}

label_of() {
  grep -oE '/macp/v1/[A-Za-z]+|/\.well-known/macp\.json' "$1" | head -n1 || true
}

fail_example() {
  local base=$1 why=$2
  {
    echo "--- example $(basename "$base") ($(label_of "$base.sh")): $why"
    echo "--- block:"; cat "$base.sh"
    echo "--- response headers:"; cat "$base.headers" 2>/dev/null || true
    echo "--- output:"; cat "$base.out" 2>/dev/null || true
    echo "--- stderr:"; cat "$base.err" 2>/dev/null || true
    echo "--- runtime log (tail):"; tail -n 20 "$log"
  } >&2
  die "example $(basename "$base") $why"
}

check_status() {
  local base=$1 want=$2 got
  got=$(status_of "$base.headers")
  [[ $got == "$want" ]] || fail_example "$base" "returned HTTP ${got:-<none>}, expected $want"
}

# Pending streams: parallel arrays of base, expected status and expect string.
s_base=()
s_status=()
s_expect=()
s_done=()

poll_streams() {
  local i
  for i in "${!s_base[@]}"; do
    [[ ${s_done[$i]} == 1 ]] && continue
    if grep -qF -- "${s_expect[$i]}" "${s_base[$i]}.out" 2>/dev/null; then
      check_status "${s_base[$i]}" "${s_status[$i]}"
      kill_tree "${stream_pids[$i]}"
      s_done[i]=1
      passed=$((passed + 1))
      echo "ok   $(basename "${s_base[$i]}") stream $(label_of "${s_base[$i]}.sh") HTTP ${s_status[$i]}"
    fi
  done
}

passed=0
for meta in "$work"/*.meta; do
  base=${meta%.meta}
  code=$(<"$base.sh")
  read -r -a words <"$meta"
  setup=0 stream=0 want='' expect=''
  for w in "${words[@]+"${words[@]}"}"; do
    case $w in
      setup) setup=1 ;;
      stream) stream=1 ;;
      status=*) want=${w#status=} ;;
      expect=*) expect=${w#expect=}; expect=${expect//%20/ } ;;
      *) die "example $(basename "$base"): unknown marker word '$w'" ;;
    esac
  done
  if ((setup)); then
    eval "$code" || fail_example "$base" "setup block failed"
    echo "ok   $(basename "$base") setup"
    continue
  fi
  [[ -n $want ]] || die "example $(basename "$base"): marker has no status="
  ex_headers=$base.headers
  ex_output=$base.out
  : >"$ex_headers"
  : >"$ex_output"
  if ((stream)); then
    [[ -n $expect ]] || die "example $(basename "$base"): a stream needs expect="
    ex_max_time=$((deadline_secs * 2))
    (eval "$code") >/dev/null 2>"$base.err" &
    stream_pids+=("$!")
    s_base+=("$base") s_status+=("$want") s_expect+=("$expect") s_done+=(0)
    # Wait until the response head is in: the stream is then open, so what
    # the next examples send reaches it.
    end=$((SECONDS + deadline_secs))
    until [[ -n $(status_of "$ex_headers") ]]; do
      ((SECONDS < end)) || fail_example "$base" "never sent a response head"
      sleep 0.1
    done
    check_status "$base" "$want"
    continue
  fi
  ex_max_time=15
  rc=0
  eval "$code" >/dev/null 2>"$base.err" || rc=$?
  ((rc == 0)) || fail_example "$base" "exited with status $rc"
  check_status "$base" "$want"
  if [[ -n $expect ]] && ! grep -qF -- "$expect" "$base.out"; then
    fail_example "$base" "output does not contain '$expect'"
  fi
  passed=$((passed + 1))
  echo "ok   $(basename "$base") $(label_of "$base.sh") HTTP $want"
  poll_streams
done

end=$((SECONDS + deadline_secs))
while :; do
  poll_streams
  pending=0
  for i in "${!s_done[@]}"; do
    [[ ${s_done[$i]} == 1 ]] || pending=$((pending + 1))
  done
  ((pending == 0)) && break
  if ((SECONDS >= end)); then
    for i in "${!s_done[@]}"; do
      [[ ${s_done[$i]} == 1 ]] || fail_example "${s_base[$i]}" "stream output never contained '${s_expect[$i]}'"
    done
  fi
  sleep 0.1
done

echo "check-http-examples: PASS: $passed examples from $(basename "$doc") against $bin ($MACP_HTTP)"

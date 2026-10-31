#!/usr/bin/env bash
# Measure what `--features otel` costs in the container image (R2 P6,
# plans/otel-spans.md). Dev-only; excluded from the published crate by
# Cargo.toml's `exclude = ["/scripts", ...]`.
#
# Builds `macp:plain` and `macp:otel` from the repo Dockerfile on the local
# Docker host (native architecture; set PLATFORM=linux/amd64 to force one,
# e.g. under emulation), then prints:
#   1. image size and binary size;
#   2. startup-to-SERVING (median and p90 over RUNS runs) for the plain image,
#      the otel image with no OTEL vars, and the otel image pointed at a
#      blackhole collector (accepts TCP, never answers);
#   3. idle RSS after 10 s (`docker stats --no-stream`);
#   4. behaviour equivalence with no OTEL vars: a scripted Decision session
#      (SessionStart, Proposal, Commitment) against each image; the gRPC
#      responses (minus `accepted_at_unix_ms`) and the log lines (minus
#      timestamps) must be identical.
#
# Startup is timed on the host from just before `docker run -d` to the first
# `grpc.health.v1.Health/Check` answering SERVING, polled with grpcurl every
# 10 ms (plus grpcurl's own per-call cost, the same for every image).
#
# Requires: docker (buildx), grpcurl, python3, cargo (only to locate the
# health.proto and macp protos in the local registry).
#
# Usage: scripts/measure-otel-image.sh [--skip-build]
#   RUNS=20 PORT=50061 PLATFORM=<platform> scripts/measure-otel-image.sh
set -euo pipefail

cd "$(dirname "$0")/.."

RUNS="${RUNS:-20}"
PORT="${PORT:-50061}"
PLATFORM="${PLATFORM:-}"
SKIP_BUILD=0
[ "${1:-}" = "--skip-build" ] && SKIP_BUILD=1

for tool in docker grpcurl python3 cargo; do
    command -v "$tool" >/dev/null || { echo "missing required tool: $tool" >&2; exit 2; }
done

# Proto import paths for grpcurl, from the locked dependency graph.
read -r MACP_PROTO_DIR HEALTH_PROTO_DIR < <(
    cargo metadata --format-version 1 --locked 2>/dev/null | python3 -c '
import json, os, sys
pkgs = {p["name"]: p for p in json.load(sys.stdin)["packages"]}
def d(name):
    return os.path.join(os.path.dirname(pkgs[name]["manifest_path"]), "proto")
print(d("macp-proto"), d("tonic-health"))
'
)

platform_args=()
[ -n "$PLATFORM" ] && platform_args=(--platform "$PLATFORM")

if [ "$SKIP_BUILD" = 0 ]; then
    echo "== building macp:plain and macp:otel ${PLATFORM:-(native)}"
    docker buildx build ${platform_args[@]+"${platform_args[@]}"} --load -t macp:plain .
    docker buildx build ${platform_args[@]+"${platform_args[@]}"} --load --build-arg CARGO_FEATURES=otel -t macp:otel .
fi

export RUNS PORT MACP_PROTO_DIR HEALTH_PROTO_DIR
exec python3 -I - ${platform_args[@]+"${platform_args[@]}"} <<'PY'
import base64, json, os, re, socket, statistics, subprocess, sys, threading, time

RUNS = int(os.environ["RUNS"])
PORT = int(os.environ["PORT"])
MACP = os.environ["MACP_PROTO_DIR"]
HEALTH = os.environ["HEALTH_PROTO_DIR"]
PLATFORM = sys.argv[1:]  # [] or ["--platform", "..."]
NAME = "macp-measure"
BASE_ENV = ["-e", "MACP_ALLOW_INSECURE=1", "-e", "MACP_MEMORY_ONLY=1"]


def sh(*args, check=True, capture=True):
    return subprocess.run(args, check=check, text=True,
                          stdout=subprocess.PIPE if capture else None,
                          stderr=subprocess.PIPE if capture else None)


def rm():
    sh("docker", "rm", "-f", NAME, check=False)


def run(image, extra=()):
    rm()
    sh("docker", "run", "-d", "--name", NAME, *PLATFORM,
       "--add-host", "host.docker.internal:host-gateway",
       "-p", f"127.0.0.1:{PORT}:50051", *BASE_ENV, *extra, image)


def serving():
    r = sh("grpcurl", "-plaintext", "-connect-timeout", "1",
           "-import-path", HEALTH, "-proto", "health.proto",
           f"127.0.0.1:{PORT}", "grpc.health.v1.Health/Check", check=False)
    return r.returncode == 0 and '"SERVING"' in r.stdout


def startup_ms(image, extra=()):
    t0 = time.monotonic()
    run(image, extra)
    deadline = t0 + 60
    while time.monotonic() < deadline:
        if serving():
            ms = (time.monotonic() - t0) * 1000
            rm()
            return ms
        time.sleep(0.01)
    logs = sh("docker", "logs", NAME, check=False)
    rm()
    sys.exit(f"{image} never reported SERVING:\n{logs.stdout}{logs.stderr}")


def p90(xs):
    xs = sorted(xs)
    return xs[max(0, int(round(0.9 * len(xs))) - 1)]


# A blackhole collector on the host: accepts TCP, never answers.
bh = socket.socket()
bh.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
bh.bind(("0.0.0.0", 0))
bh.listen(64)
BH_PORT = bh.getsockname()[1]
held = []
threading.Thread(target=lambda: [held.append(bh.accept()) for _ in iter(int, 1)],
                 daemon=True).start()
BLACKHOLE = ["-e", f"OTEL_EXPORTER_OTLP_ENDPOINT=http://host.docker.internal:{BH_PORT}"]

out = {}

# 1. sizes
for tag in ("plain", "otel"):
    img = f"macp:{tag}"
    size = int(sh("docker", "image", "inspect", "-f", "{{.Size}}", img).stdout)
    binsize = int(sh("docker", "run", "--rm", *PLATFORM, "--entrypoint", "stat", img,
                     "-c", "%s", "/usr/local/bin/macp-runtime").stdout)
    arch = sh("docker", "image", "inspect", "-f", "{{.Os}}/{{.Architecture}}", img).stdout.strip()
    # `.Size` is the content size under the containerd image store and the
    # unpacked size under the classic store; `docker image ls` shows the
    # unpacked size either way, printed for reference.
    listed = sh("docker", "image", "ls", img, "--format", "{{.Size}}").stdout.strip()
    out[tag] = {"image_bytes": size, "binary_bytes": binsize, "platform": arch,
                "listed": listed}

# 2. startup
configs = [("plain", "macp:plain", []), ("otel", "macp:otel", []),
           ("otel+blackhole", "macp:otel", BLACKHOLE)]
startup = {}
for label, img, extra in configs:
    startup_ms(img, extra)  # warm-up, not counted
    xs = [startup_ms(img, extra) for _ in range(RUNS)]
    startup[label] = {"median_ms": statistics.median(xs), "p90_ms": p90(xs)}

# 3. idle RSS
MIB = {"B": 1 / 2**20, "KiB": 1 / 1024, "MiB": 1, "GiB": 1024,
       "kB": 1e3 / 2**20, "MB": 1e6 / 2**20, "GB": 1e9 / 2**20}


def idle_rss_mib(img, extra=()):
    """(docker stats cgroup usage, the process's VmRSS), both in MiB."""
    run(img, extra)
    time.sleep(10)
    usage = sh("docker", "stats", "--no-stream", "--format", "{{.MemUsage}}", NAME).stdout
    # The entrypoint execs the runtime, so it is PID 1.
    status = sh("docker", "exec", NAME, "cat", "/proc/1/status").stdout
    rm()
    m = re.match(r"\s*([\d.]+)\s*([A-Za-z]+)", usage)
    vm = re.search(r"VmRSS:\s*(\d+) kB", status)
    return float(m.group(1)) * MIB[m.group(2)], int(vm.group(1)) / 1024


rss_both = {label: idle_rss_mib(img, extra) for label, img, extra in configs}
rss = {label: v[0] for label, v in rss_both.items()}
vmrss = {label: v[1] for label, v in rss_both.items()}

# 4. behaviour equivalence (no OTEL vars on either image)
def field(num, wire, data):
    key = bytes([num << 3 | wire])
    if wire == 2:
        return key + varint(len(data)) + data
    return key + varint(data)


def varint(n):
    b = b""
    while True:
        byte = n & 0x7F
        n >>= 7
        if n:
            b += bytes([byte | 0x80])
        else:
            return b + bytes([byte])


def s(num, text):
    return field(num, 2, text.encode())


SID = "0b7c1a3e-2f4d-4c6b-9a8e-1d2c3b4a5f62"
start = (s(1, "measure") + s(2, "coordinator") + s(3, "1.0.0") + s(4, "config.default")
         + s(5, "policy.default") + field(6, 0, 60000))
proposal = s(1, "p1") + s(2, "option a") + s(3, "because")
commitment = (s(1, "c1") + s(2, "decision.approved") + s(3, "scope") + s(4, "done")
              + s(5, "1.0.0") + s(6, "policy.default") + s(7, "config.default")
              + field(8, 0, 1))
script = [("SessionStart", "m1", start), ("Proposal", "m2", proposal),
          ("Commitment", "m3", commitment)]


def drop_accepted_at(v):
    if isinstance(v, dict):
        return {k: drop_accepted_at(x) for k, x in v.items()
                if k not in ("acceptedAtUnixMs", "accepted_at_unix_ms")}
    if isinstance(v, list):
        return [drop_accepted_at(x) for x in v]
    return v


TS = re.compile(r"^(\x1b\[[0-9;]*m)*\d{4}-\d\d-\d\dT[\d:.]+Z(\x1b\[[0-9;]*m)*\s+")
ANSI = re.compile(r"\x1b\[[0-9;]*m")


def scripted(img):
    run(img)
    deadline = time.monotonic() + 60
    while not serving():
        if time.monotonic() > deadline:
            sys.exit(f"{img} never served")
        time.sleep(0.05)
    responses = []
    for mtype, mid, payload in script:
        env = {"envelope": {"macp_version": "1.0", "mode": "macp.mode.decision.v1",
                            "message_type": mtype, "message_id": mid, "session_id": SID,
                            "timestamp_unix_ms": "1800000000000",
                            "payload": base64.b64encode(payload).decode()}}
        r = sh("grpcurl", "-plaintext", "-import-path", MACP, "-proto", "macp/v1/core.proto",
               "-H", "authorization: Bearer coordinator", "-d", json.dumps(env),
               f"127.0.0.1:{PORT}", "macp.v1.MACPRuntimeService/Send", check=False)
        responses.append(drop_accepted_at(json.loads(r.stdout)) if r.returncode == 0
                         else {"error": r.stderr.strip()})
    time.sleep(0.5)
    sh("docker", "stop", "-t", "10", NAME)  # SIGTERM; then the log is complete
    logs = sh("docker", "logs", NAME, check=False)
    rm()
    lines = [ANSI.sub("", TS.sub("", l)) for l in (logs.stdout + logs.stderr).splitlines()]
    return responses, lines


eq = {tag: scripted(f"macp:{tag}") for tag in ("plain", "otel")}
same_resp = eq["plain"][0] == eq["otel"][0]
same_logs = eq["plain"][1] == eq["otel"][1]

# report
MiB = 2**20
p, o = out["plain"], out["otel"]
print(f"\nplatform: {p['platform']} (plain), {o['platform']} (otel); runs={RUNS}")
print(f"{'metric':<34}{'plain':>14}{'otel':>14}{'delta':>14}")
print(f"{'image size (MiB)':<34}{p['image_bytes']/MiB:>14.2f}{o['image_bytes']/MiB:>14.2f}"
      f"{(o['image_bytes']-p['image_bytes'])/MiB:>14.2f}")
print(f"{'image size, docker image ls':<34}{p['listed']:>14}{o['listed']:>14}")
print(f"{'binary size (MiB)':<34}{p['binary_bytes']/MiB:>14.2f}{o['binary_bytes']/MiB:>14.2f}"
      f"{(o['binary_bytes']-p['binary_bytes'])/MiB:>14.2f}")
for label in ("otel", "otel+blackhole"):
    for k in ("median_ms", "p90_ms"):
        a, b = startup["plain"][k], startup[label][k]
        print(f"{'startup ' + k + ' [' + label + ']':<34}{a:>14.1f}{b:>14.1f}{b-a:>14.1f}")
for label in ("otel", "otel+blackhole"):
    print(f"{'idle RSS MiB [' + label + ']':<34}{rss['plain']:>14.2f}{rss[label]:>14.2f}"
          f"{rss[label]-rss['plain']:>14.2f}")
    print(f"{'  VmRSS MiB [' + label + ']':<34}{vmrss['plain']:>14.2f}{vmrss[label]:>14.2f}"
          f"{vmrss[label]-vmrss['plain']:>14.2f}")
print(f"\nequivalence: responses identical={same_resp}, log lines identical={same_logs} "
      f"({len(eq['plain'][1])} vs {len(eq['otel'][1])} lines)")
print("responses (plain):", json.dumps(eq["plain"][0]))
if not same_resp:
    print("responses (otel): ", json.dumps(eq["otel"][0]))
if not same_logs:
    import difflib
    print("\n".join(difflib.unified_diff(eq["plain"][1], eq["otel"][1], "plain", "otel",
                                         lineterm="")))

img_d = o["image_bytes"] - p["image_bytes"]
rules = {
    "image size delta <= max(8 MiB, 10%)": img_d <= max(8 * MiB, 0.10 * p["image_bytes"]),
    "median startup delta <= 50 ms": startup["otel"]["median_ms"] - startup["plain"]["median_ms"] <= 50,
    "idle RSS delta <= 5 MiB": max(rss["otel"] - rss["plain"],
                                   vmrss["otel"] - vmrss["plain"]) <= 5,
    "behaviour equivalent": same_resp and same_logs,
}
print("\ndecision rule (otel image with no OTEL vars vs plain):")
for k, v in rules.items():
    print(f"  {'PASS' if v else 'FAIL'}  {k}")
print("VERDICT:", "build the published image with --features otel" if all(rules.values())
      else "keep the plain image; document --build-arg CARGO_FEATURES=otel")
PY

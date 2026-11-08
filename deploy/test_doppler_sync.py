"""Offline tests for deploy/doppler-sync.sh.

    python3 deploy/test_doppler_sync.py

The script runs for real (bash, git, jq) against a throwaway git repo, with
fake `doppler`, `railway` and `curl` binaries first on PATH. The fake curl
serves a minimal Railway GraphQL API and logs every argv it receives, so the
tests prove that a failed Railway read stops the run with nothing written,
and that no secret value or token reaches a command line or the output.
Pattern from aitp-website scripts/test_doppler_sync.py (#117).
"""

import json
import os
import shutil
import stat
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SCRIPT = ROOT / "deploy" / "doppler-sync.sh"
TOKEN = "tok-SENTINEL-4f1c"
SECRET = "s3cr3t-VALUE-9a7e"


def script_const(name):
    for line in SCRIPT.read_text().splitlines():
        if line.startswith(name + "="):
            return line.split("=", 1)[1].split("#", 1)[0].strip()
    raise KeyError(name)


FAKE_CURL = r'''#!/usr/bin/env python3
import json, os, sys
with open(os.environ["FAKE_LOG"], "a") as log:
    log.write("curl " + json.dumps(sys.argv[1:]) + "\n")
args = sys.argv[1:]
headers, data = [], None
i = 0
while i < len(args):
    a = args[i]
    if a == "-H": headers.append(args[i + 1]); i += 2; continue
    if a in ("-d", "--data", "--data-binary"): data = args[i + 1]; i += 2; continue
    i += 1
auth = []
for h in headers:
    if h.startswith("@"):
        with open(h[1:]) as f: auth += [l.strip() for l in f if l.strip()]
    else:
        auth.append(h)
if "Authorization: Bearer " + os.environ["EXPECT_TOKEN"] not in auth:
    sys.stderr.write("fake curl: missing auth\n"); sys.exit(22)
body = json.loads(sys.stdin.read() if data == "@-" else data)
q = body["query"]
which = "project" if "project(id" in q else "vars" if "variables(" in q else "?"
fail = os.environ.get("FAKE_FAIL", "")
if fail.startswith(which + ":"):
    kind = fail.split(":", 1)[1]
    if kind == "http": sys.exit(22)
    if kind == "network": sys.exit(6)
    if kind == "garbage": sys.stdout.write("<html>oops</html>"); sys.exit(0)
    if kind == "graphql":
        sys.stdout.write(json.dumps({"errors": [{"message": "Not Authorized"}], "data": None})); sys.exit(0)
def out(o): sys.stdout.write(json.dumps({"data": o})); sys.exit(0)
if which == "project":
    out({"project": {"environments": {"edges": [{"node": {"id": "env1", "name": os.environ["FAKE_ENV"]}}]},
                     "services": {"edges": [{"node": {"id": "svc1", "name": os.environ["FAKE_SERVICE"]}}]}}})
if which == "vars":
    with open(os.environ["FAKE_RAILWAY_STATE"]) as f: out({"variables": json.load(f)})
sys.exit(22)
'''

FAKE_DOPPLER = r'''#!/usr/bin/env bash
printf 'doppler %s\n' "$*" >> "$FAKE_LOG"
case "$1" in
  me) exit 0 ;;
  secrets) [ -n "${FAKE_DOPPLER_FAIL:-}" ] && exit 1; cat "$FAKE_DOPPLER_JSON" ;;
  *) exit 1 ;;
esac
'''

# Anything but `railway whoami` (link, variable set, redeploy) is a write.
FAKE_RAILWAY = r'''#!/usr/bin/env bash
printf 'railway %s\n' "$*" >> "$FAKE_LOG"
[ -t 0 ] || cat >/dev/null
exit 0
'''


class DopplerSyncTest(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="doppler-sync-test-"))
        self.addCleanup(shutil.rmtree, self.tmp, True)
        self.repo = self.tmp / "repo"
        (self.repo / "deploy").mkdir(parents=True)
        shutil.copy(SCRIPT, self.repo / "deploy" / "doppler-sync.sh")
        (self.repo / "deploy" / "doppler-keys.txt").write_text("# managed\nSITE_FLAG\n")
        env = {**os.environ, "GIT_AUTHOR_NAME": "t", "GIT_AUTHOR_EMAIL": "t@t",
               "GIT_COMMITTER_NAME": "t", "GIT_COMMITTER_EMAIL": "t@t"}
        for cmd in (["init", "-q"], ["add", "-A"], ["commit", "-q", "-m", "keys"]):
            subprocess.run(["git", "-C", str(self.repo), *cmd], check=True, capture_output=True, env=env)
        self.bin = self.tmp / "bin"
        self.bin.mkdir()
        for name, body in (("curl", FAKE_CURL), ("doppler", FAKE_DOPPLER), ("railway", FAKE_RAILWAY)):
            p = self.bin / name
            p.write_text(body)
            p.chmod(p.stat().st_mode | stat.S_IEXEC)
        self.log = self.tmp / "calls.log"
        self.log.write_text("")
        self.state = self.tmp / "railway.json"
        self.doppler = self.tmp / "doppler.json"
        self.doppler.write_text(json.dumps({"SITE_FLAG": SECRET}))
        self.state.write_text(json.dumps({"SITE_FLAG": SECRET}))

    def run_sync(self, *args, **extra_env):
        env = {**os.environ, "PATH": f"{self.bin}:{os.environ['PATH']}", "RAILWAY_API_TOKEN": TOKEN,
               "EXPECT_TOKEN": TOKEN, "FAKE_LOG": str(self.log), "FAKE_RAILWAY_STATE": str(self.state),
               "FAKE_DOPPLER_JSON": str(self.doppler),
               "FAKE_SERVICE": script_const("RAILWAY_SERVICE"), "FAKE_ENV": script_const("RAILWAY_ENVIRONMENT"),
               **extra_env}
        r = subprocess.run(["bash", "deploy/doppler-sync.sh", *args], cwd=self.repo, env=env,
                           capture_output=True, text=True, timeout=60)
        for blob in (r.stdout, r.stderr, self.log.read_text()):
            self.assertNotIn(SECRET, blob, "secret value leaked")
            self.assertNotIn(TOKEN, blob, "token leaked onto argv or output")
        return r

    def writes(self):
        return [l for l in self.log.read_text().splitlines()
                if l.startswith("railway ") and not l.startswith("railway whoami")]

    def test_in_sync_dry_run_and_apply_write_nothing(self):
        for mode in ((), ("--apply",)):
            r = self.run_sync(*mode)
            self.assertEqual(r.returncode, 0, r.stderr)
            self.assertIn("SITE_FLAG: unchanged", r.stderr)
            self.assertIn("in sync", r.stderr)
        self.assertEqual(self.writes(), [])
        for line in self.log.read_text().splitlines():
            self.assertNotIn("Bearer", line)

    def test_failed_variables_read_fails_closed(self):
        # The key exists in Railway with another value: if a failed read
        # looked like "no entry" the script would report "new" and write.
        self.state.write_text(json.dumps({"SITE_FLAG": "old"}))
        for failing in ("vars:http", "vars:network", "vars:garbage", "vars:graphql"):
            for mode in ((), ("--apply",)):
                with self.subTest(failing=failing, mode=mode):
                    r = self.run_sync(*mode, FAKE_FAIL=failing)
                    self.assertNotEqual(r.returncode, 0)
                    self.assertIn("reading Railway variables failed", r.stderr)
                    self.assertNotIn(": new", r.stderr)
                    self.assertNotIn(": changed", r.stderr)
        self.assertEqual(self.writes(), [])

    def test_failed_project_read_fails_closed(self):
        for failing in ("project:http", "project:network", "project:garbage", "project:graphql"):
            with self.subTest(failing=failing):
                r = self.run_sync("--apply", FAKE_FAIL=failing)
                self.assertNotEqual(r.returncode, 0)
                self.assertIn("reading Railway project", r.stderr)
        self.assertEqual(self.writes(), [])

    def test_failed_doppler_read_fails_closed(self):
        for mode in ((), ("--apply",)):
            r = self.run_sync(*mode, FAKE_DOPPLER_FAIL="1")
            self.assertNotEqual(r.returncode, 0)
            self.assertIn("reading Doppler", r.stderr)
        self.assertEqual(self.writes(), [])

    def test_missing_token_stops_before_any_api_call(self):
        home = self.tmp / "home"
        home.mkdir()
        r = self.run_sync(RAILWAY_API_TOKEN="", HOME=str(home))
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("no Railway token", r.stderr)
        self.assertNotIn("curl ", self.log.read_text())


if __name__ == "__main__":
    unittest.main()

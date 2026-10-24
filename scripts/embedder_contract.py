#!/usr/bin/env python3
"""PR-time gates for the embeddable-crate contract (docs/stability.md).

Two subcommands, both run by .github/workflows/embedder-contract.yml:

  semver-gate        Fail a PR that makes an unannounced breaking change to the
                     public API of macp-core, macp-modes, macp-policy or macp-pb.
  release-migrating  Fail a release-plz PR that gives those crates a breaking
                     version bump while MIGRATING.md has no entry for it.

Both read the base commit from $BASE_SHA and run from the repository root.
"""

import os
import re
import subprocess
import sys
import tomllib

EMBEDDABLE = ["macp-core", "macp-modes", "macp-policy", "macp-pb"]
MIGRATING = "MIGRATING.md"
LABEL = "semver-breaking"


def git_show(rev, path):
    return subprocess.run(
        ["git", "show", f"{rev}:{path}"], check=True, capture_output=True, text=True
    ).stdout


def read(rev, path):
    """`path` at `rev`, or in the working tree when `rev` is None."""
    if rev is None:
        with open(path, encoding="utf-8") as f:
            return f.read()
    return git_show(rev, path)


def crate_version(rev, crate):
    """The version `crate` declares at `rev`, resolving `version.workspace`."""
    package = tomllib.loads(read(rev, f"crates/{crate}/Cargo.toml"))["package"]
    version = package["version"]
    if isinstance(version, dict) and version.get("workspace"):
        root = tomllib.loads(read(rev, "Cargo.toml"))
        version = root["workspace"]["package"]["version"]
    return version


def parse(version):
    major, minor, patch = (int(x) for x in version.split("-")[0].split("."))
    return major, minor, patch


def is_breaking_bump(old, new):
    """Cargo's semver reading: pre-1.0 the minor number is the breaking one."""
    (o_major, o_minor, _), (n_major, n_minor, _) = parse(old), parse(new)
    if n_major != o_major:
        return True
    return n_major == 0 and n_minor != o_minor


def next_breaking(version):
    major, minor, _ = parse(version)
    return f"0.{minor + 1}.0" if major == 0 else f"{major + 1}.0.0"


def has_migrating_entry(version, rev=None):
    """True when MIGRATING.md has a `## <version>` heading (template in the file)."""
    try:
        text = read(rev, MIGRATING)
    except (OSError, subprocess.CalledProcessError):
        return False
    heading = re.compile(rf"^##\s+v?{re.escape(version)}(?![0-9A-Za-z.\-])", re.M)
    return heading.search(text) is not None


def migrating_changed(base):
    diff = subprocess.run(["git", "diff", "--quiet", base, "HEAD", "--", MIGRATING])
    return diff.returncode != 0


class ToolFailure(Exception):
    """cargo-semver-checks failed without reporting a semver violation."""


def semver_checks(*extra):
    """True when clean, False on a semver violation, ToolFailure otherwise.

    The exit code alone cannot tell a violation from a failure to run (a
    crate that does not build, an unreachable registry), so a nonzero exit
    counts as a violation only when the tool printed its violation summary.
    """
    cmd = ["cargo", "semver-checks"]
    for crate in EMBEDDABLE:
        cmd += ["--package", crate]
    cmd += list(extra)
    print("+ " + " ".join(cmd), flush=True)
    run = subprocess.run(cmd, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    print(run.stdout, flush=True)
    if run.returncode == 0:
        return True
    if "semver requires new major version" in run.stdout:
        return False
    raise ToolFailure(" ".join(cmd))


def error(msg):
    print(f"::error title=Embedder contract::{msg}", flush=True)
    return 1


def semver_gate(base):
    # 1. Against the last published release: what an embedder upgrading from
    #    crates.io would see. A version bump on HEAD (a release PR) permits
    #    the break, exactly as cargo-semver-checks' own bump rule does.
    if semver_checks():
        print("OK: no breaking API change against the published crates.")
        return 0

    upcoming = next_breaking(crate_version(None, "macp-core"))

    # 2. The break exists. Is it this PR's, or already on the base branch?
    if semver_checks("--baseline-rev", base):
        if has_migrating_entry(upcoming):
            print(
                f"OK: this PR adds no breaking change. The base branch already "
                f"carries one, announced under `## {upcoming}` in {MIGRATING}."
            )
            return 0
        return error(
            f"The base branch carries a breaking change against crates.io that "
            f"{MIGRATING} does not announce. Add a `## {upcoming}` entry "
            f"(template in {MIGRATING}) before merging more work."
        )

    # 3. This PR introduces the break: it must say so twice, once by label and
    #    once in the embedder-facing migration notes.
    problems = []
    if os.environ.get("SEMVER_BREAKING_LABEL") != "true":
        problems.append(f"add the `{LABEL}` label to the PR")
    if not migrating_changed(base) or not has_migrating_entry(upcoming):
        problems.append(
            f"describe the migration under `## {upcoming}` in {MIGRATING} "
            f"(this PR must edit it)"
        )
    if problems:
        return error(
            "This PR makes a breaking change to an embeddable crate's public "
            "API (report above). To ship it: " + "; ".join(problems) + ". "
            "See docs/stability.md."
        )
    print(f"OK: breaking change announced (`{LABEL}` label, `## {upcoming}` in {MIGRATING}).")
    return 0


def release_migrating(base):
    breaking = {}
    for crate in EMBEDDABLE:
        old, new = crate_version(base, crate), crate_version(None, crate)
        print(f"{crate}: {old} -> {new}")
        if is_breaking_bump(old, new):
            breaking[crate] = new
    if not breaking:
        print("OK: no breaking version bump; MIGRATING.md entry not required.")
        return 0
    missing = sorted({v for v in breaking.values() if not has_migrating_entry(v)})
    if missing:
        return error(
            f"Breaking release ({', '.join(sorted(breaking))} -> "
            f"{', '.join(missing)}) has no `## {missing[0]}` entry in {MIGRATING}. "
            f"Add it on main (template in {MIGRATING}); release-plz refreshes this PR."
        )
    print(f"OK: {MIGRATING} has an entry for {', '.join(sorted(set(breaking.values())))}.")
    return 0


def main():
    commands = {"semver-gate": semver_gate, "release-migrating": release_migrating}
    if len(sys.argv) != 2 or sys.argv[1] not in commands:
        print(f"usage: {sys.argv[0]} {{{','.join(commands)}}}", file=sys.stderr)
        return 2
    base = os.environ.get("BASE_SHA")
    if not base:
        print("BASE_SHA is not set", file=sys.stderr)
        return 2
    try:
        return commands[sys.argv[1]](base)
    except ToolFailure as failure:
        return error(
            f"`{failure}` failed without reporting a semver result (log above). "
            f"This is a build or tool failure, not a verdict on the API."
        )


if __name__ == "__main__":
    sys.exit(main())

#!/usr/bin/env python3
"""Decides how much of CI a change needs.

Every repository on the account shares one pool of hosted runners, and a
macOS or Windows on ARM job holds a slot far longer than a Linux one. So a
pull request tests on Linux and Windows, and the full four-system matrix runs
on main, in the merge queue, by hand, and on a pull request that touches the
dependencies, the CI itself, macOS code, or carries the `all-platforms` label.
A change to Markdown, the site, or the repository's GitHub settings alone is
tested on Linux only (the branding test reads the guide) and skips the Rust
lints and the Nix package.

Usage: ci-scope.py --repo <owner/name> --event <name> [--pr <number>]
                   [--before <sha> --after <sha>]
Writes `code` and `platforms` to $GITHUB_OUTPUT.
"""

import argparse
import fnmatch
import json
import os
import subprocess

LABEL = "all-platforms"
LINUX = "ubuntu-latest"
PULL_REQUEST = [LINUX, "windows-latest"]
EVERYTHING = [LINUX, "macos-latest", "windows-latest", "windows-11-arm"]
NO_BUILD = ["docs/*", "*.md", ".github/*"]
FULL = ["Cargo.toml", "Cargo.lock", ".cargo/*", "rust-toolchain*", "build.rs", ".github/workflows/ci.yml"]


def matches(path, globs):
    """fnmatch's * crosses folders, so docs/* is the whole site."""
    return any(fnmatch.fnmatch(path, glob) for glob in globs)


def scope(event, paths, labels):
    """Whether the Rust code needs checking, and the systems to test on.

    `paths` is None when the change could not be read, which runs everything.
    """
    if paths and all(matches(path, NO_BUILD) and not matches(path, FULL) for path in paths):
        return False, [LINUX]
    if event != "pull_request" or paths is None or LABEL in labels:
        return True, EVERYTHING
    if any(matches(path, FULL) for path in paths):
        return True, EVERYTHING
    systems = list(PULL_REQUEST)
    if any("macos" in path.lower() for path in paths):
        systems.insert(1, "macos-latest")
    return True, systems


def api(path, jq):
    out = subprocess.run(
        ["gh", "api", "--paginate", path, "--jq", jq], check=True, capture_output=True, text=True
    ).stdout
    return [line for line in out.splitlines() if line]


def changed(args):
    try:
        if args.event == "pull_request":
            return api(f"repos/{args.repo}/pulls/{args.pr}/files", ".[].filename")
        if args.event == "push" and args.before and set(args.before) != {"0"}:
            return api(f"repos/{args.repo}/compare/{args.before}...{args.after}", ".files[].filename")
    except subprocess.CalledProcessError:
        pass
    return None


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--repo", required=True)
    parser.add_argument("--event", required=True)
    parser.add_argument("--pr")
    parser.add_argument("--before")
    parser.add_argument("--after")
    args = parser.parse_args()
    labels = set()
    if args.event == "pull_request":
        labels = set(api(f"repos/{args.repo}/issues/{args.pr}/labels", ".[].name"))
    code, platforms = scope(args.event, changed(args), labels)
    print(f"Rust checks: {code}. Test systems: {', '.join(platforms)}.")
    with open(os.environ["GITHUB_OUTPUT"], "a") as out:
        out.write(f"code={json.dumps(code)}\nplatforms={json.dumps(platforms)}\n")


if __name__ == "__main__":
    main()

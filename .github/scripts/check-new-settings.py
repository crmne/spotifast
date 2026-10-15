#!/usr/bin/env python3
"""Fails when a pull request adds a setting the maintainer has not approved.

A new setting needs the maintainer's approval in its issue first
(CONTRIBUTING.md, "Settings"). This compares the fields of `Settings` and the
other `...Settings` structs in src/settings.rs between the pull request's base
and its head, and fails on any field the head adds, a renamed field included.
The maintainer approves one by adding the `setting-approved` label, which only
people with triage access can do, and re-running the check.

Usage: check-new-settings.py --base <commit> [--approved]
"""

import argparse
import os
import re
import subprocess
import sys
from pathlib import Path

FILE = "src/settings.rs"
LABEL = "setting-approved"
STRUCT = re.compile(r"^pub struct (\w*Settings)\s*\{", re.MULTILINE)
# A field starts its line: optional visibility, a name, then one colon (a
# type continued on the next line may start with a path such as `crate::`).
FIELD = re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:r#)?(\w+)\s*:(?!:)")


def body(source, start):
    """The text between the brace at `start` and the brace that closes it."""
    depth = 0
    for index in range(start, len(source)):
        if source[index] == "{":
            depth += 1
        elif source[index] == "}":
            depth -= 1
            if depth == 0:
                return source[start + 1 : index]
    return source[start + 1 :]


def without_attributes(text):
    """Removes `#[...]` attributes, which may span lines and hold colons."""
    out, index = [], 0
    while index < len(text):
        if text.startswith("#[", index):
            depth = 0
            while index < len(text):
                depth += {"[": 1, "]": -1}.get(text[index], 0)
                index += 1
                if depth == 0:
                    break
        else:
            out.append(text[index])
            index += 1
    return "".join(out)


def fields(source):
    """{struct name: set of field names} for each `pub struct ...Settings`."""
    # Comments first, so that a brace or a bracket in one changes nothing.
    source = "\n".join(line.split("//", 1)[0] for line in source.splitlines())
    found = {}
    for match in STRUCT.finditer(source):
        text = without_attributes(body(source, match.end() - 1))
        names = set()
        for line in text.splitlines():
            field = FIELD.match(line)
            if field:
                names.add(field.group(1))
        found[match.group(1)] = names
    return found


def added(base, head):
    """Sorted (struct, field) pairs in `head` that `base` does not have."""
    before, after = fields(base), fields(head)
    return sorted(
        (struct, name) for struct, names in after.items() for name in names - before.get(struct, set())
    )


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--base", required=True, help="the commit the pull request merges into")
    parser.add_argument("--approved", action="store_true", help=f"the pull request has the {LABEL} label")
    args = parser.parse_args()

    root = Path(__file__).resolve().parents[2]
    base = subprocess.run(
        ["git", "show", f"{args.base}:{FILE}"], cwd=root, check=True, capture_output=True, text=True
    ).stdout
    found = added(base, (root / FILE).read_text())
    if not found:
        return 0
    for struct, name in found:
        message = f"adds the setting {struct}.{name}"
        print(f"::{'notice' if args.approved else 'error'} file={FILE}::{message}"
              if os.environ.get("GITHUB_ACTIONS") else f"{FILE}: {message}")
    if args.approved:
        print(f"The maintainer approved the new setting with the {LABEL} label.")
        return 0
    print(
        "New settings need the maintainer's approval in the issue first; prefer a good default, "
        "a shortcut or a command. See CONTRIBUTING.md, Settings. The maintainer approves one with "
        f"the {LABEL} label and a re-run of this check."
    )
    return 1


if __name__ == "__main__":
    sys.exit(main())

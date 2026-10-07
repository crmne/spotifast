#!/usr/bin/env python3
"""Check a release's Spotizgeg downloads and write their checksums."""

import argparse
import hashlib
from pathlib import Path
import re


def prepare(directory, tag):
    if not re.fullmatch(r"v\d+\.\d+\.\d+(?:-[A-Za-z0-9.-]+)?", tag):
        raise ValueError("Expected a release tag")
    files = []
    for path in directory.iterdir():
        if path.name.startswith("spotizgeg-" + tag + "-"):
            if not path.is_file() or path.is_symlink():
                raise ValueError(f"Expected a regular release file: {path.name}")
            files.append(path)
    if not files:
        raise ValueError("No release downloads found")

    def digest(path):
        with path.open("rb") as stream:
            return hashlib.file_digest(stream, "sha256").hexdigest()

    manifest = "".join(f"{digest(path)}  {path.name}\n" for path in sorted(files))
    (directory / "checksums.txt").write_text(manifest)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=Path)
    parser.add_argument("tag")
    args = parser.parse_args()
    prepare(args.directory, args.tag)

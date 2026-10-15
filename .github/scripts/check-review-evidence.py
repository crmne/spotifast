#!/usr/bin/env python3
"""Fails when the repository holds visual review evidence.

Before-and-after captures, comparison pages and recordings belong in the pull
request description or its comments, never in the repository (CONTRIBUTING.md,
"Visual reviews"). Two rules find them among the tracked files:

1. Every file under docs/assets/, and every image or video elsewhere under
   docs/, is named by the documentation: a site page or setting outside
   docs/assets/, a top-level file such as the README, or packaging metadata.
   A review folder's own README or comparison page does not count.
2. No image, video or HTML file anywhere has review, compare, comparison,
   before or after as a word in its path.
"""

import os
import re
import subprocess
import sys
from pathlib import Path

SITE = "docs/"
ASSETS = "docs/assets/"
MEDIA = re.compile(r"\.(png|jpe?g|gif|webp|avif|svg|mp4|m4v|webm|mov|mkv)$", re.IGNORECASE)
PAGE = re.compile(r"\.html?$", re.IGNORECASE)
REVIEW_WORD = re.compile(r"(?<![a-z])(reviews?|compare|comparisons?|before|after)(?![a-z])", re.IGNORECASE)


def published(path):
    """True for files the site serves only because a page shows them."""
    return path.startswith(ASSETS) or (path.startswith(SITE) and MEDIA.search(path) is not None)


def documentation(path):
    """True for files that can show a published file: pages, settings, the README, packaging metadata."""
    return (path.startswith(SITE) and not path.startswith(ASSETS)) or "/" not in path or path.startswith("packaging/")


def problems(files):
    """Returns (path, message) for every piece of review evidence in {path: bytes}."""
    # Pages name a file by its site path (/assets/...) or its repository path
    # (docs/assets/...), so the path below docs/ is what both contain.
    references = "\n".join(
        data.decode("utf-8", "replace") for path, data in files.items() if documentation(path) and b"\0" not in data
    )
    found = []
    for path in sorted(files):
        if published(path) and path.removeprefix(SITE) not in references:
            found.append((path, "no documentation uses it"))
        elif (MEDIA.search(path) or PAGE.search(path)) and REVIEW_WORD.search(path):
            found.append((path, "named like review evidence"))
    return found


def tracked_files(root):
    listing = subprocess.run(["git", "ls-files", "-z"], cwd=root, check=True, capture_output=True, text=True).stdout
    paths = [path for path in listing.split("\0") if path]
    return {path: (root / path).read_bytes() for path in paths if (root / path).is_file()}


def main():
    found = problems(tracked_files(Path(__file__).resolve().parents[2]))
    for path, message in found:
        print(f"::error file={path}::{message}" if os.environ.get("GITHUB_ACTIONS") else f"{path}: {message}")
    if found:
        print(
            "Screenshots, captures, comparison pages and recordings go in the pull request "
            "description or comments, not the repository; see CONTRIBUTING.md, Visual reviews."
        )
    return 1 if found else 0


if __name__ == "__main__":
    sys.exit(main())

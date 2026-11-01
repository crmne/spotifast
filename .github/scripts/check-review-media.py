#!/usr/bin/env python3
"""Fails when a pull request changes the interface without showing it.

A pull request that changes what people see attaches before-and-after
captures to its description or to a comment by its author (CONTRIBUTING.md,
"Visual reviews"). This reads the pull request through the GitHub API, never
its code, and fails when it touches a visual path while neither the
description nor the author's comments hold an image or a video. A change to
those files that shows nothing new, such as a crash fix, passes once the
maintainer adds the `no-visual-change` label and re-runs the check.

Usage: check-review-media.py --repo <owner/name> --pr <number>
"""

import argparse
import fnmatch
import json
import re
import subprocess
import sys

LABEL = "no-visual-change"
VISUAL_PATHS = [
    "src/ui/*",
    "src/theme.rs",
    "src/appearance.rs",
    "src/skin/*",
    "src/winamp.rs",
    "src/vis.rs",
    "src/window.rs",
    "src/window/*",
]
# Attachments dragged into GitHub's editor, and images or videos written out.
MEDIA = re.compile(
    r"github\.com/user-attachments/"
    r"|private-user-images\.githubusercontent\.com/"
    r"|user-images\.githubusercontent\.com/"
    r"|<img\b|<video\b|!\[[^\]]*\]\([^)]+\)",
    re.IGNORECASE,
)


def visual_files(paths):
    """The changed paths that draw something. fnmatch's * crosses folders."""
    return [path for path in paths if any(fnmatch.fnmatch(path, glob) for glob in VISUAL_PATHS)]


def has_media(texts):
    return any(MEDIA.search(text or "") for text in texts)


def problem(paths, texts, labels):
    """What to tell the author, or None when the pull request passes."""
    if LABEL in labels:
        return None
    changed = visual_files(paths)
    if not changed or has_media(texts):
        return None
    shown = ", ".join(changed[:5]) + (" and more" if len(changed) > 5 else "")
    return (
        f"This pull request changes the interface ({shown}) but its description and "
        "comments hold no screenshots or recording. Attach before and after captures, "
        'as "Visual reviews" in CONTRIBUTING.md describes, then re-run this check. '
        f"If nothing visible changes, the maintainer can add the {LABEL} label."
    )


def api(path):
    out = subprocess.run(
        ["gh", "api", "--paginate", "--slurp", path], check=True, capture_output=True, text=True
    ).stdout
    pages = json.loads(out)
    if pages and isinstance(pages[0], list):
        return [item for page in pages for item in page]
    return pages[0] if len(pages) == 1 else pages


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--repo", required=True)
    parser.add_argument("--pr", required=True)
    args = parser.parse_args()
    pr = api(f"repos/{args.repo}/pulls/{args.pr}")
    author = pr["user"]["login"]
    files = [item["filename"] for item in api(f"repos/{args.repo}/pulls/{args.pr}/files")]
    comments = api(f"repos/{args.repo}/issues/{args.pr}/comments")
    texts = [pr.get("body")] + [c.get("body") for c in comments if c["user"]["login"] == author]
    labels = {label["name"] for label in pr.get("labels", [])}
    message = problem(files, texts, labels)
    if message:
        print(f"::error::{message}")
        sys.exit(1)
    print("Review media check passed.")


if __name__ == "__main__":
    main()

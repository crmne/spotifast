#!/usr/bin/env python3
"""Compare the real limiter sources at a git revision and in the working tree.

No Spotify account or Cargo dependency build is needed. Temporary compiler
outputs stay under .cache and are removed when the benchmark exits.
"""

import argparse
import json
from pathlib import Path
import subprocess
import tempfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline", required=True, help="git revision to compare")
    args = parser.parse_args()
    root = Path(__file__).resolve().parent.parent
    source = subprocess.check_output(
        ["git", "show", f"{args.baseline}:src/limiter.rs"], cwd=root, text=True
    )
    cache = root / ".cache"
    cache.mkdir(exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="limiter-benchmark-", dir=cache) as work:
        work = Path(work)
        baseline = work / "baseline.rs"
        baseline.write_text(source)
        driver = work / "main.rs"
        driver.write_text(
            f"#[path = {json.dumps(str(baseline), ensure_ascii=False)}] mod baseline;\n"
            f"#[path = {json.dumps(str(root / 'src/limiter.rs'), ensure_ascii=False)}] mod candidate;\n"
            f"include!({json.dumps(str(root / 'packaging/benchmarks/limiter.rs'), ensure_ascii=False)});\n"
        )
        binary = work / "limiter-benchmark"
        subprocess.run(
            ["rustc", "--edition=2024", "-O", str(driver), "-o", str(binary)],
            check=True,
        )
        subprocess.run([str(binary)], check=True)


if __name__ == "__main__":
    main()

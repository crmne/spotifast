#!/usr/bin/env python3
"""Check that an AppImage carries Spotifast's update information for ARCH
and that the .zsync file beside it describes it."""

import argparse
import importlib.util
from pathlib import Path
import sys

sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location("release_names", Path(__file__).with_name("release-names.py"))
release_names = importlib.util.module_from_spec(spec)
spec.loader.exec_module(release_names)

if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("appimage", type=Path)
    parser.add_argument("arch", choices=sorted(release_names.APPIMAGE_ARCHITECTURES.values()))
    args = parser.parse_args()
    expected = release_names.update_information(args.arch)
    found = release_names.embedded_update_information(args.appimage)
    if found != expected:
        sys.exit(f"{args.appimage.name} carries {found!r}, not {expected!r}")
    release_names.check_zsync(Path(f"{args.appimage}.zsync"), args.appimage)
    print(f"{args.appimage.name}: {found}")

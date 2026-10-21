#!/usr/bin/env python3
"""Check a release's Spotifast downloads and write their checksums."""

import argparse
import fnmatch
import hashlib
from pathlib import Path
import re
import struct

# The architectures the AppImages are built for, by their Linux archives'
# targets.
APPIMAGE_ARCHITECTURES = {
    "x86_64-unknown-linux-gnu": "x86_64",
    "aarch64-unknown-linux-gnu": "aarch64",
}


def update_information(arch):
    """What each AppImage must carry for AppImageUpdate and the updaters
    built on it: the newest stable release's .zsync file for its own
    architecture."""
    return f"gh-releases-zsync|crmne|spotifast|latest|spotifast-*-{arch}.AppImage.zsync"


def section(data, name):
    """The bytes of a little-endian ELF64 file's section `name`, or None."""
    if data[:4] != b"\x7fELF" or data[4] != 2 or data[5] != 1:
        raise ValueError("Expected a 64-bit little-endian AppImage runtime")
    (offset,) = struct.unpack_from("<Q", data, 0x28)
    size, count, names = struct.unpack_from("<3H", data, 0x3A)

    def header(index):
        start = offset + index * size
        (label,) = struct.unpack_from("<I", data, start)
        location, length = struct.unpack_from("<2Q", data, start + 0x18)
        return label, location, length

    _, strings, _ = header(names)
    wanted = name.encode() + b"\0"
    for index in range(count):
        label, location, length = header(index)
        if data[strings + label : strings + label + len(wanted)] == wanted:
            return data[location : location + length]
    return None


def embedded_update_information(path):
    with path.open("rb") as stream:
        # The runtime comes first; its sections are well inside this.
        data = stream.read(4 * 1024 * 1024)
    found = section(data, ".upd_info")
    if found is None:
        raise ValueError(f"{path.name} has no update information section")
    return found.split(b"\0", 1)[0].decode()


def check_zsync(zsync, appimage):
    header = zsync.read_bytes().split(b"\n\n", 1)[0].decode()
    fields = dict(line.split(": ", 1) for line in header.splitlines() if ": " in line)
    with appimage.open("rb") as stream:
        sha1 = hashlib.file_digest(stream, "sha1").hexdigest()
    expected = {
        "Filename": appimage.name,
        "URL": appimage.name,
        "Length": str(appimage.stat().st_size),
        "SHA-1": sha1,
    }
    wrong = [key for key, value in expected.items() if fields.get(key) != value]
    if wrong:
        raise ValueError(f"{zsync.name} does not describe {appimage.name}: {', '.join(wrong)}")


def check_appimages(directory, tag, files):
    """Every AppImage carries the update information for its architecture
    and has its .zsync file beside it. A stable release has one for each
    Linux archive, so the updater and AppImageUpdate find them."""
    version = tag[1:]
    names = {path.name for path in files}
    for target, arch in APPIMAGE_ARCHITECTURES.items():
        appimage = directory / f"spotifast-{version}-{arch}.AppImage"
        zsync = directory / f"{appimage.name}.zsync"
        present = appimage.name in names
        if present != (zsync.name in names):
            raise ValueError(f"Expected {appimage.name} and {zsync.name} together")
        if not present:
            if "-" not in version and f"spotifast-{tag}-{target}.tar.gz" in names:
                raise ValueError(f"Expected {appimage.name} beside the {target} archive")
            continue
        information = embedded_update_information(appimage)
        if information != update_information(arch):
            raise ValueError(f"{appimage.name} carries {information!r}, not {update_information(arch)!r}")
        pattern = information.rsplit("|", 1)[1]
        if not fnmatch.fnmatchcase(zsync.name, pattern):
            raise ValueError(f"{pattern} does not match {zsync.name}")
        check_zsync(zsync, appimage)


def prepare(directory, tag):
    if not re.fullmatch(r"v\d+\.\d+\.\d+(?:-[A-Za-z0-9.-]+)?", tag):
        raise ValueError("Expected a release tag")
    # The AppImages keep the name native-packages gives them, without the v
    # and with the processor alone, as earlier releases published them.
    appimage = re.compile(re.escape(f"spotifast-{tag[1:]}-") + r"(x86_64|aarch64)\.AppImage(\.zsync)?")
    files = []
    for path in directory.iterdir():
        if path.name.startswith("spotifast-" + tag + "-") or appimage.fullmatch(path.name):
            if not path.is_file() or path.is_symlink():
                raise ValueError(f"Expected a regular release file: {path.name}")
            files.append(path)
    if not files:
        raise ValueError("No release downloads found")
    check_appimages(directory, tag, files)

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

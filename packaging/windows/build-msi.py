#!/usr/bin/env python3
"""Build the Windows Installer package (spotifast.wxs) from a release binary.

    python packaging/windows/build-msi.py PAYLOAD PACKAGE VERSION ARCH [NAME=VALUE ...]

PAYLOAD is a folder holding spotifast.exe, README.md, LICENSE and
spotifast-installer.txt; PACKAGE is the .msi to write; VERSION is the
release (1.2.3, or 1.2.3-rc1); ARCH is amd64 or arm64. native-packages runs
it this way for the windows-* targets in native-packages.yaml. NAME=VALUE
pairs override the package's identity, which only test-msi.py does.

Needs Windows, the WiX Toolset 5 command line
(dotnet tool install --global wix --version 5.0.2) and the MSVC compiler for
ARCH on PATH, from a Visual Studio developer prompt (CI uses
ilammy/msvc-dev-cmd), for the custom actions in running-copy.c and
update-leftovers.c. It writes
nothing beside PACKAGE: native-packages refuses any other output.
"""

import argparse
from pathlib import Path
import re
import struct
import subprocess
import sys
import tempfile

HERE = Path(__file__).resolve().parent
WIX = "5.0.2"
# The package's custom actions, built into one library.
ACTIONS = ("running-copy.c", "update-leftovers.c")
EXTENSIONS = (f"WixToolset.UI.wixext/{WIX}", f"WixToolset.Util.wixext/{WIX}")
# native-packages architecture: WiX -arch value and PE machine type.
ARCHES = {"amd64": ("x64", 0x8664), "arm64": ("arm64", 0xAA64)}
PAYLOAD = ("spotifast.exe", "README.md", "LICENSE", "spotifast-installer.txt")
# Variables the build sets itself; the identity ones come from NAME=VALUE.
BUILT = {"Version", "Payload", "CustomActions", "LicenseRtf", "Icon"}


def numeric_version(version):
    """Windows Installer compares three numbers: 1.2.3-rc1 installs as 1.2.3."""
    match = re.fullmatch(r"(\d+)\.(\d+)\.(\d+)(?:-[0-9A-Za-z.-]+)?", version)
    if not match:
        raise ValueError(f"Expected a release version such as 1.2.3, not {version!r}")
    major, minor, patch = (int(part) for part in match.groups())
    if major > 255 or minor > 255 or patch > 65535:
        raise ValueError(f"Windows Installer cannot compare version {version}")
    return f"{major}.{minor}.{patch}"


def pe_machine(path):
    """The machine type in a Windows executable's PE header."""
    with open(path, "rb") as stream:
        dos = stream.read(64)
        if len(dos) < 64 or dos[:2] != b"MZ":
            raise ValueError(f"{path} is not a Windows executable")
        stream.seek(struct.unpack_from("<I", dos, 0x3C)[0])
        header = stream.read(6)
    if len(header) < 6 or header[:4] != b"PE\0\0":
        raise ValueError(f"{path} has no PE header")
    return struct.unpack_from("<H", header, 4)[0]


def file_version(path):
    """The first three numbers of an executable's VERSIONINFO, or None."""
    import ctypes
    from ctypes import wintypes

    api = ctypes.WinDLL("version")
    api.GetFileVersionInfoSizeW.argtypes = [wintypes.LPCWSTR, wintypes.LPDWORD]
    api.GetFileVersionInfoSizeW.restype = wintypes.DWORD
    api.GetFileVersionInfoW.argtypes = [wintypes.LPCWSTR, wintypes.DWORD, wintypes.DWORD, ctypes.c_void_p]
    api.GetFileVersionInfoW.restype = wintypes.BOOL
    api.VerQueryValueW.argtypes = [
        ctypes.c_void_p, wintypes.LPCWSTR, ctypes.POINTER(ctypes.c_void_p), ctypes.POINTER(wintypes.UINT),
    ]
    api.VerQueryValueW.restype = wintypes.BOOL
    size = api.GetFileVersionInfoSizeW(str(path), None)
    if not size:
        return None
    data = ctypes.create_string_buffer(size)
    fixed = ctypes.c_void_p()
    length = wintypes.UINT()
    if not api.GetFileVersionInfoW(str(path), 0, size, data):
        return None
    if not api.VerQueryValueW(data, "\\", ctypes.byref(fixed), ctypes.byref(length)) or length.value < 16:
        return None
    signature, _, high, low = struct.unpack("<IIII", ctypes.string_at(fixed.value, 16))
    if signature != 0xFEEF04BD:
        return None
    return f"{high >> 16}.{high & 0xFFFF}.{low >> 16}"


def license_rtf(text):
    """LICENSE as the rich text WixUI's license page shows."""
    body = []
    for character in text.replace("\r\n", "\n"):
        if character in "\\{}":
            body.append("\\" + character)
        elif character == "\n":
            body.append("\\par\n")
        elif ord(character) > 127:
            units = struct.unpack(f"<{len(character.encode('utf-16-le')) // 2}h", character.encode("utf-16-le"))
            body.extend(f"\\u{unit}?" for unit in units)
        else:
            body.append(character)
    return (
        "{\\rtf1\\ansi\\ansicpg1252\\deff0\\uc1{\\fonttbl{\\f0\\fswiss\\fcharset0 Segoe UI;}}\\f0\\fs18\n"
        + "".join(body)
        + "}\n"
    )


def run(command, cwd=None):
    """Runs a tool and shows its output; a failure ends the build with it."""
    try:
        result = subprocess.run(
            [str(part) for part in command], cwd=cwd, capture_output=True, text=True, errors="replace",
        )
    except FileNotFoundError:
        raise SystemExit(f"{command[0]} is not on PATH; see build-msi.py for what the build needs") from None
    sys.stdout.write(result.stdout)
    if result.returncode:
        # native-packages shows standard error only.
        sys.stderr.write(result.stdout + result.stderr)
        raise SystemExit(f"{Path(command[0]).name} failed with exit code {result.returncode}")


def build(payload, package, version, arch, defines=()):
    payload = Path(payload).resolve()
    package = Path(package).resolve()
    wix_arch, machine = ARCHES[arch]
    numeric = numeric_version(version)
    for name in PAYLOAD:
        if not (payload / name).is_file():
            raise SystemExit(f"{payload} has no {name}")
    executable = payload / "spotifast.exe"
    if pe_machine(executable) != machine:
        raise SystemExit(f"{executable} is not built for {arch}")
    # Windows Installer replaces a file only for a newer version, and an
    # update that kept the old executable would roll back.
    found = file_version(executable)
    if found is None:
        raise SystemExit(f"{executable} has no version resource (see build.rs)")
    if found != numeric:
        raise SystemExit(f"{executable} is version {found}, not {numeric}")
    variables = {}
    for define in defines:
        name, separator, value = define.partition("=")
        if not separator or not re.fullmatch(r"[A-Za-z][A-Za-z0-9]*", name) or name in BUILT:
            raise SystemExit(f"Expected NAME=VALUE for the package's identity, not {define!r}")
        variables[name] = value

    with tempfile.TemporaryDirectory(prefix="spotifast-msi-") as directory:
        work = Path(directory)
        actions = work / "custom-actions.dll"
        # Objects land in the working folder (cwd). No /Fo folder: its closing
        # backslash would escape the quote MSVC reads around a path with a space.
        run(["cl", "/nologo", "/W4", "/WX", "/O1", "/MT", "/guard:cf", "/LD", *(HERE / name for name in ACTIONS),
             f"/Fe{actions}", "/link", "/guard:cf", "msi.lib"], cwd=work)
        if pe_machine(actions) != machine:
            raise SystemExit(f"The MSVC environment does not build for {arch}")
        rtf = work / "license.rtf"
        rtf.write_text(license_rtf((payload / "LICENSE").read_text(encoding="utf-8")), encoding="ascii")
        for extension in EXTENSIONS:
            run(["wix", "extension", "add", "-g", extension])
        variables.update(
            Version=numeric, Payload=payload, CustomActions=actions, LicenseRtf=rtf, Icon=HERE / "spotifast.ico",
        )
        command = ["wix", "build", HERE / "spotifast.wxs", "-arch", wix_arch]
        for extension in EXTENSIONS:
            command += ["-ext", extension]
        for name, value in variables.items():
            command += ["-d", f"{name}={value}"]
        # No .wixpdb beside the package: native-packages wants it alone.
        command += ["-intermediatefolder", work / "obj", "-pdbtype", "none", "-o", package]
        run(command, cwd=work)
    if not package.is_file():
        raise SystemExit(f"WiX did not write {package}")


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("payload", type=Path)
    parser.add_argument("package", type=Path)
    parser.add_argument("version")
    parser.add_argument("arch", choices=ARCHES)
    parser.add_argument("defines", nargs="*", metavar="NAME=VALUE")
    args = parser.parse_args(argv)
    build(args.payload, args.package, args.version, args.arch, args.defines)


if __name__ == "__main__":
    main()

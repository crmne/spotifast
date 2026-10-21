import hashlib
import importlib.util
from pathlib import Path
import struct
import tempfile
import sys
import tomllib
import unittest

sys.dont_write_bytecode = True

spec = importlib.util.spec_from_file_location("release_names", Path(__file__).with_name("release-names.py"))
release_names = importlib.util.module_from_spec(spec)
spec.loader.exec_module(release_names)


class ReleaseNamesTest(unittest.TestCase):
    def test_current_version_has_written_release_notes_before_tagging(self):
        root = Path(__file__).resolve().parent.parent
        version = tomllib.loads((root / "Cargo.toml").read_text())["package"]["version"]
        notes = root / "packaging/release-notes" / f"v{version}.md"
        self.assertTrue(notes.is_file(), f"Write {notes} before tagging the release")
        self.assertTrue(notes.read_text().strip(), "Release notes must not be empty")

    def test_checksums_cover_every_download_and_are_repeatable(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "spotifast-v0.12.0-linux.tar.gz").write_bytes(b"Linux build")
            (root / "spotifast-v0.12.0-macos.dmg").write_bytes(b"Mac build")
            (root / "spotifast-v0.11.2-linux.tar.gz").write_bytes(b"another release")
            release_names.prepare(root, "v0.12.0")
            before = (root / "checksums.txt").read_bytes()
            names = [line.split()[1] for line in before.decode().splitlines()]
            self.assertEqual(names, ["spotifast-v0.12.0-linux.tar.gz", "spotifast-v0.12.0-macos.dmg"])
            release_names.prepare(root, "v0.12.0")
            self.assertEqual((root / "checksums.txt").read_bytes(), before)

    def test_links_are_refused(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "build").write_bytes(b"build")
            (root / "spotifast-v0.12.0-linux.tar.gz").symlink_to(root / "build")
            with self.assertRaisesRegex(ValueError, "regular release file"):
                release_names.prepare(root, "v0.12.0")
            self.assertFalse((root / "checksums.txt").exists())

    def test_missing_inputs_fail(self):
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaisesRegex(ValueError, "No release"):
                release_names.prepare(Path(directory), "v0.12.0")

    def test_appimages_and_their_zsync_files_are_checked_and_listed(self):
        with tempfile.TemporaryDirectory() as directory:
            root = release(Path(directory))
            release_names.prepare(root, "v0.14.0")
            names = [line.split()[1] for line in (root / "checksums.txt").read_text().splitlines()]
            self.assertEqual(
                names,
                [
                    "spotifast-0.14.0-aarch64.AppImage",
                    "spotifast-0.14.0-aarch64.AppImage.zsync",
                    "spotifast-0.14.0-x86_64.AppImage",
                    "spotifast-0.14.0-x86_64.AppImage.zsync",
                    "spotifast-v0.14.0-aarch64-unknown-linux-gnu.tar.gz",
                    "spotifast-v0.14.0-x86_64-unknown-linux-gnu.tar.gz",
                ],
            )

    def test_a_wrong_or_missing_update_information_fails(self):
        for arch, information, message in [
            ("x86_64", release_names.update_information("aarch64"), "carries"),
            ("x86_64", "gh-releases-zsync|crmne|spotifast|latest|spotifast-*-x86_64.AppImage", "carries"),
            ("aarch64", "", "carries ''"),
        ]:
            with tempfile.TemporaryDirectory() as directory:
                root = release(Path(directory))
                appimage(root / f"spotifast-0.14.0-{arch}.AppImage", information)
                with self.assertRaisesRegex(ValueError, message):
                    release_names.prepare(root, "v0.14.0")
                self.assertFalse((root / "checksums.txt").exists())

    def test_every_appimage_needs_a_zsync_file_that_describes_it(self):
        with tempfile.TemporaryDirectory() as directory:
            root = release(Path(directory))
            (root / "spotifast-0.14.0-x86_64.AppImage.zsync").unlink()
            with self.assertRaisesRegex(ValueError, "together"):
                release_names.prepare(root, "v0.14.0")
        with tempfile.TemporaryDirectory() as directory:
            root = release(Path(directory))
            with (root / "spotifast-0.14.0-aarch64.AppImage").open("ab") as stream:
                stream.write(b"changed after zsyncmake")
            with self.assertRaisesRegex(ValueError, "does not describe .*: Length, SHA-1"):
                release_names.prepare(root, "v0.14.0")

    def test_a_stable_release_has_an_appimage_for_each_linux_archive(self):
        with tempfile.TemporaryDirectory() as directory:
            root = release(Path(directory))
            for name in ["spotifast-0.14.0-aarch64.AppImage", "spotifast-0.14.0-aarch64.AppImage.zsync"]:
                (root / name).unlink()
            with self.assertRaisesRegex(ValueError, "beside the aarch64-unknown-linux-gnu archive"):
                release_names.prepare(root, "v0.14.0")
        # A prerelease publishes none.
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "spotifast-v0.14.0-rc.1-x86_64-unknown-linux-gnu.tar.gz").write_bytes(b"Linux")
            release_names.prepare(root, "v0.14.0-rc.1")
            self.assertEqual(len((root / "checksums.txt").read_text().splitlines()), 1)


def appimage(path, information):
    """A file whose ELF64 section table has an .upd_info section, as the
    AppImage runtime does, holding `information`."""
    names = b"\0.upd_info\0.shstrtab\0"
    payload = information.encode().ljust(1024, b"\0")
    data_offset = 64
    names_offset = data_offset + len(payload)
    table = names_offset + len(names)

    def section(name, offset, size):
        return struct.pack("<IIQQQQIIQQ", name, 1, 0, 0, offset, size, 0, 0, 1, 0)

    header = b"\x7fELF" + bytes([2, 1, 1]) + bytes(9)
    header += struct.pack("<HHIQQQIHHHHHH", 2, 62, 1, 0, 0, table, 0, 64, 0, 0, 64, 3, 2)
    sections = section(0, 0, 0) + section(1, data_offset, 1024) + section(11, names_offset, len(names))
    path.write_bytes(header + payload + names + sections + b"squashfs image")


def zsync(appimage_path):
    """The header zsyncmake writes for `appimage_path`, and some block data."""
    data = appimage_path.read_bytes()
    header = (
        "zsync: 0.6.2\n"
        f"Filename: {appimage_path.name}\n"
        "MTime: Thu, 01 Jan 2026 00:00:00 +0000\n"
        "Blocksize: 2048\n"
        f"Length: {len(data)}\n"
        "Hash-Lengths: 2,2,4\n"
        f"URL: {appimage_path.name}\n"
        f"SHA-1: {hashlib.sha1(data).hexdigest()}\n\n"
    )
    Path(f"{appimage_path}.zsync").write_bytes(header.encode() + b"\x00\x01blocks")


def release(root):
    """The Linux downloads of a stable release: both archives, and both
    AppImages with their update information and .zsync files."""
    for arch in ["x86_64", "aarch64"]:
        (root / f"spotifast-v0.14.0-{arch}-unknown-linux-gnu.tar.gz").write_bytes(b"Linux " + arch.encode())
        path = root / f"spotifast-0.14.0-{arch}.AppImage"
        appimage(path, release_names.update_information(arch))
        zsync(path)
    # Another release's AppImage is not listed.
    appimage(root / "spotifast-0.13.3-x86_64.AppImage", release_names.update_information("x86_64"))
    return root


if __name__ == "__main__":
    unittest.main()

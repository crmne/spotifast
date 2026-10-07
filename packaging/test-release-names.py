import importlib.util
from pathlib import Path
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
            (root / "spotizgeg-v0.12.0-linux.tar.gz").write_bytes(b"Linux build")
            (root / "spotizgeg-v0.12.0-macos.dmg").write_bytes(b"Mac build")
            (root / "spotizgeg-v0.11.2-linux.tar.gz").write_bytes(b"another release")
            release_names.prepare(root, "v0.12.0")
            before = (root / "checksums.txt").read_bytes()
            names = [line.split()[1] for line in before.decode().splitlines()]
            self.assertEqual(names, ["spotizgeg-v0.12.0-linux.tar.gz", "spotizgeg-v0.12.0-macos.dmg"])
            release_names.prepare(root, "v0.12.0")
            self.assertEqual((root / "checksums.txt").read_bytes(), before)

    def test_links_are_refused(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "build").write_bytes(b"build")
            (root / "spotizgeg-v0.12.0-linux.tar.gz").symlink_to(root / "build")
            with self.assertRaisesRegex(ValueError, "regular release file"):
                release_names.prepare(root, "v0.12.0")
            self.assertFalse((root / "checksums.txt").exists())

    def test_missing_inputs_fail(self):
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaisesRegex(ValueError, "No release"):
                release_names.prepare(Path(directory), "v0.12.0")


if __name__ == "__main__":
    unittest.main()

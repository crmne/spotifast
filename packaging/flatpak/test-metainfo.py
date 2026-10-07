#!/usr/bin/env python3
"""The packaged metainfo must be the release's own, for that version."""

import importlib.util
from pathlib import Path
import os
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch
import xml.etree.ElementTree as ET

sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location(
    "prepare_metainfo", Path(__file__).with_name("prepare-metainfo.py")
)
metainfo = importlib.util.module_from_spec(spec)
spec.loader.exec_module(metainfo)


class ReleaseMetainfoTests(unittest.TestCase):
    def fixture(self, app_id=metainfo.APP_ID, version="0.8.0"):
        return f"""<component type="desktop-application">
          <id>{app_id}</id><name>Spotizgeg</name>
          <launchable type="desktop-id">{app_id}.desktop</launchable>
          <description><p>Existing release description.</p></description>
          <releases><release version="{version}" date="2026-09-14" /></releases>
        </component>"""

    def test_keeps_the_release_details(self):
        result = ET.fromstring(metainfo.check_metainfo(self.fixture(), "0.8.0"))
        self.assertEqual(result.findtext("id"), metainfo.APP_ID)
        self.assertEqual(result.findtext("launchable"), metainfo.APP_ID + ".desktop")
        self.assertEqual(result.findtext("name"), "Spotizgeg")
        self.assertEqual(result.findtext("description/p"), "Existing release description.")
        self.assertEqual(result.find("releases/release").attrib,
                         {"version": "0.8.0", "date": "2026-09-14"})

    def test_checking_is_idempotent(self):
        first = metainfo.check_metainfo(self.fixture(), "0.8.0")
        self.assertEqual(metainfo.check_metainfo(first, "0.8.0"), first)

    def test_rejects_another_version_or_application(self):
        for contents in [self.fixture(version="0.8.1"), self.fixture("org.example.App"),
                         self.fixture().replace(metainfo.APP_ID + ".desktop", "wrong.desktop")]:
            with self.subTest(contents=contents), self.assertRaises(ValueError):
                metainfo.check_metainfo(contents, "0.8.0")

    def test_reads_the_tag_when_the_checkout_owner_differs(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            original = root / "packaging/flatpak" / (metainfo.APP_ID + ".metainfo.xml")
            original.parent.mkdir(parents=True)
            original.write_text(self.fixture())
            def git(*args):
                return subprocess.check_output(["git", *args], cwd=root, stderr=subprocess.DEVNULL)
            git("init")
            git("add", ".")
            git("-c", "user.name=Metadata Test", "-c", "user.email=test@example.invalid",
                "-c", "commit.gpgsign=false", "commit", "-m", "Release fixture")
            git("tag", "v0.8.0")
            # The working copy differs; preparation must still read the tag.
            original.write_text(self.fixture(version="0.8.1"))
            with patch.dict(os.environ, {"GIT_TEST_ASSUME_DIFFERENT_OWNER": "1"}):
                result = metainfo.release_metainfo(root, "v0.8.0")
            component = ET.fromstring(result)
            self.assertEqual(component.findtext("id"), metainfo.APP_ID)
            self.assertEqual(component.find("releases/release").get("version"), "0.8.0")


if __name__ == "__main__":
    unittest.main()

"""Run the uninstall script against disposable Windows installations.

Usage: python packaging/windows/test-uninstall.py path/to/ISCC.exe
Requires rustc to build a harmless waiting process, without Spotify access.
"""
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time
import unittest
import uuid


ROOT = Path(__file__).resolve().parent
ISCC = sys.argv.pop(1) if len(sys.argv) > 1 else shutil.which("iscc")
FLAGS = ["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART"]


@unittest.skipUnless(sys.platform == "win32" and ISCC, "requires Windows and ISCC")
class UninstallTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.temp = tempfile.TemporaryDirectory(prefix="spotifast-uninstall-")
        cls.addClassCleanup(cls.temp.cleanup)
        cls.root = Path(cls.temp.name)
        cls.identity = "SpotifastUninstallTest-" + uuid.uuid4().hex
        payload = cls.root / "spotifast.exe"
        source = cls.root / "payload.rs"
        source.write_text('''fn main() {
    std::fs::write(std::env::args().nth(1).unwrap(), "ready").unwrap();
    loop { std::thread::park(); }
}
''')
        subprocess.run(["rustc", str(source), "-o", str(payload)], check=True, timeout=60)
        (cls.root / "keep.txt").write_text("Must survive a blocked uninstall.")
        # Compile the production event handlers with an isolated app identity.
        # Redirect their protocol lookup too, so no real Spotify key is touched.
        code = (ROOT / "spotifast.iss").read_text(encoding="utf-8").split("[Code]", 1)[1]
        code = code.replace("Software\\Classes\\spotify", "Software\\Classes\\" + cls.identity)
        script = cls.root / "test.iss"
        script.write_text(f'''#define Arch "x86_64"
#define AppExeName "spotifast.exe"
[Setup]
AppId={cls.identity}
AppName={cls.identity}
AppVersion=1.0
DefaultDirName={cls.root / 'installed'}
PrivilegesRequired=lowest
Uninstallable=yes
OutputDir={cls.root}
OutputBaseFilename=setup
[Files]
Source: "{payload}"; DestDir: "{{app}}"
Source: "{cls.root / 'keep.txt'}"; DestDir: "{{app}}"
[Code]
{code}
''', encoding="utf-8")
        subprocess.run([ISCC, "/Q", str(script)], check=True, timeout=60)

    def setUp(self):
        self.app = self.root / uuid.uuid4().hex
        self.processes = []
        subprocess.run([str(self.root / "setup.exe"), *FLAGS, f"/DIR={self.app}"], check=True, timeout=60)

    def tearDown(self):
        for process in self.processes:
            process.terminate()
            process.wait(timeout=10)
        if (self.app / "unins000.exe").exists():
            self.assertEqual(self.uninstall(), 0)

    def uninstall(self):
        log = self.root / (uuid.uuid4().hex + ".log")
        result = subprocess.run(
            [str(self.app / "unins000.exe"), *FLAGS, f"/LOG={log}"], timeout=60,
        ).returncode
        # The uninstaller can finish deleting its own files after returning.
        if result == 0:
            deadline = time.monotonic() + 10
            while (self.app / "unins000.exe").exists() and time.monotonic() < deadline:
                time.sleep(0.01)
            self.assertFalse((self.app / "unins000.exe").exists())
        return result

    def run_payload(self, path):
        ready = self.root / (uuid.uuid4().hex + ".ready")
        process = subprocess.Popen(
            [str(path), str(ready)], stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL, creationflags=subprocess.CREATE_NO_WINDOW,
        )
        self.processes.append(process)
        deadline = time.monotonic() + 10
        while not ready.exists() and time.monotonic() < deadline:
            self.assertIsNone(process.poll())
            time.sleep(0.01)
        self.assertTrue(ready.exists())
        self.assertIsNone(process.poll())

    def test_running_executable_preserves_the_whole_installation(self):
        self.run_payload(self.app / "spotifast.exe")
        before = {p.name: p.read_bytes() for p in self.app.iterdir() if p.is_file()}
        self.assertNotEqual(self.uninstall(), 0)
        after = {p.name: p.read_bytes() for p in self.app.iterdir() if p.is_file()}
        self.assertEqual(before, after)
        self.assertIsNone(self.processes[0].poll())

    def test_other_copy_does_not_block_uninstall(self):
        self.run_payload(self.root / "spotifast.exe")
        self.assertEqual(self.uninstall(), 0)
        self.assertFalse((self.app / "spotifast.exe").exists())
        self.assertFalse((self.app / "keep.txt").exists())

    def test_missing_executable_does_not_block_cleanup(self):
        (self.app / "spotifast.exe").unlink()
        self.assertEqual(self.uninstall(), 0)
        self.assertFalse((self.app / "keep.txt").exists())


if __name__ == "__main__":
    unittest.main()

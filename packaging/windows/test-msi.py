"""Install, update and remove disposable copies of the Windows Installer package.

Usage: python packaging/windows/test-msi.py [path/to/ISCC.exe]

Runs on Windows with what build-msi.py needs (WiX 5 and the MSVC tools for
this machine, which also build a harmless stand-in for spotifast.exe that
never reaches Spotify). Every package it builds has an identity of its own:
another upgrade code, link scheme, registry keys, folder and shortcut names,
so nothing it does can touch a real Spotifast installation. Inno Setup's
ISCC is needed only for the takeover tests, which are skipped without it.
"""

from pathlib import Path
import ctypes
import importlib.util
import os
import shutil
import subprocess
import sys
import tempfile
import time
import unittest
import uuid

if sys.platform == "win32":
    import winreg

sys.dont_write_bytecode = True

HERE = Path(__file__).resolve().parent
REPO = HERE.parent.parent
ISCC = sys.argv.pop(1) if len(sys.argv) > 1 and not sys.argv[1].startswith("-") else shutil.which("iscc")
if not ISCC and sys.platform == "win32":
    default = Path(os.environ.get("ProgramFiles(x86)", r"C:\Program Files (x86)"), "Inno Setup 6", "ISCC.exe")
    ISCC = str(default) if default.is_file() else None

spec = importlib.util.spec_from_file_location("build_msi", HERE / "build-msi.py")
build_msi = importlib.util.module_from_spec(spec)
spec.loader.exec_module(build_msi)

IDENTITY = "SpotifastTest" + uuid.uuid4().hex[:8]
SCHEME = IDENTITY.lower()
UPGRADE_CODE = "{%s}" % str(uuid.uuid4()).upper()
INNO_APP_ID = "{%s}" % str(uuid.uuid4()).upper()
DEFINES = [
    f"Identity={IDENTITY}",
    f"LinkScheme={SCHEME}",
    f"UpgradeCode={UPGRADE_CODE}",
    "ProgramComponent={%s}" % str(uuid.uuid4()).upper(),
    f"InnoAppId={INNO_APP_ID}",
]
INSTALLER = rf"Software\{IDENTITY}\Installer"
INNO_KEY = rf"Software\Microsoft\Windows\CurrentVersion\Uninstall\{INNO_APP_ID}_is1"
LINK_COMMAND = rf"Software\Classes\{SCHEME}\shell\open\command"
OWN_LINK_COMMAND = rf"Software\Classes\{IDENTITY}.spotify\shell\open\command"
FIRST, SECOND = "1.0.0", "1.0.1"
FILES = {"spotifast.exe", "README.md", "LICENSE", "spotifast-installer.txt"}

PAYLOAD_SOURCE = r"""
#include <stdio.h>
#include <windows.h>
/* Says it is running, then waits to be ended. */
int wmain(int argc, wchar_t **argv)
{
    FILE *ready = argc > 1 ? _wfopen(argv[1], L"w") : NULL;
    if (ready) {
        fputs("ready", ready);
        fclose(ready);
    }
    for (;;) {
        Sleep(INFINITE);
    }
}
"""


def architecture():
    """What the MSVC environment builds for, as build-msi.py names it."""
    target = os.environ.get("VSCMD_ARG_TGT_ARCH", "").lower()
    if target in ("x64", "arm64"):
        return "amd64" if target == "x64" else "arm64"
    return "arm64" if os.environ.get("PROCESSOR_ARCHITECTURE", "").upper() == "ARM64" else "amd64"


def build_payload(folder, version):
    """A stand-in spotifast.exe carrying VERSION in its version resource."""
    folder.mkdir(parents=True)
    numbers = version.replace(".", ",") + ",0"
    (folder / "payload.c").write_text(PAYLOAD_SOURCE)
    (folder / "payload.rc").write_text(f"""1 VERSIONINFO
FILEVERSION {numbers}
PRODUCTVERSION {numbers}
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904b0"
    BEGIN
      VALUE "FileVersion", "{version}"
      VALUE "ProductName", "Spotifast test stand-in"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
""")
    for command in (
        ["rc", "/nologo", "/fo", "payload.res", "payload.rc"],
        ["cl", "/nologo", "/O1", "payload.c", "payload.res", "/Fespotifast.exe"],
    ):
        subprocess.run(command, cwd=folder, check=True, timeout=120)
    payload = folder / "payload"
    payload.mkdir()
    shutil.copy2(folder / "spotifast.exe", payload)
    for name in ("README.md", "LICENSE"):
        shutil.copy2(REPO / name, payload)
    shutil.copy2(HERE / "spotifast-installer.txt", payload)
    return payload


def build_inno_setup(root, binary):
    """spotifast.iss as it ships, under the test identity."""
    script = (HERE / "spotifast.iss").read_text(encoding="utf-8-sig")
    for old, new in (
        ('#define AppName "Spotifast"', f'#define AppName "{IDENTITY}"'),
        ('#define AppIdentity "Spotifast"', f'#define AppIdentity "{IDENTITY}"'),
        ("AppId={{FCED1EA0-EBF5-4C32-BA3B-A3AD724BACC3}", "AppId={" + INNO_APP_ID),
        ("Spotifast.spotify", f"{IDENTITY}.spotify"),
        ("Software\\Classes\\spotify", f"Software\\Classes\\{SCHEME}"),
        ('ValueName: "spotify"', f'ValueName: "{SCHEME}"'),
        ("[Setup]\n", f"[Setup]\nSourceDir={HERE}\n"),
    ):
        if old not in script:
            raise AssertionError(f"spotifast.iss no longer contains {old!r}")
        script = script.replace(old, new)
    path = root / "inno.iss"
    path.write_text(script, encoding="utf-8-sig")
    arch = "aarch64" if architecture() == "arm64" else "x86_64"
    subprocess.run(
        [ISCC, "/Q", f"/DVersion={FIRST}", f"/DArch={arch}", f"/DBinary={binary}", f"/DOutputDir={root}", str(path)],
        check=True, timeout=300,
    )
    (setup,) = root.glob("*-setup.exe")
    return setup


def read_log(path):
    data = path.read_bytes() if path.exists() else b""
    return data.decode("utf-16" if data.startswith(b"\xff\xfe") else "mbcs", errors="replace")


def value(key, name=""):
    try:
        with winreg.OpenKey(winreg.HKEY_CURRENT_USER, key) as handle:
            return winreg.QueryValueEx(handle, name)[0]
    except FileNotFoundError:
        return None


def key_exists(key):
    try:
        winreg.OpenKey(winreg.HKEY_CURRENT_USER, key).Close()
        return True
    except FileNotFoundError:
        return False


def set_value(key, name, data):
    with winreg.CreateKey(winreg.HKEY_CURRENT_USER, key) as handle:
        winreg.SetValueEx(handle, name, 0, winreg.REG_SZ, data)


def delete_tree(key):
    try:
        with winreg.OpenKey(winreg.HKEY_CURRENT_USER, key) as handle:
            while True:
                try:
                    child = winreg.EnumKey(handle, 0)
                except OSError:
                    break
                delete_tree(rf"{key}\{child}")
        winreg.DeleteKey(winreg.HKEY_CURRENT_USER, key)
    except FileNotFoundError:
        pass


def delete_value(key, name):
    try:
        with winreg.OpenKey(winreg.HKEY_CURRENT_USER, key, 0, winreg.KEY_SET_VALUE) as handle:
            winreg.DeleteValue(handle, name)
    except FileNotFoundError:
        pass


def installed_products():
    """Product codes Windows Installer has for the test's upgrade code."""
    msi = ctypes.WinDLL("msi")
    products = []
    while True:
        product = ctypes.create_unicode_buffer(39)
        if msi.MsiEnumRelatedProductsW(UPGRADE_CODE, 0, len(products), product) != 0:
            return products
        products.append(product.value)


def known_folder(csidl):
    path = ctypes.create_unicode_buffer(260)
    ctypes.windll.shell32.SHGetFolderPathW(None, csidl, None, 0, path)
    return Path(path.value)


@unittest.skipUnless(sys.platform == "win32", "requires Windows")
class PackageTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.temp = tempfile.TemporaryDirectory(prefix="spotifast-msi-test-")
        cls.addClassCleanup(cls.temp.cleanup)
        # The long form: a runner's TEMP is an 8.3 path.
        cls.root = Path(cls.temp.name).resolve()
        cls.payloads = {}
        cls.packages = {}
        for version in (FIRST, SECOND):
            cls.payloads[version] = build_payload(cls.root / f"payload-{version}", version)
            cls.packages[version] = cls.root / f"{IDENTITY}-{version}.msi"
            build_msi.build(cls.payloads[version], cls.packages[version], version, architecture(), DEFINES)
        cls.inno = build_inno_setup(cls.root, cls.payloads[FIRST] / "spotifast.exe") if ISCC else None
        cls.start_menu = Path(os.environ["APPDATA"], "Microsoft", "Windows", "Start Menu", "Programs", f"{IDENTITY}.lnk")
        cls.desktop = known_folder(0x10) / f"{IDENTITY}.lnk"  # CSIDL_DESKTOPDIRECTORY
        cls.default_folder = Path(os.environ["LOCALAPPDATA"], "Programs", IDENTITY)

    def setUp(self):
        # A space, as in most real profile paths, exercises the quoting.
        self.folder = self.root / uuid.uuid4().hex / "Spotifast Test"
        self.processes = []
        self.logs = []
        self.addCleanup(self.clean_up)

    def clean_up(self):
        for process in self.processes:
            if process.poll() is None:
                process.terminate()
            process.wait(timeout=10)
        for product in installed_products():
            self.msiexec("/x", product)
        uninstaller = self.folder / "unins000.exe"
        if uninstaller.exists():
            subprocess.run([str(uninstaller), "/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART"], timeout=120)
            # Inno's uninstaller finishes after it returns; let it, before the
            # next test makes shortcuts with the same names.
            deadline = time.monotonic() + 30
            while uninstaller.exists() and time.monotonic() < deadline:
                time.sleep(0.1)
        for key in (rf"Software\Classes\{SCHEME}", rf"Software\Classes\{IDENTITY}.spotify", rf"Software\{IDENTITY}", INNO_KEY):
            delete_tree(key)
        delete_value(r"Software\RegisteredApplications", IDENTITY)
        for shortcut in (self.start_menu, self.desktop):
            shortcut.unlink(missing_ok=True)

    def msiexec(self, action, target, **properties):
        """Runs msiexec silently and returns its exit code. The command line
        is the one fastframe-update's helper writes: each value in quotes
        after its name, as msiexec reads them."""
        log = self.root / f"{uuid.uuid4().hex}.log"
        self.logs.append(log)
        msiexec = Path(os.environ["SystemRoot"], "System32", "msiexec.exe")
        command = f'"{msiexec}" {action} "{target}" /qn /norestart /l*v "{log}"'
        command += "".join(f' {name}="{value}"' for name, value in properties.items())
        for _ in range(30):
            code = subprocess.run(command, timeout=600).returncode
            if code != 1618:  # Another installation is still finishing.
                return code
            time.sleep(2)
        return code

    def assertSucceeded(self, code):
        if code != 0:
            self.fail(f"msiexec returned {code}\n" + read_log(self.logs[-1])[-20000:])

    def install(self, version, **properties):
        return self.msiexec("/i", self.packages[version], **properties)

    def install_like_the_updater(self, version):
        return self.install(version, INSTALLFOLDER=self.folder)

    def uninstall(self, version):
        return self.msiexec("/x", self.packages[version])

    def start(self, executable):
        ready = self.root / f"{uuid.uuid4().hex}.ready"
        process = subprocess.Popen(
            [str(executable), str(ready)], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
            creationflags=subprocess.CREATE_NO_WINDOW,
        )
        self.processes.append(process)
        deadline = time.monotonic() + 10
        while not ready.exists() and time.monotonic() < deadline:
            self.assertIsNone(process.poll())
            time.sleep(0.01)
        self.assertTrue(ready.exists())
        return process

    def portable_copy(self):
        folder = self.root / uuid.uuid4().hex / "portable"
        folder.mkdir(parents=True)
        return Path(shutil.copy2(self.payloads[FIRST] / "spotifast.exe", folder))

    def contents(self):
        return {path.name: path.read_bytes() for path in self.folder.iterdir() if path.is_file()}

    def assertInstalled(self, version):
        self.assertEqual({path.name for path in self.folder.iterdir() if path.is_file()}, FILES)
        self.assertEqual(
            (self.folder / "spotifast.exe").read_bytes(),
            (self.payloads[version] / "spotifast.exe").read_bytes(),
        )
        self.assertEqual((self.folder / "spotifast-installer.txt").read_text().strip(), "spotifast-installer-v1")
        self.assertEqual(len(installed_products()), 1)
        self.assertEqual(value(INSTALLER, "InstallFolder"), f"{self.folder}\\")
        command = f'"{self.folder}\\spotifast.exe" "%1"'
        self.assertEqual(value(LINK_COMMAND), command)
        self.assertEqual(value(OWN_LINK_COMMAND), command)
        self.assertEqual(value(rf"Software\{IDENTITY}\Capabilities\URLAssociations", SCHEME), f"{IDENTITY}.spotify")
        self.assertEqual(value(r"Software\RegisteredApplications", IDENTITY), rf"Software\{IDENTITY}\Capabilities")
        self.assertTrue(self.start_menu.exists())
        self.assertFalse(key_exists(INNO_KEY))
        self.assertFalse(self.default_folder.exists())

    def stage_update(self, name="0123456789abcdef"):
        """A staging folder as fastframe-update leaves one: its helper and
        the previous version, some of it nested and read-only."""
        staging = self.folder / f".spotifast-update-{name}"
        (staging / "previous").mkdir(parents=True)
        (staging / "helper.exe").write_bytes(b"helper")
        (staging / "result.txt").write_text("Updated to 1.0.0")
        backup = staging / "previous" / "spotifast.exe"
        backup.write_bytes(b"previous")
        os.chmod(backup, 0o444)
        return staging

    def hold(self, path):
        """Keeps PATH open, as a running helper keeps its own file: Windows
        refuses to delete it meanwhile."""
        handle = open(path, "rb")
        self.addCleanup(handle.close)
        return handle

    def assertFolderGone(self):
        if self.folder.exists():
            left = sorted(str(path.relative_to(self.folder)) for path in self.folder.rglob("*"))
            self.fail(f"{self.folder} still holds {left}\n" + read_log(self.logs[-1])[-20000:])

    def assertRemoved(self):
        self.assertFalse(self.folder.exists() and any(p.name in FILES for p in self.folder.iterdir()))
        self.assertEqual(installed_products(), [])
        for key in (LINK_COMMAND, rf"Software\Classes\{IDENTITY}.spotify", rf"Software\{IDENTITY}"):
            self.assertFalse(key_exists(key), key)
        self.assertIsNone(value(r"Software\RegisteredApplications", IDENTITY))
        self.assertFalse(self.start_menu.exists())
        self.assertFalse(self.desktop.exists())

    def test_installs_where_the_updater_says_and_uninstalls_cleanly(self):
        self.assertSucceeded(self.install_like_the_updater(FIRST))
        self.assertInstalled(FIRST)
        self.assertFalse(self.desktop.exists())
        self.assertSucceeded(self.uninstall(FIRST))
        self.assertRemoved()
        self.assertFolderGone()

    def test_uninstall_removes_what_the_updater_staged(self):
        self.assertSucceeded(self.install_like_the_updater(FIRST))
        self.stage_update()
        self.stage_update("abc")
        self.assertSucceeded(self.uninstall(FIRST))
        self.assertRemoved()
        self.assertFolderGone()

    def test_uninstall_removes_only_staging_folders_and_goes_on_past_a_held_file(self):
        self.assertSucceeded(self.install_like_the_updater(FIRST))
        held = self.stage_update()
        cleared = self.stage_update("abc")
        handle = self.hold(held / "helper.exe")
        mine = self.folder / "notes"
        mine.mkdir()
        (mine / "keep.txt").write_text("not Spotifast's")
        (self.folder / ".spotifast-update-file").write_text("not a folder")
        (self.folder / ".spotifast-updater").mkdir()
        self.assertSucceeded(self.uninstall(FIRST))
        self.assertRemoved()
        self.assertFalse(cleared.exists())
        self.assertEqual({path.name for path in held.rglob("*")}, {"helper.exe"})
        self.assertEqual((mine / "keep.txt").read_text(), "not Spotifast's")
        self.assertTrue((self.folder / ".spotifast-update-file").is_file())
        self.assertTrue((self.folder / ".spotifast-updater").is_dir())
        handle.close()

    def test_update_keeps_what_the_updater_staged(self):
        # The old version is uninstalled inside the update, while the
        # updater's helper runs it from its staging folder.
        self.assertSucceeded(self.install_like_the_updater(FIRST))
        staging = self.stage_update()
        before = {path.relative_to(staging): path.read_bytes() for path in staging.rglob("*") if path.is_file()}
        handle = self.hold(staging / "helper.exe")
        self.assertSucceeded(self.install_like_the_updater(SECOND))
        self.assertInstalled(SECOND)
        self.assertEqual(
            {path.relative_to(staging): path.read_bytes() for path in staging.rglob("*") if path.is_file()},
            before,
        )
        handle.close()
        self.assertSucceeded(self.uninstall(SECOND))
        self.assertRemoved()
        self.assertFolderGone()

    def test_uninstall_leaves_links_another_program_took_over(self):
        self.assertSucceeded(self.install_like_the_updater(FIRST))
        other = r'"C:\Other\Spotify.exe" --protocol-uri="%1"'
        set_value(LINK_COMMAND, "", other)
        self.assertSucceeded(self.uninstall(FIRST))
        self.assertEqual(value(LINK_COMMAND), other)
        self.assertFalse(key_exists(rf"Software\Classes\{IDENTITY}.spotify"))

    def test_running_copy_stops_a_silent_uninstall_before_anything_goes(self):
        self.assertSucceeded(self.install_like_the_updater(FIRST))
        process = self.start(self.folder / "spotifast.exe")
        before = self.contents()
        self.assertNotEqual(self.uninstall(FIRST), 0)
        self.assertEqual(self.contents(), before)
        self.assertEqual(len(installed_products()), 1)
        self.assertTrue(self.start_menu.exists())
        self.assertIsNone(process.poll())

    def test_portable_copy_does_not_block_uninstall(self):
        self.assertSucceeded(self.install_like_the_updater(FIRST))
        portable = self.start(self.portable_copy())
        self.assertSucceeded(self.uninstall(FIRST))
        self.assertRemoved()
        self.assertIsNone(portable.poll())

    def test_missing_executable_does_not_block_uninstall(self):
        self.assertSucceeded(self.install_like_the_updater(FIRST))
        (self.folder / "spotifast.exe").unlink()
        self.assertSucceeded(self.uninstall(FIRST))
        self.assertRemoved()

    def test_update_closes_only_the_installed_copy_and_keeps_choices(self):
        self.assertSucceeded(self.install(FIRST, INSTALLFOLDER=self.folder, DESKTOPSHORTCUT=1))
        self.assertTrue(self.desktop.exists())
        installed = self.start(self.folder / "spotifast.exe")
        portable = self.start(self.portable_copy())
        self.assertSucceeded(self.install_like_the_updater(SECOND))
        installed.wait(timeout=10)
        self.assertIsNone(portable.poll())
        self.assertInstalled(SECOND)
        self.assertTrue(self.desktop.exists())

    def test_package_run_by_hand_updates_the_folder_in_use(self):
        self.assertSucceeded(self.install(FIRST, INSTALLFOLDER=self.folder))
        self.assertSucceeded(self.install(SECOND))
        self.assertInstalled(SECOND)

    def test_older_package_does_not_replace_a_newer_one(self):
        self.assertSucceeded(self.install_like_the_updater(SECOND))
        self.assertNotEqual(self.install_like_the_updater(FIRST), 0)
        self.assertInstalled(SECOND)

    def install_with_inno(self, *tasks):
        if not self.inno:
            self.skipTest("requires ISCC")
        arguments = ["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART", f"/DIR={self.folder}"]
        if tasks:
            arguments.append("/TASKS=" + ",".join(tasks))
        subprocess.run([str(self.inno), *arguments], check=True, timeout=300)
        self.assertTrue((self.folder / "unins000.exe").exists())
        self.assertTrue(key_exists(INNO_KEY))
        self.assertTrue(self.start_menu.exists())
        self.assertEqual(self.desktop.exists(), "desktopicon" in tasks)
        # What the updater leaves beside the app: kept by the takeover.
        return self.stage_update()

    def test_updater_takes_over_an_inno_install(self):
        staging = self.install_with_inno("desktopicon")
        self.assertSucceeded(self.install_like_the_updater(SECOND))
        self.assertInstalled(SECOND)
        self.assertFalse(any(self.folder.glob("unins000.*")))
        self.assertTrue(self.desktop.exists())
        self.assertTrue((staging / "result.txt").exists())
        # The package now owns what Inno Setup put there.
        self.assertSucceeded(self.uninstall(SECOND))
        self.assertRemoved()
        self.assertFolderGone()

    def test_package_run_by_hand_takes_over_an_inno_install_where_it_is(self):
        self.install_with_inno()
        self.assertSucceeded(self.install(SECOND))
        self.assertInstalled(SECOND)
        self.assertFalse(any(self.folder.glob("unins000.*")))
        self.assertFalse(self.desktop.exists())

    def test_takeover_removes_an_inno_desktop_shortcut_no_longer_wanted(self):
        self.install_with_inno("desktopicon")
        self.assertSucceeded(self.install(SECOND, DESKTOPSHORTCUT=0))
        self.assertInstalled(SECOND)
        self.assertFalse(self.desktop.exists())

    def test_running_inno_copy_is_closed_before_the_takeover(self):
        self.install_with_inno()
        installed = self.start(self.folder / "spotifast.exe")
        self.assertSucceeded(self.install_like_the_updater(SECOND))
        installed.wait(timeout=10)
        self.assertInstalled(SECOND)


if __name__ == "__main__":
    unittest.main()

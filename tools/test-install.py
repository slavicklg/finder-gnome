#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Slavick
# SPDX-License-Identifier: MIT
"""Hermetic installer tests: real GPG, fake APT/curl/sudo, paths only in /tmp.

No network requests, privilege escalation or host package installation occurs.
The private signing fixtures are generated in a temporary directory and removed.
"""
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).with_name("install.sh").read_text()
GPG = shutil.which("gpg")
MOCK = r'''#!PYTHON
import json, os, pathlib, shutil, sys
name = pathlib.Path(sys.argv[0]).name
args = sys.argv[1:]
with open(os.environ["TEST_LOG"], "a") as log:
    log.write(json.dumps([name] + args) + "\n")
if name == "dpkg":
    print(os.environ.get("TEST_ARCH", "amd64"))
elif name == "dpkg-query":
    if args[-1] == "ca-certificates":
        print("install ok installed" if os.environ.get("TEST_CA", "1") == "1" else "unknown ok not-installed")
    else:
        print("1.0.0-1ubuntu24.04")
elif name == "curl":
    if os.environ.get("TEST_DOWNLOAD_FAIL") == "1": sys.exit(22)
    shutil.copyfile(os.environ["TEST_KEY"], args[args.index("--output") + 1])
elif name in ("apt-get", "apt"):
    if os.environ.get("TEST_APT_FAIL") == args[0]: sys.exit(42)
    if args[0] == "install":
        for package, tool in [("curl", "curl"), ("gnupg", "gpg")]:
            if package in args:
                target = pathlib.Path(os.environ["TEST_BIN"]) / tool
                if not target.exists():
                    target.symlink_to(os.environ["TEST_GPG"] if tool == "gpg" else "mock")
elif name == "sudo":
    if os.environ.get("TEST_SUDO_DENY") == "1": sys.exit(1)
    if args == ["-v"]: sys.exit(0)
    if args[0] == "--": args.pop(0)
    os.execvp(args[0], args)
elif name == "stat":
    # Model root-owned files when running the suite as an ordinary user.
    assert args[:2] == ["-c", "%u:%g:%a"]
    print("0:0:" + format(pathlib.Path(args[-1]).stat().st_mode & 0o777, "o"))
elif name == "mv":
    sys.exit(43)  # Only installed as a mock in the atomic-write failure test.
else:
    raise SystemExit("Unexpected mocked command: " + name)
'''


class InstallerTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        if not GPG:
            raise unittest.SkipTest("gpg is required for real key-validation tests")
        cls.keys = tempfile.TemporaryDirectory(prefix="finder-installer-keys-")
        cls.home = Path(cls.keys.name)
        cls.home.chmod(0o700)
        cls.gpg = [GPG, "--no-options", "--homedir", str(cls.home), "--batch"]
        cls.fingerprints = []
        for name in ("Fixture", "Wrong key"):
            subprocess.run(cls.gpg + ["--pinentry-mode", "loopback", "--passphrase", "",
                                      "--quick-generate-key", name, "ed25519", "sign", "1d"],
                           check=True, capture_output=True)
        listing = subprocess.check_output(cls.gpg + ["--with-colons", "--list-keys"], stderr=subprocess.DEVNULL).decode()
        cls.fingerprints = [line.split(":")[9] for line in listing.splitlines() if line.startswith("fpr:")]
        cls.armored = cls.home / "public.asc"
        cls.binary = cls.home / "public.gpg"
        cls.wrong = cls.home / "wrong.gpg"
        cls.bundle = cls.home / "bundle.gpg"
        for path, args in [(cls.armored, ["--armor", "--export", cls.fingerprints[0]]),
                           (cls.binary, ["--export", cls.fingerprints[0]]),
                           (cls.wrong, ["--export", cls.fingerprints[1]]),
                           (cls.bundle, ["--export"])]:
            path.write_bytes(subprocess.check_output(cls.gpg + args))

    @classmethod
    def tearDownClass(cls):
        subprocess.run([shutil.which("gpgconf"), "--homedir", str(cls.home), "--kill", "gpg-agent"],
                       check=False, capture_output=True)
        cls.keys.cleanup()

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="finder-installer-test-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.bin = self.root / "bin"
        self.bin.mkdir()
        self.apt = self.root / "etc/apt"
        self.apt.mkdir(parents=True)
        self.os_release = self.root / "etc/os-release"
        self.platform()
        self.script = self.root / "install.sh"
        # Only the private test copy has substituted paths/fingerprint/root ID.
        # The distributed installer has no environment overrides for these.
        patched = "\n".join(
            f"readonly GPG_FINGERPRINT='{self.fingerprints[0]}'" if line.startswith("readonly GPG_FINGERPRINT=") else line
            for line in SCRIPT.splitlines()) + "\n"
        patched = patched.replace("readonly OS_RELEASE_FILE='/etc/os-release'", f"readonly OS_RELEASE_FILE='{self.os_release}'")
        patched = patched.replace("readonly APT_DIR='/etc/apt'", f"readonly APT_DIR='{self.apt}'")
        self.assertIn("if (( EUID != 0 ));", patched)
        patched = patched.replace("if (( EUID != 0 ));", 'if [[ ${TEST_ROOT:-1} != 1 ]];')
        self.script.write_text(patched)
        for tool in ("bash", "awk", "grep", "cat", "cp", "cmp", "install", "mkdir", "mktemp", "mv", "rm"):
            self.bin.joinpath(tool).symlink_to(shutil.which(tool))
        self.bin.joinpath("gpg").symlink_to(GPG)
        mock = self.bin / "mock"
        mock.write_text(MOCK.replace("#!PYTHON", "#!" + os.sys.executable, 1))
        mock.chmod(0o755)
        for tool in ("curl", "apt-get", "apt", "dpkg", "dpkg-query", "sudo", "stat"):
            self.bin.joinpath(tool).symlink_to("mock")
        self.log = self.root / "commands.jsonl"
        self.env = dict(os.environ, PATH=str(self.bin), TEST_LOG=str(self.log), TEST_BIN=str(self.bin),
                        TEST_GPG=GPG, TEST_KEY=str(self.armored), TEST_ROOT="1", TMPDIR=str(self.root))

    def platform(self, distro="ubuntu", version="24.04"):
        self.os_release.write_text(f'ID={distro}\nVERSION_ID="{version}"\n')

    def run_install(self, *args, success=True):
        result = subprocess.run([str(self.bin / "bash"), str(self.script), *args], env=self.env,
                                text=True, capture_output=True, timeout=20)
        self.assertEqual(result.returncode == 0, success, result.stdout + result.stderr)
        self.assertFalse(list(self.apt.rglob(".finder-gnome.*")))
        self.assertFalse(list(self.root.glob("finder-gnome-install.*")))
        return result

    def commands(self, name):
        lines = self.log.read_text().splitlines() if self.log.exists() else []
        return [call for line in lines if (call := json.loads(line))[0] == name]

    def test_platforms_and_dry_run_have_no_side_effects(self):
        for version, suite in [("24.04", "noble"), ("26.04", "resolute")]:
            self.platform(version=version)
            self.assertIn(f"suite {suite}", self.run_install("--dry-run").stdout)
        for command in ("apt-get", "curl", "sudo"):
            self.assertFalse(self.commands(command))
        self.assertEqual(list(self.apt.iterdir()), [])

    def test_dry_run_without_prerequisites_or_sudo(self):
        for tool in ("curl", "gpg", "sudo"):
            self.bin.joinpath(tool).unlink()
        self.env.update(TEST_CA="0", TEST_ROOT="0")
        self.assertIn("curl gnupg ca-certificates", self.run_install("--dry-run").stdout)
        for command in ("apt-get", "curl", "sudo"):
            self.assertFalse(self.commands(command))

    def test_missing_apt_and_dpkg_refuse_before_changes(self):
        self.bin.joinpath("apt-get").unlink()
        self.assertIn("apt-get is required", self.run_install(success=False).stderr)
        self.bin.joinpath("dpkg").unlink()
        self.assertIn("dpkg is required", self.run_install(success=False).stderr)
        self.assertEqual(list(self.apt.iterdir()), [])

    def test_unsupported_platforms_and_architecture(self):
        for distro, version in [("ubuntu", "22.04"), ("debian", "24.04"), ("linuxmint", "26.04")]:
            self.platform(distro, version)
            self.run_install(success=False)
        self.platform()
        self.env["TEST_ARCH"] = "arm64"
        self.assertIn("Unsupported architecture", self.run_install(success=False).stderr)
        self.assertFalse(self.commands("apt-get"))
        self.assertFalse(self.commands("sudo"))

    def test_real_armored_and_binary_key_idempotence(self):
        self.run_install()
        key = self.apt / "keyrings/finder-gnome.gpg"
        source = self.apt / "sources.list.d/finder-gnome.sources"
        metadata = [(p.stat().st_ino, p.stat().st_mtime_ns) for p in (key, source)]
        for path in (key, source):
            self.assertEqual(path.stat().st_mode & 0o777, 0o644)
        text = source.read_text()
        self.assertIn("Suites: noble\nComponents: main\nArchitectures: amd64\nSigned-By:", text)
        self.assertEqual(text.count("URIs:"), 1)
        self.env["TEST_KEY"] = str(self.binary)
        self.assertIn("Unchanged:", self.run_install().stdout)
        self.assertEqual(metadata, [(p.stat().st_ino, p.stat().st_mtime_ns) for p in (key, source)])
        self.assertEqual(self.commands("apt-get"), [["apt-get", "update"], ["apt-get", "install", "-y", "finder-gnome"]] * 2)

    def test_resolute_source(self):
        self.platform(version="26.04")
        self.run_install()
        self.assertIn("Suites: resolute\n", (self.apt / "sources.list.d/finder-gnome.sources").read_text())

    def test_readme_manual_commands_with_real_gpg_and_isolated_paths(self):
        readme = Path(__file__).parents[1].joinpath("README.md").read_text()
        manual = readme.split("### Manual installation\n", 1)[1].split("```bash\n", 1)[1].split("```", 1)[0]
        manual = manual.replace("/etc/os-release", str(self.os_release)).replace("/etc/apt", str(self.apt))
        manual = manual.replace("F550EC3355FD905A3B39D4B5679BD5860B6EECE1", self.fingerprints[0])
        # A normal Ubuntu install already has sources.list.d.
        self.apt.joinpath("sources.list.d").mkdir()
        result = subprocess.run([str(self.bin / "bash"), "-c", manual], env=self.env,
                                text=True, capture_output=True, timeout=20)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertTrue(self.apt.joinpath("keyrings/finder-gnome.gpg").is_file())
        self.assertIn("Suites: noble\n", self.apt.joinpath("sources.list.d/finder-gnome.sources").read_text())
        self.assertIn(["apt", "install", "finder-gnome"], self.commands("apt"))

    def test_bad_download_wrong_key_and_key_bundle(self):
        invalid = self.root / "invalid"
        invalid.write_text("<html>not a public key</html>")
        for key in (invalid, self.wrong, self.bundle):
            self.env["TEST_KEY"] = str(key)
            self.run_install(success=False)
            self.assertEqual(list(self.apt.iterdir()), [])
        self.env["TEST_DOWNLOAD_FAIL"] = "1"
        self.run_install(success=False)
        self.assertFalse(self.commands("apt-get"))

    def test_failed_key_rotation_preserves_existing_files(self):
        self.run_install()
        snapshot = {p: p.read_bytes() for p in self.apt.rglob("*") if p.is_file()}
        self.env["TEST_KEY"] = str(self.wrong)
        self.run_install(success=False)
        self.assertEqual(snapshot, {p: p.read_bytes() for p in self.apt.rglob("*") if p.is_file()})

    def test_package_errors_propagate(self):
        for stage in ("update", "install"):
            self.env["TEST_APT_FAIL"] = stage
            result = self.run_install(success=False)
            self.assertEqual(result.returncode, 42)
            self.assertNotIn("Installed finder-gnome", result.stdout)

    def test_atomic_write_failure_removes_system_staging_file(self):
        self.bin.joinpath("mv").unlink()
        self.bin.joinpath("mv").symlink_to("mock")
        self.assertEqual(self.run_install(success=False).returncode, 43)
        self.assertFalse(self.apt.joinpath("keyrings/finder-gnome.gpg").exists())
        self.assertFalse(self.commands("apt-get"))

    def test_only_missing_prerequisites_are_installed(self):
        for command in ("curl", "gpg"):
            self.bin.joinpath(command).unlink()
        self.env["TEST_CA"] = "0"
        self.run_install()
        self.assertEqual(self.commands("apt-get")[1], ["apt-get", "install", "-y", "curl", "gnupg", "ca-certificates"])

    def test_unprivileged_success_missing_sudo_and_denial(self):
        self.env["TEST_ROOT"] = "0"
        self.env["TEST_SUDO_DENY"] = "1"
        self.assertIn("authorization was denied", self.run_install(success=False).stderr)
        self.env.pop("TEST_SUDO_DENY")
        self.run_install()
        self.bin.joinpath("sudo").unlink()
        self.assertIn("Run as root", self.run_install(success=False).stderr)

    def test_other_sources_are_untouched_and_duplicates_rejected(self):
        directory = self.apt / "sources.list.d"
        directory.mkdir()
        other = directory / "unrelated.list"
        other.write_text("deb https://example.org/ubuntu noble main\n")
        self.run_install()
        self.assertEqual(other.read_text(), "deb https://example.org/ubuntu noble main\n")
        other.write_text("deb https://repo.web7dev.com noble main\n")
        self.assertIn("manual review", self.run_install(success=False).stderr)
        self.assertEqual(other.read_text(), "deb https://repo.web7dev.com noble main\n")

    def test_symlinks_and_foreign_owned_path_are_rejected(self):
        directory = self.apt / "sources.list.d"
        directory.mkdir()
        source = directory / "finder-gnome.sources"
        source.symlink_to(self.os_release)
        self.assertIn("symlink", self.run_install(success=False).stderr)
        source.unlink()
        source.write_text("Types: deb\nURIs: https://example.org\nSuites: noble\n")
        self.run_install(success=False)
        self.assertIn("https://example.org", source.read_text())


if __name__ == "__main__":
    unittest.main(verbosity=2)

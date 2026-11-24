#!/usr/bin/env python3
"""Tests for scripts/set-release-version.py using a temporary checkout copy."""

from __future__ import annotations

import importlib.util
import json
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SCRIPT = ROOT / "scripts" / "set-release-version.py"
spec = importlib.util.spec_from_file_location("set_release_version", SCRIPT)
assert spec and spec.loader
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class ReleaseVersionTest(unittest.TestCase):
    def setUp(self) -> None:
        self.work = tempfile.TemporaryDirectory()
        self.addCleanup(self.work.cleanup)
        self.root = Path(self.work.name)
        for name in ("Cargo.toml", "Cargo.lock"):
            shutil.copyfile(ROOT / name, self.root / name)

    def run_script(self, *arguments: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [sys.executable, str(SCRIPT), "--root", str(self.root), *arguments],
            capture_output=True,
            text=True,
            check=False,
        )

    def test_stable_tag_mismatch_refused(self) -> None:
        manifest = (self.root / "Cargo.toml").read_text(encoding="utf-8")
        lock = (self.root / "Cargo.lock").read_text(encoding="utf-8")
        for arguments in (
            ("check", "v9.9.9"),
            ("check", "v0.3.6-dev.7.gabc1234"),
            ("set", "v9.9.9"),
            ("set", "v9.9.10-dev.9.gabc1234"),
        ):
            result = self.run_script(*arguments)
            self.assertNotEqual(result.returncode, 0, arguments)
            self.assertIn("set-release-version:", result.stderr)
            self.assertEqual((self.root / "Cargo.toml").read_text(encoding="utf-8"), manifest)
            self.assertEqual((self.root / "Cargo.lock").read_text(encoding="utf-8"), lock)
        self.assertEqual(self.run_script("check", "v0.3.5").returncode, 0)
        self.assertEqual(self.run_script("set", "v0.3.5").returncode, 0)

    def test_dev_rewrite_correct(self) -> None:
        result = self.run_script("set", "v0.3.6-dev.7.gabc1234")
        self.assertEqual(result.returncode, 0, result.stderr)
        version = "0.3.6-dev.7.gabc1234"
        manifest = (self.root / "Cargo.toml").read_text(encoding="utf-8")
        lock = (self.root / "Cargo.lock").read_text(encoding="utf-8")
        self.assertIn(f'version = "{version}"', manifest.split("[package]", 1)[1])
        self.assertIn(f'name = "tuishou"\nversion = "{version}"', lock)
        self.assertEqual(module.read_version(self.root), version)
        self.assertEqual(module.read_lock_version(self.root), version)
        self.assertEqual((self.root / ".git").exists(), False)

    def test_dev_tag_uses_next_patch(self) -> None:
        result = self.run_script("dev-tag", "--commits", "7", "--hash", "abc1234")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), "v0.3.6-dev.7.gabc1234")

    def test_ordering_sorts_after_base_stable_and_before_next(self) -> None:
        def key(tag: str) -> tuple[int, int, int, int, int, str]:
            result = self.run_script("sort-key", tag)
            self.assertEqual(result.returncode, 0, result.stderr)
            return tuple(json.loads(result.stdout))  # type: ignore[return-value]

        base = key("v0.3.5")
        dev = key("v0.3.6-dev.7.gabc1234")
        later = key("v0.3.6-dev.10.gdef5678")
        next_stable = key("v0.3.6")
        self.assertLess(base, dev)
        self.assertLess(dev, later)
        self.assertLess(later, next_stable)

        tags = [f"v0.3.6-dev.{number}.g{number:07x}" for number in range(1, 13)]
        tags.extend(["v0.3.5", "v0.3.6", "v0.3.6-rc.1"])
        pruned = module.prune_list(tags, 10)
        self.assertEqual(pruned, tags[:2])
        result = subprocess.run(
            [sys.executable, str(SCRIPT), "prune-list", "--keep", "10"],
            input="\n".join(tags) + "\n",
            capture_output=True,
            text=True,
            check=False,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.splitlines(), tags[:2])


if __name__ == "__main__":
    unittest.main()

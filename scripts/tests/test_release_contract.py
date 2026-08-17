from __future__ import annotations

import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

SCRIPTS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPTS))

import release_contract  # noqa: E402


class LockfileTests(unittest.TestCase):
    def test_requires_the_local_package_version_to_match(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            lockfile = root / "Cargo.lock"
            lockfile.write_text(
                'version = 4\n\n[[package]]\nname = "gsym-rs"\nversion = "1.2.3"\n'
            )
            with patch.object(release_contract, "ROOT", root):
                release_contract.validate_lockfile(lockfile, "1.2.3")
                with self.assertRaisesRegex(SystemExit, "not in sync with version 1.2.4"):
                    release_contract.validate_lockfile(lockfile, "1.2.4")


if __name__ == "__main__":
    unittest.main()

from __future__ import annotations

import sys
import unittest
import uuid
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPTS))

from prepare_release_sbom import prepare, serial_number  # noqa: E402


class SerialNumberTests(unittest.TestCase):
    def test_is_stable_cyclonedx_uuid(self) -> None:
        serial = serial_number("0.1.1", "x86_64-unknown-linux-gnu")

        self.assertEqual(serial, serial_number("0.1.1", "x86_64-unknown-linux-gnu"))
        self.assertEqual(uuid.UUID(serial.removeprefix("urn:uuid:")).version, 5)

    def test_distinguishes_release_subjects(self) -> None:
        subjects = {
            serial_number("0.1.1", "x86_64-unknown-linux-gnu"),
            serial_number("0.1.1", "aarch64-unknown-linux-gnu"),
            serial_number("0.1.2", "x86_64-unknown-linux-gnu"),
        }

        self.assertEqual(len(subjects), 3)

    def test_prepare_injects_attestable_serial_number(self) -> None:
        version = "0.1.1"
        target = "x86_64-unknown-linux-gnu"
        root = "pkg:cargo/gsymtool@0.1.1"
        library = "pkg:cargo/gsym-rs@0.1.1"
        document = {
            "bomFormat": "CycloneDX",
            "specVersion": "1.5",
            "version": 1,
            "serialNumber": "urn:uuid:00000000-0000-0000-0000-000000000000",
            "metadata": {
                "component": {
                    "bom-ref": root,
                    "name": "gsymtool",
                    "type": "application",
                    "version": version,
                },
                "properties": [
                    {"name": "cdx:rustc:sbom:target:triple", "value": target}
                ],
            },
            "components": [
                {
                    "bom-ref": library,
                    "name": "gsym-rs",
                    "type": "library",
                    "version": version,
                }
            ],
            "dependencies": [
                {"ref": root, "dependsOn": [library]},
                {"ref": library, "dependsOn": []},
            ],
        }

        prepare(document, version, target)

        self.assertEqual(document["serialNumber"], serial_number(version, target))


if __name__ == "__main__":
    unittest.main()

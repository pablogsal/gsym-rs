#!/usr/bin/env python3
"""Collect, checksum, and verify the exact payload of a GitHub release."""

from __future__ import annotations

import argparse
import shutil
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from release_common import (  # noqa: E402
    TARGET_NAMES,
    archive_name,
    checksum_name,
    expected_assets,
    make_fail,
    sbom_name,
    verify_checksum,
)

fail = make_fail("release payload failure")


def index(root: Path) -> dict[str, Path]:
    """Map every file under a download tree to its name, rejecting clashes."""

    found: dict[str, Path] = {}
    for path in root.rglob("*"):
        if not path.is_file():
            continue
        if path.name in found:
            fail(f"more than one {path.name} under {root}")
        found[path.name] = path
    return found


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--version", required=True)
    parser.add_argument("--input", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    arguments = parser.parse_args()

    if arguments.output.exists() and any(arguments.output.iterdir()):
        fail(f"output directory is not empty: {arguments.output}")
    arguments.output.mkdir(parents=True, exist_ok=True)

    available = index(arguments.input)
    checksums: dict[str, str] = {}
    for target in TARGET_NAMES:
        name = archive_name(arguments.version, target)
        for wanted in (name, checksum_name(name)):
            if wanted not in available:
                fail(f"no {wanted} among the built artifacts")
            shutil.copy2(available[wanted], arguments.output / wanted)
        checksums[name] = verify_checksum(arguments.output / name)
        sbom = sbom_name(arguments.version, target)
        for wanted in (sbom, checksum_name(sbom)):
            if wanted not in available:
                fail(f"no {wanted} among the built artifacts")
            shutil.copy2(available[wanted], arguments.output / wanted)
        checksums[sbom] = verify_checksum(arguments.output / sbom)

    unified = "".join(f"{digest}  {name}\n" for name, digest in sorted(checksums.items()))
    (arguments.output / "SHA256SUMS").write_text(unified)

    produced = sorted(path.name for path in arguments.output.iterdir())
    expected = expected_assets(arguments.version)
    if produced != expected:
        fail(f"the payload does not match the plan: {sorted(set(produced) ^ set(expected))}")

    print(f"prepared and verified {len(produced)} release assets in {arguments.output}")


if __name__ == "__main__":
    main()

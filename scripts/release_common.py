#!/usr/bin/env python3
"""The single source of truth shared by every release script.

The release workflow reads its build matrix from here, so the platforms that
are built, the archives that are named, and the asset set that is finally
published all derive from one list and cannot drift apart.
"""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
from typing import NoReturn

BINARY = "gsymtool"
CRATE = "gsym-rs"

# Every target is built on a native runner so that the produced binary can be
# executed and inspected by the verification step that follows the build.
TARGETS: list[dict[str, str]] = [
    {
        "target": "x86_64-unknown-linux-gnu",
        "runner": "ubuntu-22.04",
        "libc": "gnu",
        # Ubuntu 22.04 is the oldest image GitHub offers; it pins the glibc
        # baseline of the dynamically linked archives.
        "max_glibc": "2.35",
    },
    {
        "target": "aarch64-unknown-linux-gnu",
        "runner": "ubuntu-22.04-arm",
        "libc": "gnu",
        "max_glibc": "2.35",
    },
    {
        "target": "x86_64-unknown-linux-musl",
        "runner": "ubuntu-22.04",
        "libc": "musl",
        "max_glibc": "",
    },
    {
        "target": "aarch64-unknown-linux-musl",
        "runner": "ubuntu-22.04-arm",
        "libc": "musl",
        "max_glibc": "",
    },
]

# What each C library needs from the runner. Adding a target that needs a
# different toolchain means adding a row here, not a branch in the workflow.
LIBC_BUILD_ENVIRONMENT: dict[str, dict[str, str]] = {
    "gnu": {"packages": "", "linker": "", "rustflags": ""},
    "musl": {
        "packages": "musl-tools",
        "linker": "musl-gcc",
        "rustflags": "-C relocation-model=static",
    },
}

TARGET_NAMES = [entry["target"] for entry in TARGETS]


def make_fail(context: str):
    """Return a fail() that aborts with a message tagged for one script."""

    def fail(message: str) -> NoReturn:
        raise SystemExit(f"{context}: {message}")

    return fail


_fail = make_fail("release failure")


def target_row(target: str) -> dict[str, str]:
    for entry in TARGETS:
        if entry["target"] == target:
            return entry
    _fail(f"{target} is not one of the reviewed release targets")


def matrix(libc: str | None = None) -> dict[str, list[dict[str, str]]]:
    """Return the GitHub Actions matrix, carrying only what the jobs use."""

    return {
        "include": [
            {
                "target": entry["target"],
                "runner": entry["runner"],
                **LIBC_BUILD_ENVIRONMENT[entry["libc"]],
            }
            for entry in TARGETS
            if libc is None or entry["libc"] == libc
        ]
    }


def archive_name(version: str, target: str) -> str:
    return f"{BINARY}-{version}-{target}.tar.gz"


def checksum_name(archive: str) -> str:
    return f"{archive}.sha256"


def expected_assets(version: str) -> list[str]:
    """Return the exact file names a release must publish, in sorted order."""

    assets = ["SHA256SUMS"]
    for target in TARGET_NAMES:
        name = archive_name(version, target)
        assets.extend((name, checksum_name(name)))
    return sorted(assets)


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def write_checksum(archive: Path) -> str:
    """Write the sidecar next to an archive, in sha256sum's own format."""

    digest = sha256(archive)
    archive.with_name(checksum_name(archive.name)).write_text(
        f"{digest}  {archive.name}\n"
    )
    return digest


def verify_checksum(archive: Path) -> str:
    """Return an archive's digest, aborting if its sidecar disagrees."""

    sidecar = archive.with_name(checksum_name(archive.name))
    if not sidecar.is_file():
        _fail(f"missing checksum sidecar for {archive.name}")
    digest = sha256(archive)
    if sidecar.read_text().split()[0] != digest:
        _fail(f"checksum sidecar does not match {archive.name}")
    return digest


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument(
        "--matrix",
        action="store_true",
        help="print the GitHub Actions build matrix as compact JSON",
    )
    mode.add_argument(
        "--plan",
        metavar="VERSION",
        help="print the release plan for VERSION as indented JSON",
    )
    parser.add_argument(
        "--libc",
        choices=sorted(LIBC_BUILD_ENVIRONMENT),
        help="limit --matrix output to one C library",
    )
    arguments = parser.parse_args()

    if arguments.matrix:
        print(json.dumps(matrix(arguments.libc), separators=(",", ":")))
        return

    if arguments.libc is not None:
        parser.error("--libc requires --matrix")

    plan = {
        "version": arguments.plan,
        "targets": TARGET_NAMES,
        "assets": expected_assets(arguments.plan),
    }
    print(json.dumps(plan, indent=2, sort_keys=True))


if __name__ == "__main__":
    main()

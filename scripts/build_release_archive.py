#!/usr/bin/env python3
"""Stage, archive, and checksum the release payload for one target.

Keeping this out of the workflow means the exact archive a release publishes
can be produced, and inspected, from a workstation.
"""

from __future__ import annotations

import argparse
import gzip
import os
import subprocess
import sys
import tarfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from release_common import (  # noqa: E402
    BINARY,
    archive_name,
    make_fail,
    write_checksum,
)

ROOT = Path(__file__).resolve().parents[1]
DOCUMENTS = ["README.md", "CHANGELOG.md", "LICENSE-APACHE", "LICENSE-MIT"]
COMPLETIONS = {"bash": f"{BINARY}.bash", "fish": f"{BINARY}.fish", "zsh": f"_{BINARY}"}

fail = make_fail("release archive failure")


def source_date_epoch() -> int:
    """The commit time, so the archive carries no build timestamp."""

    stamped = os.environ.get("SOURCE_DATE_EPOCH")
    if stamped:
        return int(stamped)
    committed = subprocess.run(
        ["git", "show", "-s", "--format=%ct", "HEAD"],
        cwd=ROOT,
        check=True,
        stdout=subprocess.PIPE,
        text=True,
    )
    return int(committed.stdout.strip())


def stage(binary: Path, directory: Path) -> None:
    """Lay out exactly what a release archive contains."""

    directory.mkdir(parents=True)
    staged = directory / BINARY
    staged.write_bytes(binary.read_bytes())
    staged.chmod(0o755)
    subprocess.run(["strip", str(staged)], check=True)

    for document in DOCUMENTS:
        source = ROOT / document
        if not source.is_file():
            fail(f"the release payload is missing {document}")
        (directory / document).write_bytes(source.read_bytes())

    for shell, name in COMPLETIONS.items():
        generated = subprocess.run(
            [str(staged), "completions", shell],
            check=True,
            stdout=subprocess.PIPE,
            text=True,
        )
        (directory / name).write_text(generated.stdout)


def archive(staging: Path, root: str, destination: Path, timestamp: int) -> None:
    """Write a gzipped tar whose bytes depend only on the staged payload."""

    def reset(entry: tarfile.TarInfo) -> tarfile.TarInfo:
        entry.mtime = timestamp
        entry.uid = entry.gid = 0
        entry.uname = entry.gname = ""
        return entry

    members = sorted((staging / root).rglob("*"))
    with destination.open("wb") as raw:
        # mtime=0 keeps the gzip header out of the archive's identity.
        with gzip.GzipFile(fileobj=raw, mode="wb", compresslevel=9, mtime=0) as stream:
            with tarfile.open(fileobj=stream, mode="w") as tar:
                tar.add(staging / root, arcname=root, recursive=False, filter=reset)
                for member in members:
                    arcname = f"{root}/{member.relative_to(staging / root)}"
                    tar.add(member, arcname=arcname, recursive=False, filter=reset)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--target", required=True)
    parser.add_argument("--version", required=True)
    parser.add_argument("--staging", type=Path, default=Path("staging"))
    parser.add_argument("--output", type=Path, default=Path("distrib"))
    arguments = parser.parse_args()

    if not arguments.binary.is_file():
        fail(f"no built binary at {arguments.binary}")

    name = archive_name(arguments.version, arguments.target)
    root = name.removesuffix(".tar.gz")
    if (arguments.staging / root).exists():
        fail(f"staging directory already exists: {arguments.staging / root}")
    arguments.output.mkdir(parents=True, exist_ok=True)

    stage(arguments.binary, arguments.staging / root)
    destination = arguments.output / name
    archive(arguments.staging, root, destination, source_date_epoch())
    digest = write_checksum(destination)

    print(f"built {destination} ({digest})")


if __name__ == "__main__":
    main()

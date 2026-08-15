#!/usr/bin/env python3
"""Verify one release archive on the native runner that produced it.

The required payload is spelled out again here rather than shared with
`build_release_archive.py`: a verifier that reads the producer's own manifest
would agree with it by construction.
"""

from __future__ import annotations

import argparse
import re
import stat
import subprocess
import sys
import tarfile
import tempfile
from pathlib import Path, PurePosixPath

sys.path.insert(0, str(Path(__file__).resolve().parent))

from release_common import (  # noqa: E402
    BINARY,
    archive_name,
    make_fail,
    target_row,
    verify_checksum,
)

REQUIRED_FILES = {
    BINARY,
    "CHANGELOG.md",
    "LICENSE-APACHE",
    "LICENSE-MIT",
    "README.md",
    f"{BINARY}.bash",
    f"{BINARY}.fish",
    f"_{BINARY}",
}
GLIBC_VERSION = re.compile(r"GLIBC_(\d+(?:\.\d+)*)")

fail = make_fail("archive verification failure")


def version_tuple(version: str) -> tuple[int, ...]:
    return tuple(int(part) for part in version.split("."))


def run(*command: str | Path) -> str:
    return subprocess.run(
        [str(part) for part in command],
        check=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
    ).stdout


def safe_members(archive: tarfile.TarFile, root: str) -> list[tarfile.TarInfo]:
    members = archive.getmembers()
    if not members:
        fail("the archive is empty")
    for member in members:
        path = PurePosixPath(member.name)
        if path.is_absolute() or ".." in path.parts:
            fail(f"unsafe archive member: {member.name}")
        if member.issym() or member.islnk():
            fail(f"release archives may not contain links: {member.name}")
        if not member.isfile() and not member.isdir():
            fail(f"unexpected archive member type: {member.name}")
        if path.parts[0] != root:
            fail(f"archive member outside {root}/: {member.name}")
    return members


def check_linkage(binary: Path, libc: str, max_glibc: str) -> None:
    if libc == "musl":
        program_headers = run("readelf", "--program-headers", binary)
        if " INTERP " in program_headers:
            fail("the musl binary contains a dynamic interpreter")
        dynamic = run("readelf", "--dynamic", binary)
        if "(NEEDED)" in dynamic:
            fail(f"the musl binary imports a shared library:\n{dynamic}")
        if GLIBC_VERSION.search(run("readelf", "--version-info", binary)):
            fail("the musl binary still imports versioned glibc symbols")
        return

    linkage = run("ldd", binary)
    if "not found" in linkage:
        fail(f"unresolved shared library:\n{linkage}")
    required = {
        version_tuple(match)
        for match in GLIBC_VERSION.findall(run("objdump", "-T", binary))
    }
    if not required:
        fail("could not determine the glibc baseline of the binary")
    highest = max(required)
    if highest > version_tuple(max_glibc):
        rendered = ".".join(str(part) for part in highest)
        fail(f"the binary needs glibc {rendered}, above the {max_glibc} baseline")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", required=True)
    parser.add_argument("--version", required=True)
    parser.add_argument("--input", type=Path, default=Path("distrib"))
    arguments = parser.parse_args()

    platform = target_row(arguments.target)
    name = archive_name(arguments.version, arguments.target)
    path = arguments.input / name
    if not path.is_file():
        fail(f"no archive at {path}")
    digest = verify_checksum(path)

    root = name.removesuffix(".tar.gz")
    with tarfile.open(path, "r:gz") as archive:
        members = safe_members(archive, root)
        files = {PurePosixPath(m.name).name for m in members if m.isfile()}
        missing = REQUIRED_FILES - files
        if missing:
            fail(f"{name} is missing {sorted(missing)}")

        binaries = [
            member
            for member in members
            if member.isfile() and PurePosixPath(member.name).name == BINARY
        ]
        if len(binaries) != 1:
            fail(f"expected exactly one {BINARY} binary, found {len(binaries)}")
        if not binaries[0].mode & stat.S_IXUSR:
            fail(f"the packaged {BINARY} binary is not executable")

        with tempfile.TemporaryDirectory() as directory:
            archive.extractall(directory, members=members, filter="data")
            binary = Path(directory, binaries[0].name)
            reported = run(binary, "--version").strip()
            if reported != f"{BINARY} {arguments.version}":
                fail(f"unexpected version output: {reported}")
            if f"Usage: {BINARY}" not in run(binary, "--help"):
                fail("the packaged binary does not print its own usage")
            check_linkage(binary, platform["libc"], platform["max_glibc"])

    print(f"verified {name} ({digest})")


if __name__ == "__main__":
    main()

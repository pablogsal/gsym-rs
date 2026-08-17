#!/usr/bin/env python3
"""Validate the source and release contracts before anything is published.

The release workflow runs this twice: once when it plans a release, and once
more immediately before the tag and the GitHub release are created. Every
check either passes or aborts the release; nothing here is advisory.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
import time
import tomllib
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from release_common import CRATE, make_fail  # noqa: E402

ROOT = Path(__file__).resolve().parents[1]
RELEASE_TAG = re.compile(
    r"^v(?P<version>(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)\.(?:0|[1-9]\d*))$"
)
CHANGELOG_SECTION = re.compile(
    r"^## (?P<version>\d+\.\d+\.\d+) - \d{4}-\d{2}-\d{2}\s*$", re.MULTILINE
)
REQUIRED_PATHS = [
    "CHANGELOG.md",
    "Cargo.lock",
    "Cargo.toml",
    "fuzz/Cargo.lock",
    "fuzz/Cargo.toml",
    "LICENSE-APACHE",
    "LICENSE-MIT",
    "README.md",
    "RELEASE.md",
    "scripts/build_release_archive.py",
    "scripts/install_cargo_cyclonedx.sh",
    "scripts/prepare_release_assets.py",
    "scripts/prepare_release_sbom.py",
    "scripts/release_common.py",
    "scripts/verify_release_archive.py",
    "src/lib.rs",
]
LOCKFILES = ["Cargo.lock", "fuzz/Cargo.lock"]
USER_AGENT = f"{CRATE}-release-contract (https://github.com/pablogsal/gsym-rs)"

fail = make_fail("release contract failure")


def load_toml(path: Path) -> dict:
    with path.open("rb") as stream:
        return tomllib.load(stream)


def validate_lockfile(path: Path, version: str) -> None:
    locked = [
        entry for entry in load_toml(path)["package"] if entry["name"] == CRATE
    ]
    if len(locked) != 1 or locked[0]["version"] != version:
        fail(f"{path.relative_to(ROOT)} is not in sync with version {version}")


def git(*arguments: str, check: bool = True) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["git", *arguments],
        cwd=ROOT,
        check=check,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )


def in_git_worktree() -> bool:
    result = git("rev-parse", "--is-inside-work-tree", check=False)
    return result.returncode == 0 and result.stdout.strip() == "true"


def in_actions() -> bool:
    return os.environ.get("GITHUB_ACTIONS") == "true"


def resource_exists(request: urllib.request.Request, service: str) -> bool:
    """Ask a JSON API whether one resource is already taken."""

    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            json.load(response)
        return True
    except urllib.error.HTTPError as error:
        if error.code == 404:
            return False
        fail(f"{service} availability check failed with HTTP {error.code}")
    except urllib.error.URLError as error:
        fail(f"{service} availability check failed: {error.reason}")


def github_resource_exists(path: str) -> bool:
    repository = os.environ.get("GITHUB_REPOSITORY")
    token = os.environ.get("GITHUB_TOKEN")
    if not repository or not token:
        fail("GitHub tag checks require GITHUB_REPOSITORY and GITHUB_TOKEN")

    return resource_exists(
        urllib.request.Request(
            f"https://api.github.com/repos/{repository}/{path}",
            headers={
                "Accept": "application/vnd.github+json",
                "Authorization": f"Bearer {token}",
                "User-Agent": USER_AGENT,
                "X-GitHub-Api-Version": "2022-11-28",
            },
        ),
        "GitHub",
    )


def crates_io_version_exists(version: str) -> bool:
    return resource_exists(
        urllib.request.Request(
            f"https://crates.io/api/v1/crates/{CRATE}/{urllib.parse.quote(version)}",
            headers={"Accept": "application/json", "User-Agent": USER_AGENT},
        ),
        "crates.io",
    )


def validate_structure() -> str:
    missing = [path for path in REQUIRED_PATHS if not (ROOT / path).exists()]
    if missing:
        fail(f"required release paths are missing: {', '.join(missing)}")

    cargo = load_toml(ROOT / "Cargo.toml")
    package = cargo["package"]
    version = package["version"]

    if package["name"] != CRATE:
        fail(f"the root Cargo package must remain named {CRATE}")
    if cargo["lib"]["name"] != "gsym":
        fail("the library must remain importable as `gsym`")
    if package["license"] != "MIT OR Apache-2.0":
        fail("the crate must remain dual licensed as MIT OR Apache-2.0")
    if not package.get("description") or not package.get("repository"):
        fail("crates.io requires both description and repository metadata")
    if not package.get("rust-version"):
        fail("the crate must declare its supported Rust version")
    binaries = cargo.get("bin", [])
    if binaries != [
        {
            "name": "gsymtool",
            "path": "gsymtool/src/main.rs",
            "required-features": ["convert"],
        }
    ]:
        fail("the published package must contain the gsymtool binary")

    repository = package["repository"]
    slug = os.environ.get("GITHUB_REPOSITORY")
    if slug and repository.rstrip("/") != f"https://github.com/{slug}":
        fail(f"Cargo repository metadata {repository} does not match {slug}")

    for lockfile in LOCKFILES:
        validate_lockfile(ROOT / lockfile, version)

    if in_git_worktree():
        for lockfile in LOCKFILES:
            tracked = git("ls-files", "--error-unmatch", lockfile, check=False)
            if tracked.returncode != 0:
                fail(f"{lockfile} must be committed")

    changelog = (ROOT / "CHANGELOG.md").read_text()
    sections = CHANGELOG_SECTION.findall(changelog)
    if not sections:
        fail("CHANGELOG.md has no `## X.Y.Z - YYYY-MM-DD` release section")
    if sections[0] != version:
        fail(f"the newest CHANGELOG.md section is {sections[0]}, not {version}")

    return version


def release_notes(version: str) -> str:
    changelog = (ROOT / "CHANGELOG.md").read_text()
    matches = list(CHANGELOG_SECTION.finditer(changelog))
    for index, match in enumerate(matches):
        if match.group("version") != version:
            continue
        end = matches[index + 1].start() if index + 1 < len(matches) else len(changelog)
        return changelog[match.end() : end].strip()
    fail(f"CHANGELOG.md has no section for {version}")


def validate_release(
    tag: str,
    version: str,
    check_availability: bool,
    check_crates_io: bool,
    require_crates_io: bool,
) -> None:
    match = RELEASE_TAG.fullmatch(tag)
    if not match:
        fail(f"release tag {tag} must have the exact form vX.Y.Z")
    if match.group("version") != version:
        fail(f"release tag {tag} does not match project version {version}")
    if not release_notes(version):
        fail(f"the CHANGELOG.md section for {version} is empty")

    if in_actions():
        if os.environ.get("GITHUB_EVENT_NAME") != "workflow_dispatch":
            fail("a real release may only run through workflow_dispatch")
        if os.environ.get("GITHUB_REF") != "refs/heads/main":
            fail("a real release must be dispatched from main")

    # The crate reaches crates.io before the tag is created, so the two
    # availability checks are separate: the re-validation that guards tagging
    # must not object to the version it has just published itself.
    if check_crates_io and crates_io_version_exists(version):
        fail(f"{CRATE} {version} is already published on crates.io")
    if require_crates_io and not crates_io_version_exists(version):
        fail(f"{CRATE} {version} is not published on crates.io")

    if not check_availability:
        return

    if in_git_worktree():
        local = git("show-ref", "--verify", "--quiet", f"refs/tags/{tag}", check=False)
        if local.returncode == 0:
            fail(f"tag {tag} already exists locally")

    if in_actions():
        encoded = urllib.parse.quote(tag, safe="")
        if github_resource_exists(f"git/ref/tags/{encoded}"):
            fail(f"tag {tag} already exists on GitHub")
        if github_resource_exists(f"releases/tags/{encoded}"):
            fail(f"release {tag} already exists on GitHub")
    elif in_git_worktree():
        remote = git(
            "ls-remote", "--exit-code", "--tags", "origin", f"refs/tags/{tag}",
            check=False,
        )
        if remote.returncode == 0:
            fail(f"tag {tag} already exists on origin")
        if remote.returncode not in {0, 2}:
            fail(f"could not query origin for {tag}: {remote.stderr.strip()}")


def wait_for_crates_io(version: str, attempts: int = 6, interval: int = 20) -> None:
    """Block until crates.io serves the version this run published."""

    for attempt in range(1, attempts + 1):
        if crates_io_version_exists(version):
            print(f"crates.io serves {CRATE} {version}", file=sys.stderr)
            return
        print(f"attempt {attempt}: crates.io does not serve it yet", file=sys.stderr)
        if attempt < attempts:
            time.sleep(interval)
    fail(f"crates.io never served {CRATE} {version}")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--tag",
        default="dry-run",
        help="the exact vX.Y.Z tag to validate, or dry-run for a rehearsal",
    )
    parser.add_argument(
        "--check-availability",
        action="store_true",
        help="also require that the tag and the GitHub release do not exist yet",
    )
    registry = parser.add_mutually_exclusive_group()
    registry.add_argument(
        "--check-crates-io",
        action="store_true",
        help="also require that crates.io does not serve this version yet",
    )
    registry.add_argument(
        "--require-crates-io",
        action="store_true",
        help="also require that crates.io already serves this version",
    )
    parser.add_argument(
        "--wait-for-crates-io",
        action="store_true",
        help="instead, wait until crates.io serves this version",
    )
    parser.add_argument(
        "--notes-out",
        type=Path,
        help="write the CHANGELOG.md section for this version to this file",
    )
    parser.add_argument(
        "--print-version",
        action="store_true",
        help="print the validated version to stdout, for use as a job output",
    )
    arguments = parser.parse_args()

    version = validate_structure()
    if arguments.tag != "dry-run":
        validate_release(
            arguments.tag,
            version,
            arguments.check_availability,
            arguments.check_crates_io,
            arguments.require_crates_io,
        )
    if arguments.notes_out is not None:
        arguments.notes_out.write_text(release_notes(version) + "\n")
    if arguments.wait_for_crates_io:
        wait_for_crates_io(version)

    print(
        f"release contracts passed for {CRATE} {version} ({arguments.tag})",
        file=sys.stderr,
    )
    if arguments.print_version:
        print(version)


if __name__ == "__main__":
    main()

#!/usr/bin/env python3
"""Validate and stage the reproducible CycloneDX release SBOM."""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from release_common import CRATE, make_fail, sbom_name, write_checksum  # noqa: E402

MAX_ATTESTATION_SIZE = 16 * 1024 * 1024
fail = make_fail("release SBOM failure")


def components(document: dict) -> list[dict]:
    metadata = document.get("metadata")
    root = metadata.get("component") if isinstance(metadata, dict) else None
    top_level = document.get("components")
    if not isinstance(root, dict):
        fail("metadata.component is missing")
    if not isinstance(top_level, list) or not top_level:
        fail("the SBOM has no dependency components")

    pending = [root, *reversed(top_level)]
    found: list[dict] = []
    while pending:
        component = pending.pop()
        if not isinstance(component, dict):
            fail("a component is not an object")
        found.append(component)
        children = component.get("components", [])
        if not isinstance(children, list):
            fail("a component's children are not a list")
        pending.extend(reversed(children))
    return found


def component_refs(document: dict) -> list[str]:
    refs: list[str] = []
    for component in components(document):
        reference = component.get("bom-ref")
        if not isinstance(reference, str) or not reference:
            fail("a component has no nonempty bom-ref")
        refs.append(reference)
    return refs


def normalize_local_references(document: dict) -> None:
    all_components = components(document)
    old_refs = component_refs(document)
    if len(set(old_refs)) != len(old_refs):
        fail("the SBOM contains duplicate component references")

    replacements: dict[str, str] = {}
    for component in all_components:
        reference = component["bom-ref"]
        if not reference.startswith("path+file://"):
            continue
        purl = component.get("purl")
        if not isinstance(purl, str) or not purl:
            fail("a local component has no package URL")
        replacements[reference] = purl
        component["bom-ref"] = purl

    dependencies = document.get("dependencies")
    if not isinstance(dependencies, list):
        fail("the SBOM has no dependency graph")
    for dependency in dependencies:
        if not isinstance(dependency, dict):
            fail("a dependency entry is not an object")
        reference = dependency.get("ref")
        if isinstance(reference, str):
            dependency["ref"] = replacements.get(reference, reference)
        depends_on = dependency.get("dependsOn")
        if isinstance(depends_on, list):
            dependency["dependsOn"] = [
                replacements.get(reference, reference) for reference in depends_on
            ]


def validate(document: dict, version: str, target: str) -> None:
    if document.get("bomFormat") != "CycloneDX":
        fail("bomFormat is not CycloneDX")
    if document.get("specVersion") != "1.5" or document.get("version") != 1:
        fail("the SBOM must use CycloneDX 1.5 document version 1")
    if "serialNumber" in document:
        fail("the reproducible SBOM must not contain a random serial number")

    metadata = document.get("metadata")
    root = metadata.get("component")
    if root.get("name") != "gsymtool" or root.get("version") != version:
        fail("metadata.component does not describe this gsymtool release")
    if root.get("type") != "application":
        fail("gsymtool must be identified as an application")
    properties = metadata.get("properties", [])
    if not isinstance(properties, list) or not any(
        isinstance(prop, dict)
        and prop.get("name") == "cdx:rustc:sbom:target:triple"
        and prop.get("value") == target
        for prop in properties
    ):
        fail(f"the SBOM does not describe target {target}")

    dependency_components = document.get("components")
    dependencies = document.get("dependencies")
    if not isinstance(dependencies, list) or not dependencies:
        fail("the SBOM has no dependency graph")
    if not any(
        isinstance(component, dict)
        and component.get("name") == CRATE
        and component.get("version") == version
        for component in dependency_components
    ):
        fail(f"the SBOM does not contain {CRATE} {version}")

    all_refs = component_refs(document)
    refs = set(all_refs)
    if len(refs) != len(all_refs):
        fail("the SBOM contains duplicate component references")

    dependency_refs: set[str] = set()
    dependency_entries: set[str] = set()
    for dependency in dependencies:
        if not isinstance(dependency, dict) or not isinstance(dependency.get("ref"), str):
            fail("a dependency entry has no reference")
        if dependency["ref"] in dependency_entries:
            fail("the dependency graph contains duplicate entries")
        dependency_entries.add(dependency["ref"])
        dependency_refs.add(dependency["ref"])
        depends_on = dependency.get("dependsOn", [])
        if not isinstance(depends_on, list) or not all(
            isinstance(reference, str) for reference in depends_on
        ):
            fail("a dependency edge is not a string reference")
        dependency_refs.update(depends_on)
    unknown = dependency_refs - refs
    if unknown:
        fail(f"the dependency graph contains {len(unknown)} unknown references")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--version", required=True)
    parser.add_argument("--target", required=True)
    parser.add_argument("--input", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    arguments = parser.parse_args()

    try:
        size = arguments.input.stat().st_size
    except OSError as error:
        fail(f"cannot inspect the SBOM: {error}")
    if size == 0 or size > MAX_ATTESTATION_SIZE:
        fail(f"the SBOM size {size} is outside the attestation limit")
    try:
        document = json.loads(arguments.input.read_text())
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
        fail(f"cannot read the SBOM: {error}")
    if not isinstance(document, dict):
        fail("the SBOM root is not an object")
    normalize_local_references(document)
    validate(document, arguments.version, arguments.target)

    arguments.output.mkdir(parents=True, exist_ok=True)
    destination = arguments.output / sbom_name(arguments.version, arguments.target)
    encoded = (json.dumps(document, indent=2, sort_keys=True) + "\n").encode()
    if len(encoded) > MAX_ATTESTATION_SIZE:
        fail(f"the normalized SBOM size {len(encoded)} exceeds the attestation limit")
    destination.write_bytes(encoded)
    digest = write_checksum(destination)
    print(f"prepared and verified {destination.name} ({digest})")


if __name__ == "__main__":
    main()

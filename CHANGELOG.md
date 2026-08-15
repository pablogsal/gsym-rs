# Changelog

All notable changes to this project are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Release sections use the exact form `## X.Y.Z - YYYY-MM-DD`; the release
workflow reads the newest section as the GitHub release notes.

## Unreleased

### Added

- `gsymtool convert` takes several ELF inputs and directories alongside
  `--output-dir`, scanning them for ELF images, mirroring each scanned tree into
  the destination, and processing up to `--jobs` files concurrently. `--jobs`
  does not affect output bytes.

## 0.1.0 - 2026-08-15

First release.

### Added

- Checked, borrowed readers for little- and big-endian GSYM v1 and v2, with
  memory-mapped lookup that needs no LLVM runtime or native library.
- Deterministic GSYM v1 and v2 writers, whole-file transcoding between
  versions and byte orders, and size-bounded shards.
- Conversion from linked or relocatable Linux ELF files containing DWARF,
  including GNU debug-link, build-ID, debuginfod, supplementary, and
  split-DWARF discovery.
- `gsymtool`, a command-line program for conversion, transcoding, lookup,
  inspection, and validation.

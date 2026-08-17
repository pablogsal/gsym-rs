# Changelog

## 0.1.4 - 2026-08-17

### Added

- Publish `gsymtool` with the `gsym-rs` crate so `cargo install gsym-rs --locked` installs the complete command-line tool.

### Changed

- Enable conversion, memory mapping, and debuginfod in the default feature set.
- Keep binary release metadata, SBOM generation, and fuzzing aligned with the unified Cargo package.

## 0.1.3 - 2026-08-17

### Fixed

- Resolve relative split-DWARF files from both linked-image and recorded working-directory locations, validating DWO identifiers before import.
- Preserve executable line rows and declaration and inline file indexes across GCC DWARF 4 and 5 DWO and DWP data.
- Normalize duplicate and overlapping inline ranges without changing address coverage, and apply the GSYM depth limit only to the resulting inline tree.

### Performance

- Reduce conversion time and peak memory by compacting retained inline trees and replacing hash-based relocation maps with sorted tables.

## 0.1.2 - 2026-08-16

### Fixed

- Treat zero, `-1`, and `-2` DWARF addresses outside executable sections as linker tombstones while retaining diagnostics for unexpected ranges.
- Aggregate repeated range diagnostics, suppress nonfatal warnings in quiet mode, and retain individual diagnostics in verbose mode.

### Documentation

- Add LLVM benchmark results and expand the format, conversion, and symbolication guides.

## 0.1.1 - 2026-08-15

### Added

- Resolve supplementary DWARF from `.gnu_debugaltlink`, including debuginfod fallback.
- Report debuginfod activity to callers, show live discovery progress in `gsymtool`, and honor configured servers in batch mode unless `--no-debuginfod` is used.
- Add explicit function-set policies while retaining the existing builder controls.

### Fixed

- Reject malformed nested records, invalid inline file references, bad reserved entries, and unsupported records during full decode and transcode.
- Preserve the richest duplicate function regardless of name ordering and compact redundant line states without changing lookup results.
- Correct statement-sequence selection, declaration fallback, cross-unit references, integer decoding, and bounded LEB handling.
- Coalesce concurrent debuginfod requests without preventing later retries.

### Performance

- Avoid duplicate symbol-name storage during ELF conversion and redundant reference walks during owned decoding.
- Keep lookup validation off the hot path and reserve result storage exactly.
- Use mimalloc for the Linux `gsymtool` binaries.

## 0.1.0 - 2026-08-15

Initial release.

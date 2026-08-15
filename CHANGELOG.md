# Changelog

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

# Compatibility and scope

The wire implementation was checked against LLVM main at commit
`dd19d5210352f004758d0a877b396ea9f04cfd70` and against the installed LLVM 21
tools on Linux. GSYM v2 originally landed in LLVM commit
`c3650687e0b7317b686b9187d15d2c4c63e05f8b`; v1 remains the default because
v2 requires LLVM 23 or newer.

The differential interoperability gate covers both directions:

- LLVM 21 reads and symbolizes a v1 file written by `gsym-rs`.
- `gsym-rs` verifies and symbolizes a v1 file written by LLVM 21.
- Independent fixtures cover v1 and v2 in little- and big-endian byte order.

For ELF conversion, the GSYM base address follows LLVM's
`getImageBaseAddress`: the `p_vaddr` of the first `PT_LOAD` program header. The
writer chooses the smallest canonical address-offset width from 1, 2, 4, or 8
bytes. String offset zero and file index zero are reserved for their required
empty entries.

## Converter coverage

The converter accepts linked `ET_EXEC`/`ET_DYN` images and relocatable `ET_REL`
objects. It supports:

- `.gnu_debuglink` CRC, local build-ID, and bounded debuginfod discovery;
- `.gnu_debugaltlink` supplementary files, `.dwo` split units, and indexed
  `.dwp` packages;
- bounded pure-Rust XZ decoding of `.gnu_debugdata` mini-debug ELF files, with
  architecture and build-ID validation when IDs are present;
- local, cross-unit, and supplementary name/call-origin references;
- absolute, PC-relative, and section-relative DWARF relocations plus synthetic
  text bases for x86-64 and `AArch64` `ET_REL` inputs;
- dead-range filtering, malformed-range diagnostics, and sequence-aware line
  clamping and declaration-location fallback;
- `DW_AT_LLVM_stmt_sequence` selection, invalid-offset recovery, and sentinel
  handling;
- direct-child call sites with neutral DWARF flags;
- recursive inline richness when selecting duplicate function records.

These are ELF/DWARF import limits, not GSYM codec limits. The reader and writer
handle v1/v2 line tables, inline information, merged functions, and call sites.

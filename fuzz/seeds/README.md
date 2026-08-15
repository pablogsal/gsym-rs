# Fuzz seeds

These reviewed inputs are the immutable starting corpus used locally and in CI.
`parse` and `lookup` contain valid GSYM v1/v2 files in both byte orders.
`roundtrip` seeds the structured model generator, `mutate` selects each
valid-file mutation strategy, and `writer` selects duplicate/finalization
policies. `elf_convert` contains linked DWARF 2 through 5 ELFs, an `ET_REL` object,
zlib-compressed debug sections, a standalone DWO, a DWP package with its
skeleton image, and a stripped image paired with separate debug information.

Regenerate the GSYM fixtures with:

```console
cargo run --example generate_fuzz_corpus
LLVM_DWP=llvm-dwp fuzz/fixtures/generate-elf-seeds.sh
```

The ELF generator uses `CC`, `SPLIT_CC`, `OBJCOPY`, and `LLVM_DWP` when set.
It discovers a versioned `/usr/lib/llvm-*/bin/llvm-dwp` automatically on common
Linux installations. Split DWARF uses DWARF 4 because it interoperates with
both GNU producers and released `llvm-dwp` versions.

The mutable, minimized corpus belongs in `fuzz/corpus/`; that directory is
intentionally ignored. Promote a particularly valuable input into this folder
or a deterministic regression test after reviewing it.

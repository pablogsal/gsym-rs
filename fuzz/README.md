# Fuzz harness

This directory contains the contributor-facing fuzz harness. The public crate
documentation intentionally describes the supported API and format behavior,
not the mechanics of maintaining this test suite.

The suite uses `cargo-fuzz`, libFuzzer, and AddressSanitizer. It combines raw
byte mutation with bounded structured generation so it reaches line, inline,
call-site, writer, transcoding, segmentation, and ELF conversion behavior.

## Targets

| Target | Input and invariant |
| --- | --- |
| `parse` | Arbitrary bytes; parsing, verification, owned decoding, and verified transcoding must not panic or access invalid memory. |
| `lookup` | Arbitrary bytes and lookup controls; allocating lookup must agree with the frame visitor. |
| `roundtrip` | Bounded semantic models; v1/v2 and either byte order must round-trip deterministically. |
| `mutate` | Rich valid GSYM images with distributed mutations; deep records must fail safely or remain semantically valid. |
| `writer` | Duplicate and finalization controls; ordering, merging, and zero-size repair must remain deterministic and valid. |
| `elf_convert` | Linux ELF/debug inputs; successful conversion must produce a GSYM image that parses and verifies. |

All generated models and conversion settings are bounded. Inline depth, record
counts, string lengths, decompressed sections, and downloaded data have
explicit limits.

## Run locally

Install the framework version used by CI:

```console
cargo install cargo-fuzz --version 0.13.2 --locked
```

From the repository root, run a five-minute parser campaign:

```console
cargo +nightly fuzz run parse fuzz/corpus/parse fuzz/seeds/parse -- \
  -dict=fuzz/gsym.dict -max_total_time=300 -timeout=10 -rss_limit_mb=2048
```

Run ELF conversion with its dictionary and reviewed fixtures:

```console
cargo +nightly fuzz run elf_convert \
  fuzz/corpus/elf_convert fuzz/seeds/elf_convert -- \
  -dict=fuzz/elf.dict -max_len=65536
```

The first corpus directory is mutable and ignored by version control.
`fuzz/seeds/` contains reviewed, reproducible inputs.

Regenerate GSYM seeds with:

```console
cargo run --example generate_fuzz_corpus
```

Regenerate Linux ELF/DWARF seeds with:

```console
LLVM_DWP=llvm-dwp fuzz/fixtures/generate-elf-seeds.sh
```

Those fixtures cover DWARF 2 through 5, linked and relocatable ELF, compressed debug
sections, separate debug information, and split DWARF in DWO and DWP forms.
Multi-file inputs use a small fuzz-only envelope; the library API continues to
receive ordinary borrowed byte slices.

## Reproduce and minimize failures

libFuzzer writes failures to `fuzz/artifacts/<target>/`:

```console
cargo +nightly fuzz run lookup fuzz/artifacts/lookup/crash-...
cargo +nightly fuzz tmin lookup fuzz/artifacts/lookup/crash-...
cargo +nightly fuzz cmin lookup \
  fuzz/corpus/lookup-minimized fuzz/corpus/lookup fuzz/seeds/lookup
```

After diagnosis, add a focused deterministic regression test. Promote a
minimized input into `fuzz/seeds/` only when it contributes a reusable format
shape or state transition.

## Coverage

```console
rustup component add llvm-tools-preview --toolchain nightly
cargo +nightly fuzz coverage lookup fuzz/corpus/lookup fuzz/seeds/lookup
```

Coverage is a navigation aid. Review which records, errors, lookup options,
and conversion stages are reached rather than optimizing only a percentage.

The fuzz workflow compiles every target and runs sanitizer-backed smoke
campaigns on changes. Scheduled campaigns run longer and upload failures by
target.

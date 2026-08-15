# gsym-rs

`gsym-rs` is a pure-Rust reader, writer, and Linux ELF/DWARF converter for
[LLVM GSYM](https://llvm.org/doxygen/dir_11913c55ade52754878c574ae3024754.html).
GSYM keeps the information needed to turn an instruction address into a
function, source location, and inline call stack while remaining compact and
friendly to memory mapping.

The project provides:

- checked, borrowed readers for little- and big-endian GSYM;
- memory-mapped lookup without an LLVM runtime or native library;
- deterministic GSYM v1 and v2 writers;
- whole-file v1/v2/endian transcoding and size-bounded shards;
- conversion from linked or relocatable Linux ELF files containing DWARF;
- GNU debug-link, build-ID, debuginfod, supplementary, and split-DWARF discovery;
- a `gsymtool` command-line program for conversion and inspection.

The library does not use `BlazeSym` and does not invoke LLVM tools at runtime.
The full development suite requires `llvm-gsymutil`, `llvm-dwp`, and
`yaml2obj` as interoperability and fixture oracles.

## Documentation

The API documentation carries four guide pages under `gsym::docs`. Build it with
`cargo doc --open`, or read the sources here:

- [Symbolication](docs/symbolication.md): which address to look up, how to read
  the frames that come back, storage choices, performance, and threading.
- [Cookbook](docs/cookbook.md): worked examples for each part of the public API.
- [Conversion](docs/cookbook-convert.md): building GSYM from ELF and DWARF.
- [Format](docs/format.md): the on-disk format, for debugging a file or
  comparing against `llvm-gsymutil`.

## Command line

Build the CLI with the workspace:

```console
cargo build --release --workspace
```

Convert a linked ELF executable or shared library containing DWARF:

```console
target/release/gsymtool convert ./app -o ./app.gsym
target/release/gsymtool convert ./app -o ./app.gsym --version v2
target/release/gsymtool convert ./app -o ./app.gsym --dwp ./app.dwp
target/release/gsymtool transcode ./app.gsym -o ./app-v2.gsym --version v2 --endian big
target/release/gsymtool segment ./app-v2.gsym -o ./app-shard.gsym --size 64MiB
```

Convert many at once with `--output-dir`. Directory inputs are scanned for ELF
files, `--recursive` descends into subdirectories, and every output mirrors its
input under the destination root, so `bin/sub/app` becomes `gsym/sub/app.gsym`:

```console
target/release/gsymtool convert ./bin --output-dir ./gsym --recursive
target/release/gsymtool convert ./a.so ./b.so --output-dir ./gsym --jobs 8
```

`--jobs` does not affect output bytes. Non-ELF paths and ELF files with nothing
to symbolize are skipped and counted. Conversion failures are reported without
stopping the remaining jobs; any failure makes the command exit non-zero.

Look up one or more virtual addresses. Addresses may be decimal or prefixed
with `0x`:

```console
target/release/gsymtool lookup ./app.gsym 0x401120 0x40113a
```

Inspect metadata and functions, or validate every indexed function record:

```console
target/release/gsymtool dump ./app.gsym
target/release/gsymtool dump ./app.gsym --functions
target/release/gsymtool verify ./app.gsym
```

Byte-size arguments accept binary units such as `KiB`, `MiB`, and `GiB`, or
decimal `KB`, `MB`, and `GB`. Terminal colors follow `NO_COLOR` and `CLICOLOR`
and can be overridden with `--color always|never`. Use
`gsymtool completions <shell>` to generate completion scripts for Bash, Zsh,
Fish, Elvish, or PowerShell.

`lookup` expects the unslid virtual addresses recorded in the input ELF. A
runtime address from a PIE or shared object must first have its load bias
removed.

## Library use

The default features are `mmap` and `convert`. The `debuginfod` feature adds
network lookup through the dedicated Rust debuginfod client and is enabled by
`gsymtool`. Disable default features when a
consumer only needs the in-memory codec:

```toml
[dependencies]
gsym-rs = { version = "0.1", default-features = false }
```

The public API is split by responsibility:

- `Gsym<D>` parses any `D: AsRef<[u8]>`, borrowing or owning the storage;
- `Gsym::open` safely reads an immutable owned snapshot from a path;
- `MappedGsym` is the same reader backed by a read-only mapping;
- `GsymBuilder` creates a semantic GSYM image from function records;
- `DecodedGsym` owns a complete file for rewriting or segmentation;
- `ElfConverter` imports symbols and DWARF from Linux ELF input.

`Gsym::for_each_frame` is the allocation-free result path: it visits borrowed
frames innermost-first and reuses caller-owned traversal scratch. The ergonomic
`lookup` methods collect those values into compact boxed slices.

Function names and paths are represented as bytes by the format. The library
does not reject non-UTF-8 data; presentation layers may render it lossily.

The common in-memory path is small and does not require optional features:

```rust
use gsym::{AddressRange, FileEntry, Function, Gsym, GsymBuilder, LineEntry};

let mut builder = GsymBuilder::new().base_address(0x4000);
let source = builder.add_file(FileEntry::new(b"src", b"main.rs"))?;
builder.add_function(Function {
    lines: vec![LineEntry::new(0x4010, source, 12)],
    ..Function::new(AddressRange::new(0x4010, 0x4020), b"example")
})?;

let bytes = builder.to_bytes()?;
let gsym = Gsym::parse(&bytes)?;
let symbol = gsym.lookup(0x4014)?.expect("address is covered");
assert_eq!(symbol.frames[0].name, b"example");

# Ok::<(), gsym::Error>(())
```

For ELF conversion, `ElfConverter::default().convert_path(path)` handles the
normal discovery flow. Call `convert(ElfInputs::new(bytes))` for caller-owned
input, with `with_debug`, `with_symbols`, `with_supplementary`, or `with_dwp`
when companions are already available in memory.

## Conversion scope

The converter accepts linked `ET_EXEC`/`ET_DYN` images and relocatable `ET_REL`
objects. Linked images retain virtual addresses. Relocatable inputs apply DWARF
relocations and assign deterministic, aligned bases to their text sections so
functions from section-per-function objects remain distinct.

The input must contain usable symbols or DWARF. GSYM cannot reconstruct source
locations that were removed before conversion.

Separate debug files are accepted explicitly or discovered through validated
`.gnu_debuglink` CRCs, local GNU build-ID trees, and debuginfod. Network lookup
is enabled only when `DEBUGINFOD_URLS` contains one or more server roots.
Downloaded files have a 1 GiB default limit, are checked against the requested
build ID and image architecture, and are stored under `DEBUGINFOD_CACHE_PATH`
or a temporary cache root. XZ-compressed `.gnu_debugdata` mini-debug ELF files
are unpacked in pure Rust with a configurable 1 GiB default limit.
`.gnu_debugaltlink` supplementary files and compiler-generated `.dwo` files
are loaded by path. Indexed `.dwp` packages can be supplied explicitly or are
discovered beside the image when individual DWO files are unavailable.
Names and call origins follow unit-local, cross-unit, and supplementary
references with cycle limits. The converter also filters dead ranges, clamps
line rows within their statement sequence, honors `DW_AT_LLVM_stmt_sequence`,
falls back to `DW_AT_decl_file` and `DW_AT_decl_line` when a function has no
usable rows, tolerates missing file declarations, and reports malformed ranges
without discarding unrelated units. Relocatable x86-64 and `AArch64` inputs
support absolute, PC-relative, and section-relative generic debug relocations.

## Compatibility

The upstream interoperability baselines and remaining limits are recorded in
[`docs/compatibility.md`](docs/compatibility.md). Case-by-case compatibility
coverage is recorded in
[`docs/compatibility-coverage.md`](docs/compatibility-coverage.md).

## License

Licensed under either of:

- Apache License, Version 2.0
- MIT License

at your option.

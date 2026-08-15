# Conversion

Building GSYM from Linux ELF files and their DWARF. Requires the `convert`
feature, which is on by default.

## What the converter produces

[`ElfConverter`](crate::convert::ElfConverter) reads an ELF image, imports
`STT_FUNC` symbols and DWARF subprogram information, and returns a populated
[`GsymBuilder`](crate::GsymBuilder) rather than bytes. The extra step lets you
inspect or edit the model and choose a version and byte order before encoding.

```no_run
use gsym::convert::ElfConverter;

let report = ElfConverter::default().convert_path("./app")?;
std::fs::write("./app.gsym", report.builder.to_bytes()?)?;
# Ok::<(), gsym::Error>(())
```

Byte order, base address, and build ID come from the image rather than from the
caller. The output uses the ELF's byte order, its base address comes from the
image's executable sections, and an empty
[`WriterOptions::build_id`](crate::WriterOptions) is filled in from the image's
GNU build ID.

The input must contain symbols or DWARF. GSYM cannot reconstruct source
locations that were stripped before conversion; it can only re-encode what is
present.

## Companion file discovery

Debug information often does not live in the shipped binary.
[`convert_path`](crate::convert::ElfConverter::convert_path) therefore searches
for it, in this order, unless
[`DiscoveryPolicy::Disabled`](crate::convert::DiscoveryPolicy) is set:

- `.gnu_debuglink`, a name and CRC-32 embedded in the image. A candidate is
  accepted only if the CRC matches, so a stale or mismatched file is skipped
  instead of being used.
- Build-ID trees, at `<root>/.build-id/ab/cdef….debug` under each of
  [`debug_directories`](crate::convert::ConversionOptions) (`/usr/lib/debug` by
  default), plus the mirrored-path layout beside it.
- debuginfod, tried only when
  [`debuginfod_urls`](crate::convert::ConversionOptions) is non-empty, which by
  default means only when `DEBUGINFOD_URLS` is set in the environment. Downloads
  are capped by
  [`debuginfod_max_download_size`](crate::convert::ConversionOptions) (1 GiB),
  checked against the requested build ID and the image's architecture, and
  cached under `DEBUGINFOD_CACHE_PATH` or a temporary root. This is the only
  step that touches the network.
- `.gnu_debugdata`, the XZ-compressed mini-debug ELF some distributions embed.
  It is decompressed with a 1 GiB default bound and validated against the image
  before use.

Two further kinds of companion are resolved from whichever debug file is
selected: `.gnu_debugaltlink` supplementary objects, and split DWARF as either
individual `.dwo` files by path or an indexed `.dwp` package, supplied
explicitly or found beside the image. `Disabled` stops the `.dwp` search, but a
`.gnu_debugaltlink` reference is still followed.

The report says what was chosen, which is worth logging when a conversion
produces unexpected output:

```no_run
use gsym::convert::ElfConverter;

let report = ElfConverter::default().convert_path("./app")?;
println!("debug:         {:?}", report.discovered_debug);
println!("supplementary: {:?}", report.discovered_supplementary);
println!("dwp:           {:?}", report.discovered_dwp);
# Ok::<(), gsym::Error>(())
```

## Controlling conversion

[`ConversionOptions`](crate::convert::ConversionOptions) is a plain struct;
build it from `Default` and adjust. Its defaults read `DEBUGINFOD_URLS` and
`DEBUGINFOD_CACHE_PATH` from the environment, so a process that must not reach
the network should clear
[`debuginfod_urls`](crate::convert::ConversionOptions) explicitly rather than
rely on the environment.

```no_run
use gsym::convert::{
    ConversionOptions, DiscoveryPolicy, DwarfImportOptions, ElfConverter,
};
use gsym::GsymVersion;

let mut options = ConversionOptions::default();
options.writer.version = GsymVersion::V2;
options.include_symbols = true;
options.dwarf = Some(DwarfImportOptions {
    inline_info: true,
    call_sites: true, // off by default
});

// Image only: no debug links, no build-ID roots, and no network.
options.discovery = DiscoveryPolicy::Disabled;
options.debug_directories.clear();
options.debuginfod_urls.clear();

// Tighter bounds than the 1 GiB defaults.
options.debuginfod_max_download_size = 64 * 1024 * 1024;
options.gnu_debugdata_max_decompressed_size = 64 * 1024 * 1024;

let converter = ElfConverter::new(options);
assert_eq!(converter.options().discovery, DiscoveryPolicy::Disabled);
let report = converter.convert_path("./app")?;
# Ok::<(), gsym::Error>(())
```

Setting `dwarf` to `None` imports symbol names only, with no lines and no inline
frames. That is fast and produces a much smaller file, and it is enough when the
consumer only prints `function+offset`.

## Supplying inputs yourself

When the bytes are already in memory, or the companion files come from somewhere
other than the filesystem, describe them with
[`ElfInputs`](crate::convert::ElfInputs) and call
[`convert`](crate::convert::ElfConverter::convert). This form runs no discovery:
what you pass is what is used.

```no_run
use gsym::convert::{ElfConverter, ElfInputs};

let image = std::fs::read("app")?;
let debug = std::fs::read("app.debug")?;
let dwp = std::fs::read("app.dwp")?;

let inputs = ElfInputs::new(&image)
    .with_debug(&debug)
    .with_dwp(&dwp);

let report = ElfConverter::default().convert(inputs)?;
assert!(!report.builder.functions().is_empty());
# Ok::<(), gsym::Error>(())
```

Explicit companions are cross-checked against the image. A debug or symbol file
whose architecture or build ID disagrees is rejected with
[`Error::CompanionMismatch`](crate::Error), which names the input role
([`ElfInputKind`](crate::ElfInputKind)) and the property that differs
([`CompanionMismatch`](crate::CompanionMismatch)).

## Reading the report

[`ConversionStats`](crate::convert::ConversionStats) counts what was imported.
The number to watch is `rejected_ranges`, which counts invalid, dead, or
unrepresentable ranges that were dropped. A few are normal for optimized code; a
large fraction usually means the DWARF does not match the image.

```no_run
use gsym::convert::ElfConverter;

let report = ElfConverter::default().convert_path("./app")?;
let stats = report.stats;
println!(
    "symbols={} dwarf={} lines={} inlines={} rejected={}",
    stats.symbol_functions,
    stats.dwarf_functions,
    stats.line_rows,
    stats.inline_nodes,
    stats.rejected_ranges,
);
for warning in &report.warnings {
    eprintln!("warning: {warning}");
}
# Ok::<(), gsym::Error>(())
```

[`ConversionWarning`](crate::convert::ConversionWarning) is the non-fatal
channel. It reports unusable mini-debug data, failed debuginfod requests,
split-DWARF units that could not be found, malformed range lists, line rows
referencing a missing file, and similar problems. Conversion continues in each
case and the affected records are skipped. Warnings implement `Display`, and the
enum is `#[non_exhaustive]`, so matching specific variants needs a fallback arm.

## Relocatable objects

Besides linked `ET_EXEC` and `ET_DYN` images, the converter accepts `ET_REL`
object files on x86-64 and `AArch64`. An object has no virtual addresses, so
each text section is given a synthetic base, which keeps functions distinct even
in objects built with `-ffunction-sections`.

Addresses in the resulting file are therefore synthetic too. They are useful for
inspecting the object's own contents, not for symbolicating a process that
loaded a binary linked from it.

## Further reading

Elsewhere in these docs:

- [Format](crate::docs::format) for what conversion is writing into
- [Symbolication](crate::docs::symbolication) for using the result at runtime

On the inputs, when a conversion produces less than expected:

- [GDB: separate debug files](https://sourceware.org/gdb/current/onlinedocs/gdb.html/Separate-Debug-Files.html)
  covers `.gnu_debuglink`, build-ID trees, and how `objcopy --only-keep-debug`
  splits a binary
- [GDB: MiniDebugInfo](https://sourceware.org/gdb/current/onlinedocs/gdb.html/MiniDebugInfo.html)
  covers what `.gnu_debugdata` does and does not contain
- [debuginfod](https://sourceware.org/elfutils/Debuginfod.html) documents
  `DEBUGINFOD_URLS`, `DEBUGINFOD_CACHE_PATH`, and the servers behind them
- [Debug fission](https://gcc.gnu.org/wiki/DebugFission) and
  [DWP packages](https://gcc.gnu.org/wiki/DebugFissionDWP) cover `-gsplit-dwarf`,
  `.dwo` files, and the `.dwp` index
- [DWARF 5 standard](https://dwarfstd.org/doc/DWARF5.pdf) defines the debugging
  information this importer reads
- [`llvm-gsymutil`](https://github.com/llvm/llvm-project/tree/main/llvm/tools/llvm-gsymutil)
  is the upstream converter

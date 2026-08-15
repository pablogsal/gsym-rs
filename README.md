<div align="center">
  <img src="https://raw.githubusercontent.com/pablogsal/gsym-rs/main/assets/mascot-compact.png" alt="gsym-rs detective crab mascot" width="325"><br>
  Pure-Rust reading, writing, and Linux ELF/DWARF conversion for
  <a href="https://llvm.org/doxygen/dir_11913c55ade52754878c574ae3024754.html">LLVM GSYM</a>.<br>
  <a href="https://pablogsal.com/gsym-rs/"><strong>Documentation</strong></a><br><br>
  <a href="https://github.com/pablogsal/gsym-rs/actions/workflows/ci.yml"><img src="https://github.com/pablogsal/gsym-rs/actions/workflows/ci.yml/badge.svg?branch=main" alt="CI"></a>
  <a href="https://app.codecov.io/github/pablogsal/gsym-rs"><img src="https://codecov.io/gh/pablogsal/gsym-rs/graph/badge.svg?branch=main" alt="Coverage"></a>
  <a href="https://app.codspeed.io/pablogsal/gsym-rs"><img src="https://img.shields.io/endpoint?url=https://codspeed.io/badge.json" alt="CodSpeed"></a>
  <a href="https://crates.io/crates/gsym-rs"><img src="https://img.shields.io/crates/v/gsym-rs.svg" alt="crates.io"></a>
  <a href="https://docs.rs/gsym-rs"><img src="https://docs.rs/gsym-rs/badge.svg" alt="docs.rs"></a>
</div>

## Install

```toml
[dependencies]
gsym-rs = "0.1"
```

Build the CLI from a checkout:

```console
cargo install --path gsymtool
```

## Read

```rust,no_run
use gsym::Gsym;

let gsym = Gsym::open("app.gsym")?;
if let Some(symbol) = gsym.lookup(0x401120)? {
    for frame in symbol.frames() {
        println!("{}", String::from_utf8_lossy(frame.name));
    }
}
# Ok::<(), gsym::Error>(())
```

## Write

```rust
use gsym::{AddressRange, FileEntry, Function, Gsym, GsymBuilder, LineEntry};

let mut builder = GsymBuilder::new().base_address(0x4000);
let source = builder.add_file(FileEntry::new(b"src", b"main.rs"))?;
builder.add_function(Function {
    lines: vec![LineEntry::new(0x4010, source, 12)],
    ..Function::new(AddressRange::new(0x4010, 0x4020), b"example")
})?;

let bytes = builder.to_bytes()?;
let gsym = Gsym::parse(bytes)?;
let symbol = gsym.lookup(0x4014)?.expect("address is covered");
assert_eq!(symbol.frames()[0].name, b"example");
# Ok::<(), gsym::Error>(())
```

## Convert ELF

```rust,no_run
# #[cfg(feature = "convert")]
# {
use gsym::convert::ElfConverter;

let report = ElfConverter::default().convert_path("./app")?;
std::fs::write("./app.gsym", report.builder.to_bytes()?)?;
# }
# Ok::<(), gsym::Error>(())
```

```console
gsymtool convert ./app -o ./app.gsym
gsymtool convert ./app -o ./app.gsym --version v2
gsymtool convert ./app -o ./app.gsym --dwp ./app.dwp
gsymtool convert ./bin --output-dir ./gsym --recursive --jobs 8
```

## Query and inspect

```console
gsymtool lookup ./app.gsym 0x401120 0x40113a
gsymtool dump ./app.gsym --functions
gsymtool verify ./app.gsym
gsymtool transcode ./app.gsym -o ./app-v2.gsym --version v2 --endian big
gsymtool segment ./app-v2.gsym -o ./app-shard.gsym --size 64MiB
```

`lookup` accepts the unslid virtual addresses stored in the ELF image. Subtract
the load bias from runtime addresses in PIE executables and shared libraries.

## Support

| Input or format | Support |
| --- | --- |
| GSYM | v1 and v2, little-endian and big-endian |
| Linux | x86-64 and AArch64 |
| Linked ELF | `ET_EXEC` and `ET_DYN` |
| Relocatable ELF | `ET_REL` |
| DWARF | Versions 2 through 5, lines, inline frames, and call sites |
| Separate debug files | Explicit paths, `.gnu_debuglink`, and GNU build-ID trees |
| Compressed debug data | SHF-compressed sections and `.gnu_debugdata` |
| Split DWARF | `.dwo` and `.dwp` |
| Supplementary DWARF | `.gnu_debugaltlink` |
| Remote debug files | debuginfod |

## Cargo features

| Feature | Default | Provides |
| --- | --- | --- |
| `mmap` | Yes | Memory-mapped GSYM files |
| `convert` | Yes | Linux ELF and DWARF conversion |
| `debuginfod` | No | Remote separate-debug discovery |

Reader and writer only:

```toml
[dependencies]
gsym-rs = { version = "0.1", default-features = false }
```

## License

Licensed under either Apache-2.0 or MIT, at your option.

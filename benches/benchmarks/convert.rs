use criterion::Criterion;

#[cfg(all(feature = "convert", target_os = "linux"))]
use std::fmt::Write as _;
#[cfg(all(feature = "convert", target_os = "linux"))]
use std::hint::black_box;
#[cfg(all(feature = "convert", target_os = "linux"))]
use std::process::Command;

#[cfg(all(feature = "convert", target_os = "linux"))]
use criterion::Throughput;
#[cfg(all(feature = "convert", target_os = "linux"))]
use gsym::convert::{ConversionOptions, DwarfImportOptions, ElfConverter, ElfInputs};

#[cfg(all(feature = "convert", target_os = "linux"))]
pub(crate) fn benchmarks(criterion: &mut Criterion) {
    let (_directory, elf) = compiled_elf();
    let configurations = [
        (
            "symbols_only",
            ConversionOptions {
                dwarf: None,
                ..ConversionOptions::default()
            },
        ),
        (
            "dwarf_lines_only",
            ConversionOptions {
                include_symbols: false,
                dwarf: Some(DwarfImportOptions {
                    inline_info: false,
                    call_sites: false,
                }),
                ..ConversionOptions::default()
            },
        ),
        (
            "dwarf_lines_and_inlines",
            ConversionOptions {
                include_symbols: false,
                dwarf: Some(DwarfImportOptions {
                    inline_info: true,
                    call_sites: false,
                }),
                ..ConversionOptions::default()
            },
        ),
        ("symbols_and_full_dwarf", ConversionOptions::default()),
    ];

    let mut group = criterion.benchmark_group("convert/elf_dwarf");
    group.throughput(Throughput::Bytes(elf.len() as u64));
    for (name, options) in configurations {
        let converter = ElfConverter::new(options);
        group.bench_function(name, |bencher| {
            bencher.iter(|| {
                black_box(
                    converter
                        .convert(ElfInputs::new(black_box(elf.as_slice())))
                        .expect("compiler-produced ELF fixture must convert"),
                )
            });
        });
    }
    group.finish();
}

#[cfg(all(feature = "convert", target_os = "linux"))]
fn compiled_elf() -> (tempfile::TempDir, Vec<u8>) {
    const FUNCTIONS: usize = 256;

    let directory = tempfile::tempdir().expect("converter benchmark directory must be created");
    let source_path = directory.path().join("fixture.c");
    let image_path = directory.path().join("fixture");
    let mut source = String::from("static inline int bump(int value) { return value * 3 + 1; }\n");
    for index in 0..FUNCTIONS {
        writeln!(
            source,
            "__attribute__((noinline)) int function_{index}(int value) {{ return bump(value) + {index}; }}"
        )
        .expect("writing C fixture to a string cannot fail");
    }
    source.push_str("int main(void) {\n  int value = 0;\n");
    for index in 0..FUNCTIONS {
        writeln!(source, "  value += function_{index}(value);")
            .expect("writing C fixture to a string cannot fail");
    }
    source.push_str("  return value;\n}\n");
    std::fs::write(&source_path, source).expect("converter benchmark source must be written");

    let compiler = std::env::var_os("CC").unwrap_or_else(|| "cc".into());
    let output = Command::new(&compiler)
        .args([
            "-g",
            "-gdwarf-5",
            "-O2",
            "-fno-omit-frame-pointer",
            "-Wl,--build-id=none",
            "-o",
        ])
        .arg(&image_path)
        .arg(&source_path)
        .output()
        .expect("C compiler must be available for the converter benchmark");
    assert!(
        output.status.success(),
        "C compiler failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let elf = std::fs::read(&image_path).expect("compiled converter benchmark ELF must be read");
    (directory, elf)
}

#[cfg(not(all(feature = "convert", target_os = "linux")))]
pub(crate) const fn benchmarks(_criterion: &mut Criterion) {}

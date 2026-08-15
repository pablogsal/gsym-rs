//! Cross-checking our output against LLVM's own `llvm-gsymutil`.

use std::process::Command;

use gsym::Gsym;
use gsym::convert::{ConversionOptions, ElfConverter};

use crate::tools::{required_tool, run};

#[test]
fn rust_and_external_gsym_tools_read_each_others_output() {
    let gsymutil = required_tool("llvm-gsymutil");
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("interop.c");
    let image = directory.path().join("interop");
    let rust_gsym = directory.path().join("rust.gsym");
    let external_gsym = directory.path().join("external.gsym");
    std::fs::write(
        &source,
        "__attribute__((noinline)) int interop_target(int x) { return x + 9; }\nint main(void) { return interop_target(1); }\n",
    )
    .unwrap();
    run(Command::new("cc").args([
        "-g",
        "-O1",
        "-Wl,--build-id",
        "-o",
        image.to_str().unwrap(),
        source.to_str().unwrap(),
    ]));

    let report = ElfConverter::new(ConversionOptions::default())
        .convert_path(&image)
        .unwrap();
    let address = report
        .builder
        .functions()
        .iter()
        .find(|function| function.name == b"interop_target")
        .unwrap()
        .range
        .start;
    std::fs::write(&rust_gsym, report.builder.to_bytes().unwrap()).unwrap();
    let lookup = run(Command::new(&gsymutil).args([
        rust_gsym.to_str().unwrap(),
        "--address",
        &format!("{address:#x}"),
    ]));
    assert!(
        String::from_utf8_lossy(&lookup.stdout).contains("interop_target"),
        "external lookup did not return the Rust-written function"
    );

    run(Command::new(&gsymutil).args([
        "--convert",
        image.to_str().unwrap(),
        "--num-threads=1",
        "--out-file",
        external_gsym.to_str().unwrap(),
    ]));
    let external_bytes = std::fs::read(external_gsym).unwrap();
    let parsed = Gsym::parse(&external_bytes).unwrap();
    parsed.verify().unwrap();
    let lookup = parsed.lookup(address).unwrap().unwrap();
    assert_eq!(lookup.frames.last().unwrap().name, b"interop_target");
}

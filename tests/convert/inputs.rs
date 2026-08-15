//! The ELF inputs the converter accepts: images, companions and object files.

use std::process::Command;

use gsym::convert::{ConversionOptions, ElfConverter, ElfInputs};
use gsym::{CompanionMismatch, ElfInputKind, Error, Gsym};

use crate::elf::{convert, corrupt_build_id, current_image, find_function, retarget_machine};
use crate::tools::{required_tool, run};

#[test]
fn rejects_non_elf_input() {
    let error = ElfConverter::new(ConversionOptions::default())
        .convert(ElfInputs::new(b"not an ELF file"))
        .unwrap_err();
    assert!(matches!(error, Error::Object { .. } | Error::NotElf { .. }));
}

#[test]
fn converts_the_linked_test_image_from_symbols() {
    let executable = current_image();
    let options = ConversionOptions {
        dwarf: None,
        ..ConversionOptions::default()
    };
    let converter = ElfConverter::new(options.clone());
    assert_eq!(converter.options(), &options);
    let report = converter.convert(ElfInputs::new(executable)).unwrap();

    assert!(report.stats.symbol_functions > 0);
    assert!(!report.builder.functions().is_empty());
    let bytes = report.builder.to_bytes().unwrap();
    let gsym = Gsym::parse(&bytes).unwrap();
    gsym.verify().unwrap();
}

#[test]
fn symtab_file_equivalent_accepts_a_matching_companion() {
    let image = current_image();
    let report = convert(ElfInputs::new(image).with_debug(image).with_symbols(image)).unwrap();
    assert!(report.stats.symbol_functions > 0);
    assert!(
        report.stats.dwarf_functions > 0,
        "conversion warnings: {:?}",
        report.warnings
    );
    let bytes = report.builder.to_bytes().unwrap();
    Gsym::parse(&bytes).unwrap().verify().unwrap();
}

#[test]
fn symtab_file_equivalent_rejects_an_architecture_mismatch() {
    let image = current_image();
    let mut other_architecture = image.to_vec();
    retarget_machine(&mut other_architecture);

    let error = convert(ElfInputs::new(image).with_symbols(&other_architecture)).unwrap_err();
    assert!(matches!(
        error,
        Error::CompanionMismatch {
            input: ElfInputKind::Symbols,
            mismatch: CompanionMismatch::Architecture
        }
    ));
}

#[test]
fn image_dynamic_symbols_survive_a_companion_without_a_symbol_table() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("dynamic.c");
    let library = directory.path().join("libdynamic.so");
    let stripped = directory.path().join("libdynamic.stripped.so");
    let debug = directory.path().join("libdynamic.debug");
    std::fs::write(&source, "int exported_only(int x) { return x + 1; }\n").unwrap();
    run(Command::new("cc").args([
        "-shared",
        "-fPIC",
        "-g",
        "-O1",
        "-Wl,--build-id",
        "-o",
        library.to_str().unwrap(),
        source.to_str().unwrap(),
    ]));
    run(Command::new("objcopy").args([
        "--strip-all",
        library.to_str().unwrap(),
        stripped.to_str().unwrap(),
    ]));
    run(Command::new("objcopy").args([
        "--only-keep-debug",
        stripped.to_str().unwrap(),
        debug.to_str().unwrap(),
    ]));

    let image = std::fs::read(&stripped).unwrap();
    let companion = std::fs::read(&debug).unwrap();
    let report = convert(ElfInputs::new(&image).with_debug(&companion)).unwrap();

    assert!(report.stats.symbol_functions > 0);
    let function = find_function(&report, b"exported_only").unwrap();
    assert!(!function.range.is_empty());
    Gsym::parse(&report.builder.to_bytes().unwrap())
        .unwrap()
        .verify()
        .unwrap();
}

#[test]
fn separate_debug_file_rejects_a_build_id_mismatch() {
    let image = current_image();
    let mut mismatched = image.to_vec();
    corrupt_build_id(&mut mismatched);

    let error = convert(ElfInputs::new(image).with_debug(&mismatched)).unwrap_err();
    assert!(matches!(
        error,
        Error::CompanionMismatch {
            input: ElfInputKind::Debug,
            mismatch: CompanionMismatch::BuildId
        }
    ));
}

#[test]
fn relocatable_elf_applies_debug_relocations_and_assigns_section_bases() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("relocatable.c");
    let object = directory.path().join("relocatable.o");
    std::fs::write(
        &source,
        "volatile int bias = 1;\n__attribute__((noinline)) int alpha(int x) { return x + bias; }\n__attribute__((noinline)) int beta(int x) { return x + 2; }\n",
    )
    .unwrap();
    run(Command::new("cc").args([
        "-g",
        "-O1",
        "-ffunction-sections",
        "-c",
        "-o",
        object.to_str().unwrap(),
        source.to_str().unwrap(),
    ]));
    let bytes = std::fs::read(object).unwrap();
    let report = convert(ElfInputs::new(&bytes)).unwrap();
    assert!(report.stats.symbol_functions >= 2);
    assert!(report.stats.dwarf_functions >= 2);
    let alpha = find_function(&report, b"alpha").unwrap();
    let beta = find_function(&report, b"beta").unwrap();
    assert_ne!(alpha.range.start, beta.range.start);
    Gsym::parse(&report.builder.to_bytes().unwrap())
        .unwrap()
        .verify()
        .unwrap();
}

#[test]
fn relocatable_aarch64_elf_applies_debug_relocations() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("relocatable-aarch64.c");
    let object = directory.path().join("relocatable-aarch64.o");
    std::fs::write(
        &source,
        "__attribute__((noinline)) int arm_target(int x) { return x + 3; }\n",
    )
    .unwrap();
    run(Command::new(required_tool("clang")).args([
        "--target=aarch64-linux-gnu",
        "-g",
        "-O1",
        "-ffunction-sections",
        "-c",
        "-o",
        object.to_str().unwrap(),
        source.to_str().unwrap(),
    ]));

    let bytes = std::fs::read(object).unwrap();
    let report = convert(ElfInputs::new(&bytes)).unwrap();
    let function = report
        .builder
        .functions()
        .iter()
        .find(|function| function.name == b"arm_target" && !function.lines.is_empty())
        .unwrap();
    assert!(!function.range.is_empty());
    assert!(report.stats.line_rows > 0, "{:?}", report.warnings);
    Gsym::parse(&report.builder.to_bytes().unwrap())
        .unwrap()
        .verify()
        .unwrap();
}

//! Split DWARF: individual `.dwo` files and the `.dwp` packages that bundle them.

use std::process::Command;
use std::time::{Duration, Instant};

use gsym::Error;
use gsym::convert::{
    ConversionOptions, ConversionWarning, DiscoveryPolicy, ElfConverter, ElfInputs,
};
use object::endian::Endian;
use object::{Object, ObjectSection, ObjectSymbol};

use crate::elf::{convert, find_function};
use crate::tools::{required_tool, run};

const SPLIT_FUNCTIONS: &[&[u8]] = &[b"split_a", b"split_b", b"split_c", b"main"];

/// This caps `llvm-dwp`, which builds the fixtures: it has been seen to hang on
/// some inputs, and a hung packager should fail the test rather than the run.
/// The timeout does not cover the converter itself.
fn run_bounded(command: &mut Command, timeout: Duration) {
    let mut child = command.spawn().unwrap();
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success(), "command failed with {status}");
            return;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("command exceeded {timeout:?}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn copies_skeleton_address_base_for_dwo_and_dwp() {
    let directory = tempfile::tempdir().unwrap();
    let image = compile_split_dwarf_image(directory.path());
    let dwo = dwo_files(directory.path()).remove(0);
    assert_uses_indexed_split_addresses(&image, &dwo);

    let report = ElfConverter::new(dwarf_only_options())
        .convert_path(&image)
        .unwrap();
    assert_eq!(report.stats.split_dwarf_units, 1);
    assert_function_addresses(&report, &image, SPLIT_FUNCTIONS);

    let dwp = image.with_extension("dwp");
    package_dwo_files(std::slice::from_ref(&dwo), &dwp);
    std::fs::rename(&dwo, dwo.with_extension("dwo.unavailable")).unwrap();

    let report = ElfConverter::new(dwarf_only_options())
        .convert_path(&image)
        .unwrap();
    assert_eq!(report.discovered_dwp.as_deref(), Some(dwp.as_path()));
    assert_eq!(report.stats.split_dwarf_units, 1);
    assert_function_addresses(&report, &image, SPLIT_FUNCTIONS);
}

#[test]
fn loose_dwo_selects_the_split_unit_with_the_matching_id() {
    let directory = tempfile::tempdir().unwrap();
    let image = compile_split_dwarf_image(directory.path());
    let dwo = dwo_files(directory.path()).remove(0);
    prepend_unrelated_split_unit(&dwo, directory.path());

    let report = ElfConverter::new(dwarf_only_options())
        .convert_path(&image)
        .unwrap();
    assert_eq!(report.stats.split_dwarf_units, 1);
    assert_function_addresses(&report, &image, SPLIT_FUNCTIONS);
}

fn prepend_unrelated_split_unit(dwo: &std::path::Path, directory: &std::path::Path) {
    let dwo_bytes = std::fs::read(dwo).unwrap();
    let dwo_file = object::File::parse(&*dwo_bytes).unwrap();
    let endian = dwo_file.endianness();
    let info = directory.join("debug-info.dwo.bin");
    let abbrev = directory.join("debug-abbrev.dwo.bin");
    let original_info = dwo_file
        .section_by_name(".debug_info.dwo")
        .unwrap()
        .uncompressed_data()
        .unwrap();
    let mut abbreviations = dwo_file
        .section_by_name(".debug_abbrev.dwo")
        .unwrap()
        .uncompressed_data()
        .unwrap()
        .into_owned();
    let abbreviation_offset = u32::try_from(abbreviations.len()).unwrap();

    let mut combined_info = Vec::with_capacity(21 + original_info.len());
    combined_info.extend_from_slice(&endian.write_u32(17));
    combined_info.extend_from_slice(&endian.write_u16(5));
    combined_info.push(gimli::constants::DW_UT_split_compile.0);
    combined_info.push(8);
    combined_info.extend_from_slice(&endian.write_u32(abbreviation_offset));
    combined_info.extend_from_slice(&endian.write_u64(0x0123_4567_89ab_cdef));
    combined_info.push(1);
    combined_info.extend_from_slice(&original_info);
    std::fs::write(&info, combined_info).unwrap();

    abbreviations.extend_from_slice(&[
        1,
        u8::try_from(gimli::constants::DW_TAG_compile_unit.0).unwrap(),
        gimli::constants::DW_CHILDREN_no.0,
        0,
        0,
        0,
    ]);
    std::fs::write(&abbrev, abbreviations).unwrap();
    let rewritten = directory.join("multiple-units.dwo");
    run(Command::new("objcopy")
        .arg(format!(
            "--update-section=.debug_info.dwo={}",
            info.display()
        ))
        .arg(format!(
            "--update-section=.debug_abbrev.dwo={}",
            abbrev.display()
        ))
        .arg(dwo)
        .arg(&rewritten));
    std::fs::rename(rewritten, dwo).unwrap();
}

fn assert_uses_indexed_split_addresses(image: &std::path::Path, dwo: &std::path::Path) {
    let image_bytes = std::fs::read(image).unwrap();
    let image_file = object::File::parse(&*image_bytes).unwrap();
    let endian = if image_file.is_little_endian() {
        gimli::RunTimeEndian::Little
    } else {
        gimli::RunTimeEndian::Big
    };
    let image_sections = gimli::DwarfSections::load(|id| -> object::Result<Vec<u8>> {
        image_file.section_by_name(id.name()).map_or_else(
            || Ok(Vec::new()),
            |section| Ok(section.uncompressed_data()?.into_owned()),
        )
    })
    .unwrap();
    let image_dwarf = image_sections.borrow(|section| gimli::EndianSlice::new(section, endian));
    let skeleton_header = image_dwarf.units().next().unwrap().unwrap();
    let skeleton = image_dwarf.unit(skeleton_header).unwrap();
    assert_ne!(skeleton.addr_base.0, 0);

    let dwo_bytes = std::fs::read(dwo).unwrap();
    let dwo_file = object::File::parse(&*dwo_bytes).unwrap();
    let dwo_sections = gimli::DwarfSections::load(|id| -> object::Result<Vec<u8>> {
        let Some(name) = id.dwo_name() else {
            return Ok(Vec::new());
        };
        dwo_file.section_by_name(name).map_or_else(
            || Ok(Vec::new()),
            |section| Ok(section.uncompressed_data()?.into_owned()),
        )
    })
    .unwrap();
    let mut dwo_dwarf = dwo_sections.borrow(|section| gimli::EndianSlice::new(section, endian));
    dwo_dwarf.make_dwo(&image_dwarf);
    let split_header = dwo_dwarf.units().next().unwrap().unwrap();
    let split_unit = dwo_dwarf.unit(split_header).unwrap();
    let mut entries = split_unit.entries();
    let mut indexed_addresses = 0;
    while let Some(entry) = entries.next_dfs().unwrap() {
        if entry.tag() == gimli::constants::DW_TAG_subprogram
            && matches!(
                entry.attr_value(gimli::constants::DW_AT_low_pc),
                Some(gimli::AttributeValue::DebugAddrIndex(_))
            )
        {
            indexed_addresses += 1;
        }
    }
    assert_eq!(indexed_addresses, SPLIT_FUNCTIONS.len());
}

fn dwarf_only_options() -> ConversionOptions {
    ConversionOptions {
        include_symbols: false,
        debuginfod_urls: Vec::new(),
        ..ConversionOptions::default()
    }
}

fn assert_function_addresses(
    report: &gsym::convert::ConversionReport,
    image: &std::path::Path,
    names: &[&[u8]],
) {
    let bytes = std::fs::read(image).unwrap();
    let file = object::File::parse(&*bytes).unwrap();
    for name in names {
        let symbol = file.symbol_by_name_bytes(name).unwrap();
        assert!(symbol.is_definition());
        let expected = symbol.address();
        let actual = find_function(report, name).map(|function| function.range.start);
        assert_eq!(actual, Some(expected), "{}", String::from_utf8_lossy(name));
    }
}

fn compile_split_dwarf_image(directory: &std::path::Path) -> std::path::PathBuf {
    let source = directory.join("split.c");
    let image = directory.join("split");
    std::fs::write(
        &source,
        "__attribute__((noinline)) int split_a(int x) { return x + 1; }\n__attribute__((noinline)) int split_b(int x) { return x + 2; }\n__attribute__((noinline)) int split_c(int x) { return x + 3; }\nint main(void) { return split_a(1) + split_b(2) + split_c(3); }\n",
    )
    .unwrap();
    run(Command::new("cc").current_dir(directory).args([
        "-g",
        "-gdwarf-5",
        "-O2",
        "-gsplit-dwarf",
        "-o",
        image.to_str().unwrap(),
        source.to_str().unwrap(),
    ]));
    assert!(!dwo_files(directory).is_empty());
    image
}

#[test]
fn disabled_discovery_ignores_individual_dwo_files() {
    let directory = tempfile::tempdir().unwrap();
    let image = compile_split_dwarf_image(directory.path());
    let options = ConversionOptions {
        include_symbols: false,
        discovery: DiscoveryPolicy::Disabled,
        ..ConversionOptions::default()
    };

    let report = ElfConverter::new(options).convert_path(&image).unwrap();
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| matches!(warning, ConversionWarning::SplitDwarfUnavailable { .. }))
    );
    assert_eq!(report.stats.dwarf_functions, 0, "{:?}", report.warnings);
    assert_eq!(report.stats.split_dwarf_units, 0);
    assert!(report.builder.functions().is_empty());
}

#[test]
fn resolves_relative_compilation_directories_from_the_current_directory() {
    let current_directory = std::env::current_dir().unwrap();
    let directory = tempfile::Builder::new()
        .prefix("relative-comp-dir-")
        .tempdir_in(&current_directory)
        .unwrap();
    let build_directory = directory.path().join("build");
    let binary_directory = build_directory.join("bin/nested");
    std::fs::create_dir_all(&binary_directory).unwrap();
    let source = build_directory.join("relative.c");
    let image = binary_directory.join("relative");
    std::fs::write(
        &source,
        "__attribute__((noinline)) int relative_target(int x) { return x + 1; }\nint main(void) { return relative_target(1); }\n",
    )
    .unwrap();
    let relative_build_directory = build_directory.strip_prefix(&current_directory).unwrap();
    run(Command::new("cc")
        .current_dir(&build_directory)
        .arg("-g")
        .arg("-O0")
        .arg("-gsplit-dwarf")
        .arg(format!(
            "-fdebug-prefix-map={}={}",
            build_directory.display(),
            relative_build_directory.display()
        ))
        .args(["-o", "bin/nested/relative", "relative.c"]));
    assert!(image.with_extension("dwo").is_file());
    let shadow = binary_directory
        .join(relative_build_directory)
        .join("bin/nested/relative.dwo");
    std::fs::create_dir_all(shadow.parent().unwrap()).unwrap();
    std::fs::write(shadow, b"not a DWO file").unwrap();

    let report = ElfConverter::new(ConversionOptions::default())
        .convert_path(&image)
        .unwrap();
    assert!(report.stats.dwarf_functions > 0, "{:?}", report.warnings);
    assert!(
        find_function(&report, b"relative_target").is_some(),
        "{:?}",
        report.warnings
    );
}

#[test]
fn resolves_relative_compilation_directories_from_the_binary_directory() {
    let directory = tempfile::tempdir().unwrap();
    let compilation_directory = directory.path().join("dwo");
    std::fs::create_dir(&compilation_directory).unwrap();
    let source = compilation_directory.join("relative.c");
    let compiled_image = compilation_directory.join("relative");
    let image = directory.path().join("relative");
    std::fs::write(
        &source,
        "__attribute__((noinline)) int binary_relative_target(int x) { return x + 1; }\nint main(void) { return binary_relative_target(1); }\n",
    )
    .unwrap();
    run(Command::new("cc")
        .current_dir(&compilation_directory)
        .arg("-g")
        .arg("-O0")
        .arg("-gsplit-dwarf")
        .arg(format!(
            "-fdebug-prefix-map={}=dwo",
            compilation_directory.display()
        ))
        .args(["-o", "relative", "relative.c"]));
    assert!(compiled_image.with_extension("dwo").is_file());
    std::fs::rename(&compiled_image, &image).unwrap();

    let report = ElfConverter::new(ConversionOptions::default())
        .convert_path(&image)
        .unwrap();
    assert!(report.stats.dwarf_functions > 0, "{:?}", report.warnings);
    assert!(find_function(&report, b"binary_relative_target").is_some());
}

#[test]
fn imports_gcc_split_line_rows_and_dwo_file_tables() {
    for dwarf_version in ["-gdwarf-4", "-gdwarf-5"] {
        check_gcc_split_line_rows_and_dwo_file_table(dwarf_version);
    }
}

fn check_gcc_split_line_rows_and_dwo_file_table(dwarf_version: &str) {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("implicit-line-table.c");
    let image = directory.path().join("implicit-line-table");
    std::fs::write(
        &source,
        "static inline __attribute__((always_inline)) int implicit_inline(int x) { volatile int value = x + 3; return value; }\n__attribute__((noinline)) int implicit_target(int x) { return implicit_inline(x); }\nint main(void) { return implicit_target(1); }\n",
    )
    .unwrap();
    run(Command::new(required_tool("gcc"))
        .current_dir(directory.path())
        .args([
            "-g",
            dwarf_version,
            "-O2",
            "-gsplit-dwarf",
            "-o",
            "implicit-line-table",
            "implicit-line-table.c",
        ]));
    let dwo_files = dwo_files(directory.path());
    let [dwo] = dwo_files.as_slice() else {
        panic!("expected exactly one DWO file");
    };
    let info = run(Command::new(required_tool("x86_64-linux-gnu-readelf"))
        .arg("--debug-dump=info")
        .arg(dwo));
    let info = String::from_utf8_lossy(&info.stdout);
    assert!(
        !info.contains("DW_AT_stmt_list"),
        "fixture unexpectedly has DW_AT_stmt_list:\n{info}"
    );

    let report = ElfConverter::new(ConversionOptions::default())
        .convert_path(&image)
        .unwrap();
    let target = report
        .builder
        .functions()
        .iter()
        .find(|function| function.name == b"implicit_target" && function.inline.is_some())
        .unwrap();
    let inline = target.inline.as_ref().unwrap();
    let [child] = inline.children.as_slice() else {
        panic!("expected exactly one inline child");
    };
    assert!(
        target.lines.iter().any(|line| line.line == 1),
        "{dwarf_version}: missing an executable line row from the inlined body: {:?}",
        target.lines
    );
    assert_ne!(child.call_file, gsym::FileIndex::ZERO);
    assert!(
        report.warnings.iter().all(|warning| !matches!(
            warning,
            ConversionWarning::MissingInlineCallFile { .. }
                | ConversionWarning::InvalidDeclarationFile { .. }
        )),
        "{dwarf_version}: {:?}",
        report.warnings
    );
}

fn dwo_files(directory: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut files = directory
        .read_dir()
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "dwo"))
        .collect::<Vec<_>>();
    files.sort();
    files
}

#[test]
fn discovers_dwp_packages_after_individual_dwo_files_are_unavailable() {
    let dwp_tool = required_tool("llvm-dwp");
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("package.c");
    let image = directory.path().join("package");
    std::fs::write(
        &source,
        "__attribute__((noinline)) int packaged(int x) { return x + 7; }\nint main(void) { return packaged(1); }\n",
    )
    .unwrap();
    run(Command::new("cc").current_dir(directory.path()).args([
        "-g",
        "-gdwarf-4",
        "-O0",
        "-gsplit-dwarf",
        "-o",
        image.to_str().unwrap(),
        source.to_str().unwrap(),
    ]));
    let dwo = dwo_files(directory.path()).remove(0);
    let dwp = image.with_extension("dwp");
    run_bounded(
        Command::new(dwp_tool).args([dwo.to_str().unwrap(), "-o", dwp.to_str().unwrap()]),
        Duration::from_secs(10),
    );
    std::fs::rename(&dwo, dwo.with_extension("dwo.unavailable")).unwrap();

    let report = ElfConverter::new(ConversionOptions::default())
        .convert_path(&image)
        .unwrap();
    assert_eq!(report.discovered_dwp.as_deref(), Some(dwp.as_path()));
    assert!(report.stats.dwarf_functions > 0, "{:?}", report.warnings);
    assert_eq!(report.stats.split_dwarf_units, 1);
    assert!(
        report
            .builder
            .functions()
            .iter()
            .any(|function| function.name == b"packaged" && !function.lines.is_empty()),
        "{:?}",
        report.warnings
    );

    let disabled = ElfConverter::new(ConversionOptions {
        include_symbols: false,
        discovery: DiscoveryPolicy::Disabled,
        ..ConversionOptions::default()
    })
    .convert_path(&image)
    .unwrap();
    assert!(disabled.discovered_dwp.is_none());
    assert!(
        disabled
            .warnings
            .iter()
            .any(|warning| matches!(warning, ConversionWarning::SplitDwarfUnavailable { .. }))
    );
    assert_eq!(disabled.stats.dwarf_functions, 0, "{:?}", disabled.warnings);

    let symbols_only = ElfConverter::new(ConversionOptions {
        dwarf: None,
        ..ConversionOptions::default()
    })
    .convert_path(&image)
    .unwrap();
    assert!(symbols_only.discovered_dwp.is_none());
    assert_eq!(symbols_only.stats.dwarf_functions, 0);
}

fn compile_split_units(
    directory: &std::path::Path,
    dwarf_version: &str,
) -> (std::path::PathBuf, Vec<std::path::PathBuf>) {
    let first = directory.join(format!("package-{dwarf_version}-first.c"));
    let second = directory.join(format!("package-{dwarf_version}-second.c"));
    let image = directory.join(format!("package-{dwarf_version}"));
    std::fs::write(
        &first,
        "static inline __attribute__((always_inline)) int packaged_inline(int x) { volatile int value = x + 3; return value; }\n__attribute__((noinline)) int packaged_first(int x) { return packaged_inline(x); }\n",
    )
    .unwrap();
    std::fs::write(
        &second,
        "int packaged_first(int);\n__attribute__((noinline)) int packaged_second(int x) { return packaged_first(x) + 5; }\nint main(void) { return packaged_second(1); }\n",
    )
    .unwrap();
    run(Command::new("cc").current_dir(directory).args([
        "-g",
        dwarf_version,
        "-O1",
        "-gsplit-dwarf",
        "-o",
        image.to_str().unwrap(),
        first.to_str().unwrap(),
        second.to_str().unwrap(),
    ]));
    let dwo_files = dwo_files(directory);
    assert_eq!(dwo_files.len(), 2);
    (image, dwo_files)
}

fn package_dwo_files(dwo_files: &[std::path::PathBuf], output: &std::path::Path) {
    let mut command = Command::new(required_tool("llvm-dwp"));
    command
        .args(dwo_files)
        .args(["-o", output.to_str().unwrap()]);
    run_bounded(&mut command, Duration::from_secs(15));
}

#[test]
fn imports_multi_unit_dwarf4_and_dwarf5_packages() {
    for dwarf_version in ["-gdwarf-4", "-gdwarf-5"] {
        let directory = tempfile::tempdir().unwrap();
        let (image, dwo_files) = compile_split_units(directory.path(), dwarf_version);
        let dwp = image.with_extension("dwp");
        package_dwo_files(&dwo_files, &dwp);
        for dwo in dwo_files {
            std::fs::rename(&dwo, dwo.with_extension("dwo.unavailable")).unwrap();
        }

        let report = ElfConverter::new(dwarf_only_options())
            .convert_path(&image)
            .unwrap();
        let first = report
            .builder
            .functions()
            .iter()
            .find(|function| function.name == b"packaged_first" && function.inline.is_some())
            .unwrap();
        let second = report
            .builder
            .functions()
            .iter()
            .find(|function| function.name == b"packaged_second" && !function.lines.is_empty())
            .unwrap();
        assert!(
            !first.lines.is_empty(),
            "{dwarf_version}: {:?}",
            report.warnings
        );
        assert!(!second.lines.is_empty());
        assert_function_addresses(
            &report,
            &image,
            &[b"packaged_first", b"packaged_second", b"main"],
        );
        let [inline] = first.inline.as_ref().unwrap().children.as_slice() else {
            panic!("expected one packaged inline child for {dwarf_version}");
        };
        assert_ne!(inline.call_file, gsym::FileIndex::ZERO);
        assert!(
            report.warnings.iter().all(|warning| !matches!(
                warning,
                ConversionWarning::MissingInlineCallFile { .. }
                    | ConversionWarning::InvalidDeclarationFile { .. }
            )),
            "{dwarf_version}: {:?}",
            report.warnings
        );
    }
}

#[test]
fn missing_dwp_unit_is_reported_without_aborting_symbol_conversion() {
    let directory = tempfile::tempdir().unwrap();
    let (image, dwo_files) = compile_split_units(directory.path(), "-gdwarf-5");
    let dwp = image.with_extension("dwp");
    package_dwo_files(&dwo_files[..1], &dwp);
    for dwo in dwo_files {
        std::fs::rename(&dwo, dwo.with_extension("dwo.unavailable")).unwrap();
    }

    let report = ElfConverter::new(ConversionOptions::default())
        .convert_path(&image)
        .unwrap();
    assert!(report.stats.symbol_functions > 0);
    assert!(report.warnings.iter().any(|warning| matches!(
        warning,
        ConversionWarning::SplitDwarfUnavailable { reasons, .. }
            if reasons.iter().any(|reason| reason.contains("absent from the DWP index"))
    )));
}

#[test]
fn malformed_dwp_indexes_fail_with_a_bounded_error() {
    let directory = tempfile::tempdir().unwrap();
    let (image, dwo_files) = compile_split_units(directory.path(), "-gdwarf-5");
    let dwp = image.with_extension("dwp");
    package_dwo_files(&dwo_files, &dwp);
    let index = directory.path().join("cu-index.bin");
    run(Command::new("objcopy")
        .arg(format!(
            "--dump-section=.debug_cu_index={}",
            index.display()
        ))
        .arg(&dwp));
    let mut index_bytes = std::fs::read(&index).unwrap();
    index_bytes.truncate(index_bytes.len() / 2);
    std::fs::write(&index, index_bytes).unwrap();
    let malformed = directory.path().join("malformed.dwp");
    run(Command::new("objcopy")
        .arg(format!(
            "--update-section=.debug_cu_index={}",
            index.display()
        ))
        .arg(&dwp)
        .arg(&malformed));

    let image_bytes = std::fs::read(image).unwrap();
    let dwp_bytes = std::fs::read(malformed).unwrap();
    let error = convert(ElfInputs::new(&image_bytes).with_dwp(&dwp_bytes))
        .expect_err("a truncated DWP index must be rejected");
    assert!(matches!(error, Error::Dwarf { .. }));
}

#[test]
fn dwp_index_cannot_pair_a_skeleton_with_the_wrong_split_unit() {
    let directory = tempfile::tempdir().unwrap();
    let (image, dwo_files) = compile_split_units(directory.path(), "-gdwarf-5");
    let dwp = image.with_extension("dwp");
    package_dwo_files(&dwo_files, &dwp);
    let index = directory.path().join("cu-index.bin");
    let dwp_bytes = std::fs::read(&dwp).unwrap();
    let dwp_file = object::File::parse(&*dwp_bytes).unwrap();
    let endian = dwp_file.endianness();
    let mut index_bytes = dwp_file
        .section_by_name(".debug_cu_index")
        .unwrap()
        .uncompressed_data()
        .unwrap()
        .into_owned();
    swap_first_two_dwp_index_rows(&mut index_bytes, endian);
    std::fs::write(&index, index_bytes).unwrap();
    let mismatched = directory.path().join("mismatched.dwp");
    run(Command::new("objcopy")
        .arg(format!(
            "--update-section=.debug_cu_index={}",
            index.display()
        ))
        .arg(&dwp)
        .arg(&mismatched));

    let image_bytes = std::fs::read(image).unwrap();
    let dwp_bytes = std::fs::read(mismatched).unwrap();
    let error = convert(ElfInputs::new(&image_bytes).with_dwp(&dwp_bytes))
        .expect_err("a DWP index must not redirect a skeleton to another split unit");
    assert!(matches!(
        error,
        Error::Malformed {
            context: "split DWARF unit",
            detail,
        } if detail.contains("ID mismatch")
    ));
}

fn swap_first_two_dwp_index_rows(index: &mut [u8], endian: object::Endianness) {
    let slot_count = endian.read_u32(index[12..16].try_into().unwrap()) as usize;
    let rows_start = 16 + slot_count * 8;
    let mut occupied = (0..slot_count).filter(|slot| {
        let start = 16 + slot * 8;
        endian.read_u64(index[start..start + 8].try_into().unwrap()) != 0
    });
    let first = occupied.next().expect("first occupied DWP index slot");
    let second = occupied.next().expect("second occupied DWP index slot");
    let first = rows_start + first * 4;
    let second = rows_start + second * 4;
    let first_row: [u8; 4] = index[first..first + 4].try_into().unwrap();
    let second_row: [u8; 4] = index[second..second + 4].try_into().unwrap();
    index[first..first + 4].copy_from_slice(&second_row);
    index[second..second + 4].copy_from_slice(&first_row);
}

#[test]
fn missing_split_dwarf_is_reported_without_aborting_symbol_conversion() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("missing-split.c");
    let image = directory.path().join("missing-split");
    std::fs::write(
        &source,
        "__attribute__((noinline)) int available_symbol(int x) { return x + 1; }\nint main(void) { return available_symbol(1); }\n",
    )
    .unwrap();
    run(Command::new("cc").current_dir(directory.path()).args([
        "-g",
        "-O1",
        "-gsplit-dwarf",
        "-o",
        image.to_str().unwrap(),
        source.to_str().unwrap(),
    ]));
    let dwo = dwo_files(directory.path()).remove(0);
    std::fs::rename(&dwo, dwo.with_extension("dwo.unavailable")).unwrap();

    let report = ElfConverter::new(ConversionOptions::default())
        .convert_path(&image)
        .unwrap();
    assert!(report.stats.symbol_functions > 0);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| matches!(warning, ConversionWarning::SplitDwarfUnavailable { .. }))
    );
}

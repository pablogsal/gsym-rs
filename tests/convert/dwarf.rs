//! DWARF import: line programs, inline trees, declarations and origins.

use std::process::Command;

use gsym::Gsym;
use gsym::convert::{
    ConversionOptions, ConversionWarning, DwarfImportOptions, ElfConverter, ElfInputs,
};

use crate::elf::{convert, find_function};
use crate::tools::{required_tool, run};

fn convert_yaml_with_text(
    source: &str,
    text_size: usize,
    options: ConversionOptions,
) -> gsym::convert::ConversionReport {
    let directory = tempfile::tempdir().unwrap();
    let yaml = directory.path().join("fixture.yaml");
    let object = directory.path().join("fixture");
    let text = directory.path().join("text.bin");
    let image = directory.path().join("fixture-with-text");
    std::fs::write(&yaml, source).unwrap();
    std::fs::write(&text, vec![0_u8; text_size]).unwrap();
    run(Command::new(required_tool("yaml2obj")).args([
        yaml.to_str().unwrap(),
        "-o",
        object.to_str().unwrap(),
    ]));
    run(
        Command::new(required_tool("x86_64-linux-gnu-objcopy")).args([
            "--add-section",
            &format!(".text={}", text.display()),
            "--set-section-flags",
            ".text=alloc,code,readonly",
            "--change-section-address",
            ".text=0x1000",
            object.to_str().unwrap(),
            image.to_str().unwrap(),
        ]),
    );
    ElfConverter::new(options).convert_path(&image).unwrap()
}

fn convert_statement_sequence_fixture(value: &str) -> gsym::convert::ConversionReport {
    let source = include_str!("../fixtures/statement_sequence.yaml").replacen(
        "- Value: 0x29",
        &format!("- Value: {value}"),
        1,
    );
    convert_yaml_with_text(&source, 0x100, ConversionOptions::default())
}

/// The rows a converted `selected_sequence` carries, as `(address, line)`.
fn statement_sequence_lines(report: &gsym::convert::ConversionReport) -> Vec<(u64, u32)> {
    find_function(report, b"selected_sequence")
        .expect("the fixture subprogram must convert")
        .lines
        .iter()
        .map(|line| (line.address, line.line))
        .collect()
}

#[test]
fn linker_tombstones_are_filtered_without_hiding_unexpected_ranges() {
    let options = ConversionOptions {
        include_symbols: false,
        dwarf: Some(DwarfImportOptions {
            inline_info: false,
            ..DwarfImportOptions::default()
        }),
        ..ConversionOptions::default()
    };

    let report = convert_yaml_with_text(
        include_str!("../fixtures/linker_tombstones.yaml"),
        0x100,
        options,
    );

    assert_eq!(report.stats.rejected_ranges, 4);
    assert_eq!(report.stats.dwarf_functions, 1);
    assert_eq!(
        report
            .builder
            .functions()
            .iter()
            .map(|function| function.name.as_slice())
            .collect::<Vec<_>>(),
        [b"live".as_slice()]
    );
    assert_eq!(
        report.warnings,
        [ConversionWarning::RejectedRange {
            range: gsym::AddressRange::new(0x3000, 0x3020),
        }]
    );
}

/// `DW_AT_LLVM_stmt_sequence` picks one of two sequences over the same code.
#[test]
fn honors_valid_statement_sequence_offsets_and_recovers_from_invalid_ones() {
    const FALLBACK: [(u64, u32); 4] = [(0x1000, 10), (0x1000, 200), (0x1010, 11), (0x1010, 201)];

    let first = convert_statement_sequence_fixture("0x29");
    assert_eq!(
        statement_sequence_lines(&first),
        [(0x1000, 10), (0x1010, 11)]
    );
    assert!(
        first
            .warnings
            .iter()
            .all(|warning| !matches!(warning, ConversionWarning::InvalidStatementSequence { .. }))
    );

    let second = convert_statement_sequence_fixture("0x44");
    assert_eq!(
        statement_sequence_lines(&second),
        [(0x1000, 200), (0x1010, 201)]
    );
    assert!(
        second
            .warnings
            .iter()
            .all(|warning| !matches!(warning, ConversionWarning::InvalidStatementSequence { .. }))
    );

    let invalid = convert_statement_sequence_fixture("0x2a");
    assert_eq!(statement_sequence_lines(&invalid), FALLBACK);
    assert!(invalid.warnings.iter().any(|warning| matches!(
        warning,
        ConversionWarning::InvalidStatementSequence {
            sequence_offset: 0x2a,
            ..
        }
    )));

    let sentinel = convert_statement_sequence_fixture("0xffffffff");
    assert_eq!(statement_sequence_lines(&sentinel), FALLBACK);
    assert!(
        sentinel
            .warnings
            .iter()
            .all(|warning| !matches!(warning, ConversionWarning::InvalidStatementSequence { .. }))
    );
}

#[test]
fn imports_direct_dwarf_call_sites_with_neutral_flags_and_target_names() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("calls.c");
    let image = directory.path().join("calls");
    std::fs::write(
        &source,
        "__attribute__((noinline)) int callee(int x) { return x + 1; }\n__attribute__((noinline)) int caller(int x) { return callee(x) + 1; }\nint main(void) { return caller(1); }\n",
    )
    .unwrap();
    run(Command::new("cc").args([
        "-g",
        "-gdwarf-5",
        "-O2",
        "-fno-optimize-sibling-calls",
        "-o",
        image.to_str().unwrap(),
        source.to_str().unwrap(),
    ]));
    let options = ConversionOptions {
        dwarf: Some(DwarfImportOptions {
            call_sites: true,
            ..DwarfImportOptions::default()
        }),
        ..ConversionOptions::default()
    };
    let report = ElfConverter::new(options).convert_path(&image).unwrap();
    let caller = report
        .builder
        .functions()
        .iter()
        .find(|function| function.name == b"caller" && !function.call_sites.is_empty())
        .expect("compiler-generated caller must contain a call-site DIE");
    assert!(caller.call_sites.iter().all(|site| site.flags.bits() == 0));
    assert!(caller.call_sites.iter().any(|site| {
        site.match_regex
            .iter()
            .any(|pattern| pattern.as_slice() == b"callee")
    }));
}

#[test]
fn recursive_type_graphs_do_not_affect_function_conversion() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("recursive.c");
    let image = directory.path().join("recursive");
    std::fs::write(
        &source,
        "typedef struct Node Node; struct Node { Node *next; int value; };\n__attribute__((noinline)) int recursive_type(Node *node) { return node ? node->value : 0; }\nint main(void) { return recursive_type(0); }\n",
    )
    .unwrap();
    run(Command::new("cc").args([
        "-g",
        "-O1",
        "-o",
        image.to_str().unwrap(),
        source.to_str().unwrap(),
    ]));
    let report = ElfConverter::new(ConversionOptions::default())
        .convert_path(&image)
        .unwrap();
    assert!(find_function(&report, b"recursive_type").is_some());
}

#[test]
fn falls_back_to_declaration_location_for_dwarf4_and_dwarf5() {
    for dwarf_version in ["-gdwarf-4", "-gdwarf-5"] {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("declaration.c");
        let image = directory.path().join("declaration");
        std::fs::write(
            &source,
            "volatile int declaration_sink;\n__attribute__((noinline)) int decl_fallback(int x) { declaration_sink = x; return x + 1; }\nint main(void) { return decl_fallback(1); }\n",
        )
        .unwrap();
        run(Command::new("cc").args([
            "-g",
            dwarf_version,
            "-O0",
            "-o",
            image.to_str().unwrap(),
            source.to_str().unwrap(),
        ]));
        let patched = replace_line_program_with_empty(directory.path(), &image);
        let options = ConversionOptions {
            include_symbols: false,
            ..ConversionOptions::default()
        };

        let report = ElfConverter::new(options).convert_path(&patched).unwrap();
        let function =
            find_function(&report, b"decl_fallback").expect("function must be imported from DWARF");
        assert_eq!(function.lines.len(), 1, "{dwarf_version}");
        assert_eq!(function.lines[0].address, function.range.start);
        assert_eq!(function.lines[0].line, 2);
        assert_ne!(function.lines[0].file.get(), 0);
    }
}

fn replace_line_program_with_empty(
    directory: &std::path::Path,
    image: &std::path::Path,
) -> std::path::PathBuf {
    let original_line = directory.join("debug-line.bin");
    run(Command::new("objcopy")
        .arg(format!(
            "--dump-section=.debug_line={}",
            original_line.display()
        ))
        .arg(image));
    let line = std::fs::read(&original_line).unwrap();
    let version = u16::from_le_bytes(line[4..6].try_into().unwrap());
    let header_length_offset = if version >= 5 { 8 } else { 6 };
    let header_length = usize::try_from(u32::from_le_bytes(
        line[header_length_offset..header_length_offset + 4]
            .try_into()
            .unwrap(),
    ))
    .unwrap();
    let program_offset = header_length_offset + 4 + header_length;
    let mut without_rows = line[..program_offset].to_vec();
    without_rows.extend_from_slice(&[0, 1, 1]);
    let unit_length = u32::try_from(without_rows.len() - 4).unwrap();
    without_rows[..4].copy_from_slice(&unit_length.to_le_bytes());
    let replacement = directory.join("debug-line-empty.bin");
    std::fs::write(&replacement, without_rows).unwrap();
    let patched = directory.join("declaration-no-rows");
    run(Command::new("objcopy")
        .arg(format!(
            "--update-section=.debug_line={}",
            replacement.display()
        ))
        .arg(image)
        .arg(&patched));
    patched
}

#[test]
fn follows_abstract_origins_for_declaration_fallback() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("origin-declaration.c");
    let image = directory.path().join("origin-declaration");
    std::fs::write(
        &source,
        "volatile int origin_sink;\n__attribute__((noinline)) int origin_fallback(int x) { origin_sink = x; return x + 1; }\nint main(void) { return origin_fallback(1); }\n",
    )
    .unwrap();
    run(Command::new("cc").args([
        "-g",
        "-gdwarf-5",
        "-O2",
        "-fno-ipa-cp",
        "-o",
        image.to_str().unwrap(),
        source.to_str().unwrap(),
    ]));
    let patched = replace_line_program_with_empty(directory.path(), &image);
    let options = ConversionOptions {
        include_symbols: false,
        ..ConversionOptions::default()
    };

    let report = ElfConverter::new(options).convert_path(&patched).unwrap();
    let function = find_function(&report, b"origin_fallback")
        .expect("referenced declaration must supply the fallback location");
    assert_eq!(function.lines.len(), 1);
    assert_eq!(function.lines[0].address, function.range.start);
    assert_eq!(function.lines[0].line, 2);
}

#[test]
fn invalid_declaration_file_indexes_warn_and_do_not_drop_functions() {
    let options = ConversionOptions {
        include_symbols: false,
        ..ConversionOptions::default()
    };

    let report = convert_yaml_with_text(
        include_str!("../fixtures/declaration_files.yaml"),
        0x2100,
        options,
    );
    let valid = find_function(&report, b"valid_decl").unwrap();
    assert_eq!(valid.lines.len(), 1);
    assert_eq!(valid.lines[0].line, 20);
    let invalid = find_function(&report, b"invalid_decl")
        .expect("an invalid declaration file must not discard the function");
    assert!(invalid.lines.is_empty());
    assert!(report.warnings.iter().any(|warning| matches!(
        warning,
        ConversionWarning::InvalidDeclarationFile { index: 10, .. }
    )));
}

#[test]
fn resolves_cross_compilation_unit_abstract_origins() {
    let directory = tempfile::tempdir().unwrap();
    let first = directory.path().join("first.c");
    let second = directory.path().join("second.c");
    let image = directory.path().join("cross-unit");
    std::fs::write(
        &first,
        "inline int shared(int x) { return x + 1; } int call_shared(int x) { return shared(x); }\n",
    )
    .unwrap();
    std::fs::write(
        &second,
        "int call_shared(int); int main(void) { return call_shared(1); }\n",
    )
    .unwrap();
    run(Command::new("cc").args([
        "-O2",
        "-g",
        "-flto",
        first.to_str().unwrap(),
        second.to_str().unwrap(),
        "-o",
        image.to_str().unwrap(),
    ]));
    let options = ConversionOptions {
        include_symbols: false,
        ..ConversionOptions::default()
    };
    let report = ElfConverter::new(options).convert_path(&image).unwrap();
    assert!(find_function(&report, b"main").is_some());
}

#[test]
fn elf_dwarf_and_gsym_only_equivalent_preserves_lines_and_inlines() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("inline.c");
    let image = directory.path().join("inline");
    let stripped = directory.path().join("inline.stripped");
    let debug = directory.path().join("inline.debug");
    std::fs::write(
        &source,
        br"
static inline __attribute__((always_inline)) int inctwo(int *value) {
    return (*value)++;
}
static inline __attribute__((always_inline)) int inc(int *value) {
    return inctwo(value) + (*value)++;
}
__attribute__((noinline)) int target(int value) {
    return inc(&value);
}
int main(void) {
    return target(1);
}
",
    )
    .unwrap();
    run(Command::new("cc").args([
        "-g",
        "-O2",
        "-Wl,--build-id",
        "-o",
        image.to_str().unwrap(),
        source.to_str().unwrap(),
    ]));
    run(Command::new("objcopy").args([
        "--only-keep-debug",
        image.to_str().unwrap(),
        debug.to_str().unwrap(),
    ]));
    run(Command::new("objcopy").args([
        "--strip-debug",
        image.to_str().unwrap(),
        stripped.to_str().unwrap(),
    ]));

    let image_bytes = std::fs::read(&stripped).unwrap();
    let debug_bytes = std::fs::read(&debug).unwrap();
    let report = convert(
        ElfInputs::new(&image_bytes)
            .with_debug(&debug_bytes)
            .with_symbols(&image_bytes),
    )
    .unwrap();
    assert!(report.stats.dwarf_functions > 0);
    assert!(report.stats.line_rows > 0);
    assert!(
        report.stats.inline_nodes >= 1,
        "conversion stats: {:?}",
        report.stats
    );
    let target = report
        .builder
        .functions()
        .iter()
        .find(|function| function.name == b"target" && function.inline.is_some())
        .unwrap();
    let inline_address = target.inline.as_ref().unwrap().children[0].ranges[0].start;
    let gsym_bytes = report.builder.to_bytes().unwrap();
    let gsym = Gsym::parse(&gsym_bytes).unwrap();
    let lookup = gsym.lookup(inline_address).unwrap().unwrap();
    assert_eq!(lookup.frames().last().unwrap().name, b"target");
    assert!(lookup.frames().len() >= 2);
    assert!(!lookup.frames()[0].basename.is_empty());
}

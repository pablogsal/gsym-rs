//! Split DWARF: individual `.dwo` files and the `.dwp` packages that bundle them.

use std::process::Command;
use std::time::{Duration, Instant};

use gsym::Error;
use gsym::convert::{ConversionOptions, ConversionWarning, ElfConverter, ElfInputs};

use crate::elf::{convert, find_function};
use crate::tools::{required_tool, run};

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
fn follows_split_dwarf_units_and_imports_dwo_functions() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("split.c");
    let image = directory.path().join("split");
    std::fs::write(
        &source,
        "static inline __attribute__((always_inline)) int split_inline(int x) { return x + 3; }\n__attribute__((noinline)) int split_target(int x) { return split_inline(x); }\nint main(void) { return split_target(1); }\n",
    )
    .unwrap();
    run(Command::new("cc").current_dir(directory.path()).args([
        "-g",
        "-O2",
        "-gsplit-dwarf",
        "-o",
        image.to_str().unwrap(),
        source.to_str().unwrap(),
    ]));
    assert!(!dwo_files(directory.path()).is_empty());

    let report = ElfConverter::new(ConversionOptions::default())
        .convert_path(&image)
        .unwrap();
    assert!(report.stats.dwarf_functions > 0, "{:?}", report.warnings);
    assert!(find_function(&report, b"split_target").is_some());
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
    assert!(
        report
            .builder
            .functions()
            .iter()
            .any(|function| function.name == b"packaged" && !function.lines.is_empty()),
        "{:?}",
        report.warnings
    );
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

        let report = ElfConverter::new(ConversionOptions::default())
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

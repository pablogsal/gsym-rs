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
    assert!(find_function(&report, b"packaged").is_some());
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
        "__attribute__((noinline)) int packaged_first(int x) { return x + 3; }\n",
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
        for name in [b"packaged_first".as_slice(), b"packaged_second".as_slice()] {
            assert!(
                report
                    .builder
                    .functions()
                    .iter()
                    .any(|function| function.name == name),
                "missing {} for {dwarf_version}: {:?}",
                String::from_utf8_lossy(name),
                report.warnings
            );
        }
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
